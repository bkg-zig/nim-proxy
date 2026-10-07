//! Racing integration tests with deepseek-v4.1-flash

#[cfg(test)]
mod tests {
    use super::*;
    use crate::racing::RaceStrategy;

    const TEST_MODEL: &str = "deepseek-v4.1-flash";

    #[test]
    fn test_race_strategy_with_deepseek() {
        // Test smart racing with deepseek as primary
        let strategy = RaceStrategy::Smart {
            primary_model: TEST_MODEL.to_string(),
            fallback_models: vec![
                "llama-3.1-8b-instruct".to_string(),
                "mistral-7b-instruct-v0.3".to_string(),
            ],
            primary_timeout_ms: 1500, // Deepseek is usually fast
        };

        let json = serde_json::to_string(&strategy).unwrap();
        assert!(json.contains("deepseek-v4.1-flash"));
        assert!(json.contains("smart"));
    }

    #[test]
    fn test_full_parallel_with_deepseek() {
        // Test full parallel racing with deepseek
        let strategy = RaceStrategy::FullParallel {
            models: vec![
                TEST_MODEL.to_string(),
                "llama-3.1-8b-instruct".to_string(),
                "mistral-7b-instruct-v0.3".to_string(),
            ],
        };

        let json = serde_json::to_string(&strategy).unwrap();
        assert!(json.contains("deepseek"));
        assert!(json.contains("full_parallel"));
    }

    #[test]
    fn test_ttft_racing_with_deepseek() {
        // Test TTFT racing with deepseek
        let strategy = RaceStrategy::TtftRacing {
            models: vec![
                TEST_MODEL.to_string(),
                "llama-3.1-70b-instruct".to_string(),
            ],
        };

        let json = serde_json::to_string(&strategy).unwrap();
        assert!(json.contains("ttft_racing"));
    }
}
