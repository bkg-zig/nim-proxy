//! Selection policy only. The proxy owns admission, retries and response I/O.
//! Dropping a race drops pending attempts instead of detaching upstream tasks.
pub use crate::racing_config::RaceStrategy;
use futures_util::{stream::FuturesUnordered, StreamExt};
use metrics::counter;
use std::future::Future;
use std::time::Duration;

pub(crate) struct Winner<T> {
    pub model: String,
    pub value: T,
}

pub(crate) async fn select<T, F, Fut>(
    strategy: RaceStrategy,
    mut attempt: F,
) -> Result<Winner<T>, ()>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<T, ()>>,
{
    let kind = match &strategy {
        RaceStrategy::FullParallel { .. } => "full_parallel",
        RaceStrategy::Smart { .. } => "smart",
        RaceStrategy::TtftRacing { .. } => "ttft",
        RaceStrategy::None => return Err(()),
    };
    counter!("nimproxy_race_requests_total", "strategy" => kind).increment(1);
    match strategy {
        RaceStrategy::FullParallel { models } => {
            parallel(models, &mut attempt, "winner_found").await
        }
        RaceStrategy::TtftRacing { models } => parallel(models, &mut attempt, "ttft_winner").await,
        RaceStrategy::Smart {
            primary_model,
            fallback_models,
            primary_timeout_ms,
        } => {
            // Timeout drops the primary before fallback admission.
            match tokio::time::timeout(
                Duration::from_millis(primary_timeout_ms),
                attempt(primary_model.clone()),
            )
            .await
            {
                Ok(Ok(value)) => {
                    counter!("nimproxy_race_smart_primary_hit_total").increment(1);
                    return Ok(Winner {
                        model: primary_model,
                        value,
                    });
                }
                Err(_) => {
                    counter!("nimproxy_race_cancelled_total", "reason" => "primary_timeout")
                        .increment(1);
                }
                Ok(Err(())) => {}
            }
            counter!("nimproxy_race_smart_fallback_triggered_total").increment(1);
            parallel(fallback_models, &mut attempt, "winner_found").await
        }
        RaceStrategy::None => Err(()),
    }
}

async fn parallel<T, F, Fut>(
    models: Vec<String>,
    attempt: &mut F,
    reason: &'static str,
) -> Result<Winner<T>, ()>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<T, ()>>,
{
    let mut pending = FuturesUnordered::new();
    for model in models {
        let future = attempt(model.clone());
        pending.push(async move { (model, future.await) });
    }
    while let Some((model, result)) = pending.next().await {
        if let Ok(value) = result {
            counter!("nimproxy_race_cancelled_total", "reason" => reason)
                .increment(pending.len() as u64);
            return Ok(Winner { model, value });
        }
    }
    Err(())
}

/// Bounded complete-event classifier; the relay preserves the original bytes.
#[derive(Default)]
pub(crate) struct SseGate {
    line: Vec<u8>,
    data: Vec<u8>,
    after_cr: bool,
    prefix_bytes: usize,
    finished: bool,
    pub ready: bool,
    pub failed: bool,
    pub done: bool,
}
pub(crate) const PREFIX_LIMIT: usize = 64 * 1024;

impl SseGate {
    pub fn is_complete(&self) -> bool {
        !self.failed && (self.done || self.finished) && self.line.is_empty() && self.data.is_empty()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.failed || self.done {
                return;
            }
            if !self.ready {
                self.prefix_bytes += 1;
                if self.prefix_bytes > PREFIX_LIMIT {
                    self.failed = true;
                    return;
                }
            }
            // SSE accepts LF, CRLF and bare CR, even when CRLF is split
            // between reads. The byte limit ends at the winning event.
            if self.after_cr && byte == b'\n' {
                self.after_cr = false;
                continue;
            }
            self.after_cr = byte == b'\r';
            if matches!(byte, b'\r' | b'\n') {
                self.finish_line();
            } else if self.line.len() + self.data.len() >= PREFIX_LIMIT {
                self.failed = true;
                self.line.clear();
                self.data.clear();
            } else {
                self.line.push(byte);
            }
        }
    }

    fn finish_line(&mut self) {
        if self.line.is_empty() {
            if self.data == b"[DONE]" {
                self.done = true;
            } else if !self.data.is_empty() {
                match serde_json::from_slice::<serde_json::Value>(&self.data) {
                    Ok(value) if value.get("error").is_some() => self.failed = true,
                    Ok(value) => {
                        self.ready |= value.get("choices").and_then(|v| v.as_array()).is_some_and(
                            |choices| {
                                self.finished |= !choices.is_empty()
                                    && choices.iter().all(|choice| {
                                        choice
                                            .get("finish_reason")
                                            .and_then(|v| v.as_str())
                                            .is_some_and(|reason| !reason.is_empty())
                                    });
                                choices
                                    .iter()
                                    .any(|choice| choice.get("delta").is_some_and(meaningful_delta))
                            },
                        );
                    }
                    Err(_) => self.failed = true,
                }
            }
            self.data.clear();
        } else if let Some(data) = self.line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            if !self.data.is_empty() {
                self.data.push(b'\n');
            }
            self.data.extend_from_slice(data);
        } else if let Some(event) = self.line.strip_prefix(b"event:") {
            self.failed |= event.strip_prefix(b" ").unwrap_or(event) == b"error";
        }
        self.line.clear();
    }
}

fn meaningful_delta(delta: &serde_json::Value) -> bool {
    let nonempty = |value: Option<&serde_json::Value>| {
        value
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    };
    nonempty(delta.get("content"))
        || nonempty(delta.get("reasoning_content"))
        || delta
            .get("tool_calls")
            .and_then(|v| v.as_array())
            .is_some_and(|calls| {
                calls.iter().any(|call| {
                    nonempty(call.get("id"))
                        || call.get("function").is_some_and(|function| {
                            nonempty(function.get("name")) || nonempty(function.get("arguments"))
                        })
                })
            })
        || delta.get("function_call").is_some_and(|function| {
            nonempty(function.get("name")) || nonempty(function.get("arguments"))
        })
}
