//! Model racing: concurrent requests to multiple models with first-response-wins semantics.
//!
//! Supports three racing strategies:
//! 1. Full parallel racing - all models race from start
//! 2. Smart racing - primary model with fallback racing on timeout
//! 3. TTFT racing - race to first token, then cancel others

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{stream, Stream, StreamExt};
use metrics::{counter, histogram};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;



use crate::AppState;

/// Racing strategy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RaceStrategy {
    /// Race all models concurrently from the start
    FullParallel {
        models: Vec<String>,
    },
    /// Try primary first, race fallbacks on timeout
    Smart {
        primary_model: String,
        fallback_models: Vec<String>,
        primary_timeout_ms: u64,
    },
    /// Race to first token only (streaming requests)
    TtftRacing {
        models: Vec<String>,
    },
    /// No racing, single model request
    None,
}

impl Default for RaceStrategy {
    fn default() -> Self {
        Self::None
    }
}

/// Result of a model race
pub struct RaceResult {
    /// The winning model
    pub winner: String,
    /// Response from the winning model
    pub response: RaceResponse,
    /// Time to first token (for streaming) or total time (for buffered)
    pub ttft: Duration,
    /// Number of models that participated
    pub participants: usize,
    /// Number of cancelled requests
    pub cancelled: usize,
}

/// Response from a race participant
pub enum RaceResponse {
    /// Buffered response
    Buffered(Bytes),
    /// Streaming response
    Stream(Box<dyn Stream<Item = Result<Bytes, String>> + Send + Unpin>),
}

/// Race multiple models with full parallel strategy
pub async fn race_full_parallel(
    state: Arc<AppState>,
    models: Vec<String>,
    request_body: Bytes,
    headers: axum::http::HeaderMap,
) -> Result<RaceResult, RaceError> {
    let started = Instant::now();

    if models.is_empty() {
        return Err(RaceError::NoModels);
    }

    counter!("nimproxy_race_requests_total", "strategy" => "full_parallel").increment(1);

    // Spawn concurrent requests for all models
    let mut tasks = Vec::new();

    for model in &models {
        let state = state.clone();
        let model = model.clone();
        let body = request_body.clone();
        let headers = headers.clone();

        let task = tokio::spawn(async move {
            let result = send_single_model_request(state, model.clone(), body, headers).await;
            (model, result, Instant::now())
        });

        tasks.push(task);
    }

    // Wait for first successful response
    let mut winner = None;
    let mut errors = Vec::new();

    for task in tasks {
        match task.await {
            Ok((model, Ok(response), timestamp)) => {
                if winner.is_none() {
                    let ttft = timestamp.duration_since(started);
                    winner = Some((model.clone(), response, ttft));

                    counter!("nimproxy_race_model_wins_total", "model" => model.clone()).increment(1);
                    histogram!("nimproxy_race_ttft_seconds", "model" => model.clone()).record(ttft.as_secs_f64());

                    // Cancel remaining tasks (they will be dropped)
                    break;
                }
            }
            Ok((model, Err(e), _)) => {
                errors.push((model, e));
            }
            Err(e) => {
                errors.push(("unknown".to_string(), format!("Task error: {}", e)));
            }
        }
    }

    match winner {
        Some((model, response, ttft)) => {
            let participants = models.len();
            let cancelled = participants - 1;

            counter!("nimproxy_race_cancelled_total", "reason" => "winner_found").increment(cancelled as u64);

            Ok(RaceResult {
                winner: model,
                response,
                ttft,
                participants,
                cancelled,
            })
        }
        None => {
            Err(RaceError::AllFailed { errors })
        }
    }
}

/// Race with smart strategy: try primary first, fallback on timeout
pub async fn race_smart(
    state: Arc<AppState>,
    primary_model: String,
    fallback_models: Vec<String>,
    primary_timeout_ms: u64,
    request_body: Bytes,
    headers: axum::http::HeaderMap,
) -> Result<RaceResult, RaceError> {
    let started = Instant::now();

    counter!("nimproxy_race_requests_total", "strategy" => "smart").increment(1);

    // Try primary model first with timeout
    let primary_future = send_single_model_request(
        state.clone(),
        primary_model.clone(),
        request_body.clone(),
        headers.clone(),
    );

    match timeout(Duration::from_millis(primary_timeout_ms), primary_future).await {
        Ok(Ok(response)) => {
            // Primary succeeded within timeout
            let ttft = Instant::now().duration_since(started);

            counter!("nimproxy_race_model_wins_total", "model" => primary_model.clone()).increment(1);
            counter!("nimproxy_race_smart_primary_hit_total").increment(1);

            Ok(RaceResult {
                winner: primary_model,
                response,
                ttft,
                participants: 1,
                cancelled: 0,
            })
        }
        _ => {
            // Primary timed out or failed, race fallbacks
            counter!("nimproxy_race_smart_fallback_triggered_total").increment(1);

            race_full_parallel(state, fallback_models, request_body, headers).await
        }
    }
}

/// Race to first token for streaming requests
pub async fn race_ttft(
    state: Arc<AppState>,
    models: Vec<String>,
    request_body: Bytes,
    headers: axum::http::HeaderMap,
) -> Result<RaceResult, RaceError> {
    let started = Instant::now();

    if models.is_empty() {
        return Err(RaceError::NoModels);
    }

    counter!("nimproxy_race_requests_total", "strategy" => "ttft").increment(1);

    // Spawn streaming requests for all models
    let mut streams = Vec::new();

    for model in &models {
        let state = state.clone();
        let model = model.clone();
        let body = request_body.clone();
        let headers = headers.clone();

        match send_streaming_request(state, model.clone(), body, headers).await {
            Ok(stream) => {
                streams.push((model, stream));
            }
            Err(e) => {
                tracing::warn!("Failed to start stream for model {}: {}", model, e);
            }
        }
    }

    if streams.is_empty() {
        return Err(RaceError::AllFailed { errors: vec![] });
    }

    // Race to first token
    let (winner_model, winner_stream) = race_streams_to_first_token(streams).await?;

    let ttft = Instant::now().duration_since(started);

    counter!("nimproxy_race_model_wins_total", "model" => winner_model.clone()).increment(1);
    histogram!("nimproxy_race_ttft_seconds", "model" => winner_model.clone()).record(ttft.as_secs_f64());

    let participants = models.len();
    let cancelled = participants - 1;
    counter!("nimproxy_race_cancelled_total", "reason" => "ttft_winner").increment(cancelled as u64);

    Ok(RaceResult {
        winner: winner_model,
        response: RaceResponse::Stream(Box::new(winner_stream)),
        ttft,
        participants,
        cancelled,
    })
}

/// Race multiple streams to first token
async fn race_streams_to_first_token(
    mut streams: Vec<(String, impl Stream<Item = Result<Bytes, String>> + Unpin)>,
) -> Result<(String, impl Stream<Item = Result<Bytes, String>> + Unpin), RaceError> {
    use futures_util::future::select_all;

    // Create futures that wait for first chunk from each stream
    let futures: Vec<_> = streams
        .iter_mut()
        .enumerate()
        .map(|(idx, (_, stream))| {
            let fut = async move {
                match stream.next().await {
                    Some(Ok(chunk)) => Ok((idx, chunk)),
                    Some(Err(e)) => Err(e),
                    None => Err("Stream ended without data".to_string()),
                }
            };
            Box::pin(fut)
        })
        .collect();

    // Wait for first successful chunk
    let (result, _, remaining) = select_all(futures).await;
    drop(remaining);
    let (winner_idx, first_chunk) = result.map_err(|error| RaceError::AllFailed { errors: vec![("unknown".into(), error)] })?;

    // Extract winner and prepend first chunk to its stream
    let (winner_model, winner_stream) = streams.swap_remove(winner_idx);

    let prepended_stream = stream::once(async move { Ok(first_chunk) })
        .chain(winner_stream);

    Ok((winner_model, Box::pin(prepended_stream)))
}

/// Send a single model request (placeholder - will integrate with existing proxy code)
async fn send_single_model_request(
    _state: Arc<AppState>,
    _model: String,
    _body: Bytes,
    _headers: axum::http::HeaderMap,
) -> Result<RaceResponse, String> {
    // TODO: Integrate with existing proxy::relay logic
    // This will call the actual NIM API through the existing dispatch/pool
    Err("Not yet implemented".to_string())
}

/// Send a streaming request (placeholder)
async fn send_streaming_request(
    _state: Arc<AppState>,
    _model: String,
    _body: Bytes,
    _headers: axum::http::HeaderMap,
) -> Result<futures_util::stream::Empty<Result<Bytes, String>>, String> {
    // TODO: Integrate with existing proxy::streaming logic
    Err("Not yet implemented".to_string())
}

#[derive(Debug)]
pub enum RaceError {
    NoModels,
    AllFailed { errors: Vec<(String, String)> },
    Timeout,
}

impl std::fmt::Display for RaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoModels => write!(f, "No models specified for racing"),
            Self::AllFailed { errors } => {
                write!(f, "All models failed: ")?;
                for (model, error) in errors {
                    write!(f, "{}: {}; ", model, error)?;
                }
                Ok(())
            }
            Self::Timeout => write!(f, "Race timeout"),
        }
    }
}

impl std::error::Error for RaceError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_race_strategy_serialization() {
        let strategy = RaceStrategy::Smart {
            primary_model: "llama-3.1-70b".to_string(),
            fallback_models: vec!["mistral-7b".to_string()],
            primary_timeout_ms: 2000,
        };

        let json = serde_json::to_string(&strategy).unwrap();
        assert!(json.contains("smart"));
        assert!(json.contains("llama-3.1-70b"));
    }
}
