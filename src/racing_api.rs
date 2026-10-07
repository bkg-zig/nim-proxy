//! Racing configuration and management endpoints

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::racing::RaceStrategy;
use crate::performance::ModelStats;
use crate::AppState;

/// Request to update racing configuration
#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateRacingConfigRequest {
    pub enabled: bool,
    pub strategy: Option<RaceStrategy>,
}

/// Response with current racing configuration
#[derive(Debug, Serialize)]
pub struct RacingConfigResponse {
    pub enabled: bool,
    pub strategy: Option<RaceStrategy>,
}

/// Performance statistics response
#[derive(Debug, Serialize)]
pub struct PerformanceStatsResponse {
    pub models: Vec<ModelStatsEntry>,
}

#[derive(Debug, Serialize)]
pub struct ModelStatsEntry {
    pub name: String,
    pub avg_ttft_ms: f64,
    pub avg_tokens_per_sec: f64,
    pub success_rate: f64,
    pub total_requests: u64,
    pub successful_requests: u64,
}

/// GET /api/racing/config - Get current racing configuration
pub async fn get_racing_config(
    State(state): State<Arc<AppState>>,
) -> Response {
    let config = state.race_config.read().unwrap();

    let response = RacingConfigResponse {
        enabled: config.is_some(),
        strategy: config.clone(),
    };

    Json(response).into_response()
}

/// POST /api/racing/config - Update racing configuration
pub async fn update_racing_config(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateRacingConfigRequest>,
) -> Response {
    let mut config = state.race_config.write().unwrap();

    if req.enabled {
        *config = req.strategy;
    } else {
        *config = None;
    }

    drop(config);

    tracing::info!("Racing configuration updated: enabled={}", req.enabled);

    StatusCode::OK.into_response()
}

/// GET /api/racing/stats - Get performance statistics for all models
pub async fn get_performance_stats(
    State(state): State<Arc<AppState>>,
) -> Response {
    let all_stats = state.perf_tracker.get_all_stats();

    let models: Vec<ModelStatsEntry> = all_stats
        .into_iter()
        .map(|(name, stats)| ModelStatsEntry {
            name,
            avg_ttft_ms: stats.avg_ttft_ms,
            avg_tokens_per_sec: stats.avg_tokens_per_sec,
            success_rate: stats.success_rate,
            total_requests: stats.total_requests,
            successful_requests: stats.successful_requests,
        })
        .collect();

    Json(PerformanceStatsResponse { models }).into_response()
}

/// GET /api/racing/ranked - Get models ranked by performance
pub async fn get_ranked_models(
    State(state): State<Arc<AppState>>,
) -> Response {
    let ranked = state.perf_tracker.get_ranked_models();

    let models: Vec<ModelStatsEntry> = ranked
        .into_iter()
        .map(|(name, stats)| ModelStatsEntry {
            name,
            avg_ttft_ms: stats.avg_ttft_ms,
            avg_tokens_per_sec: stats.avg_tokens_per_sec,
            success_rate: stats.success_rate,
            total_requests: stats.total_requests,
            successful_requests: stats.successful_requests,
        })
        .collect();

    Json(PerformanceStatsResponse { models }).into_response()
}

/// Example configuration for deepseek-v4.1-flash racing
pub fn example_deepseek_racing_config() -> RaceStrategy {
    RaceStrategy::Smart {
        primary_model: "deepseek-v4.1-flash".to_string(),
        fallback_models: vec![
            "llama-3.1-8b-instruct".to_string(),
            "mistral-7b-instruct-v0.3".to_string(),
        ],
        primary_timeout_ms: 1500,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_example_deepseek_config() {
        let config = example_deepseek_racing_config();

        match config {
            RaceStrategy::Smart { primary_model, fallback_models, primary_timeout_ms } => {
                assert_eq!(primary_model, "deepseek-v4.1-flash");
                assert_eq!(fallback_models.len(), 2);
                assert_eq!(primary_timeout_ms, 1500);
            }
            _ => panic!("Expected Smart strategy"),
        }
    }
}
