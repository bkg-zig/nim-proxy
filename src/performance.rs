//! Performance tracking for model racing and optimization
//!
//! Tracks historical performance metrics to inform racing decisions

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Performance statistics for a model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStats {
    /// Average time to first token (ms)
    pub avg_ttft_ms: f64,
    /// Average tokens per second
    pub avg_tokens_per_sec: f64,
    /// Success rate (0.0 - 1.0)
    pub success_rate: f64,
    /// Total requests tracked
    pub total_requests: u64,
    /// Total successful requests
    pub successful_requests: u64,
    /// Last updated timestamp
    pub last_updated: std::time::Instant,
}

impl ModelStats {
    fn new() -> Self {
        Self {
            avg_ttft_ms: 0.0,
            avg_tokens_per_sec: 0.0,
            success_rate: 1.0,
            total_requests: 0,
            successful_requests: 0,
            last_updated: std::time::Instant::now(),
        }
    }

    /// Update stats with a new observation
    fn update(&mut self, observation: &PerformanceObservation) {
        self.total_requests += 1;

        if observation.success {
            self.successful_requests += 1;

            // Exponential moving average for TTFT
            let alpha = 0.3; // Weight for new observations
            if let Some(ttft) = observation.ttft {
                let new_ttft = ttft.as_millis() as f64;
                self.avg_ttft_ms = if self.avg_ttft_ms == 0.0 {
                    new_ttft
                } else {
                    alpha * new_ttft + (1.0 - alpha) * self.avg_ttft_ms
                };
            }

            // Update tokens per second
            if let Some(tps) = observation.tokens_per_sec {
                self.avg_tokens_per_sec = if self.avg_tokens_per_sec == 0.0 {
                    tps
                } else {
                    alpha * tps + (1.0 - alpha) * self.avg_tokens_per_sec
                };
            }
        }

        // Update success rate
        self.success_rate = self.successful_requests as f64 / self.total_requests as f64;
        self.last_updated = std::time::Instant::now();
    }
}

/// Single performance observation
#[derive(Debug)]
pub struct PerformanceObservation {
    pub model: String,
    pub success: bool,
    pub ttft: Option<Duration>,
    pub tokens_per_sec: Option<f64>,
    pub error: Option<String>,
}

/// Performance tracker for all models
pub struct PerformanceTracker {
    stats: Arc<RwLock<HashMap<String, ModelStats>>>,
}

impl PerformanceTracker {
    pub fn new() -> Self {
        Self {
            stats: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Record a performance observation
    pub fn record(&self, observation: PerformanceObservation) {
        let mut stats = self.stats.write().unwrap();
        let model_stats = stats
            .entry(observation.model.clone())
            .or_insert_with(ModelStats::new);
        model_stats.update(&observation);
    }

    /// Get stats for a specific model
    pub fn get_stats(&self, model: &str) -> Option<ModelStats> {
        let stats = self.stats.read().unwrap();
        stats.get(model).cloned()
    }

    /// Get all stats
    pub fn get_all_stats(&self) -> HashMap<String, ModelStats> {
        self.stats.read().unwrap().clone()
    }

    /// Get the fastest model (lowest TTFT)
    pub fn get_fastest_model(&self) -> Option<String> {
        let stats = self.stats.read().unwrap();

        stats
            .iter()
            .filter(|(_, s)| s.successful_requests >= 5) // Minimum sample size
            .min_by(|(_, a), (_, b)| {
                a.avg_ttft_ms
                    .partial_cmp(&b.avg_ttft_ms)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(model, _)| model.clone())
    }

    /// Get models sorted by performance (fastest first)
    pub fn get_ranked_models(&self) -> Vec<(String, ModelStats)> {
        let stats = self.stats.read().unwrap();

        let mut models: Vec<_> = stats
            .iter()
            .filter(|(_, s)| s.successful_requests >= 3)
            .map(|(model, stats)| (model.clone(), stats.clone()))
            .collect();

        // Sort by composite score: TTFT (50%) + success rate (30%) + TPS (20%)
        models.sort_by(|(_, a), (_, b)| {
            let score_a = a.avg_ttft_ms * 0.5
                + (1.0 - a.success_rate) * 10000.0 * 0.3
                + (1.0 / a.avg_tokens_per_sec.max(1.0)) * 0.2;
            let score_b = b.avg_ttft_ms * 0.5
                + (1.0 - b.success_rate) * 10000.0 * 0.3
                + (1.0 / b.avg_tokens_per_sec.max(1.0)) * 0.2;
            score_a
                .partial_cmp(&score_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        models
    }
}

impl Default for PerformanceTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_performance_tracking() {
        let tracker = PerformanceTracker::new();

        // Record some observations
        tracker.record(PerformanceObservation {
            model: "llama-3.1-70b".to_string(),
            success: true,
            ttft: Some(Duration::from_millis(1500)),
            tokens_per_sec: Some(25.0),
            error: None,
        });

        tracker.record(PerformanceObservation {
            model: "mistral-7b".to_string(),
            success: true,
            ttft: Some(Duration::from_millis(800)),
            tokens_per_sec: Some(40.0),
            error: None,
        });

        // Check stats
        let stats = tracker.get_stats("llama-3.1-70b");
        assert!(stats.is_some());
        let stats = stats.unwrap();
        assert_eq!(stats.total_requests, 1);
        assert_eq!(stats.avg_ttft_ms, 1500.0);
    }

    #[test]
    fn test_fastest_model() {
        let tracker = PerformanceTracker::new();

        // Need at least 5 observations for each
        for _ in 0..5 {
            tracker.record(PerformanceObservation {
                model: "slow-model".to_string(),
                success: true,
                ttft: Some(Duration::from_millis(2000)),
                tokens_per_sec: Some(20.0),
                error: None,
            });

            tracker.record(PerformanceObservation {
                model: "fast-model".to_string(),
                success: true,
                ttft: Some(Duration::from_millis(800)),
                tokens_per_sec: Some(40.0),
                error: None,
            });
        }

        let fastest = tracker.get_fastest_model();
        assert_eq!(fastest, Some("fast-model".to_string()));
    }
}
