//! The validated, opt-in racing policy shared by storage and request dispatch.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Every participant consumes a separately governed upstream request. Policies
/// are bounded to five distinct model ids and are disabled unless persisted.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RaceStrategy {
    /// Race all models concurrently from the start.
    FullParallel { models: Vec<String> },
    /// Start fallbacks when the primary fails or its delay expires.
    Smart {
        primary_model: String,
        fallback_models: Vec<String>,
        primary_timeout_ms: u64,
    },
    /// Race until a meaningful streamed delta; buffered requests race to completion.
    TtftRacing { models: Vec<String> },
    /// No racing. An enabled durable policy may not use this variant.
    #[default]
    None,
}

impl RaceStrategy {
    /// Validate an enabled policy at every settings write and at startup.
    pub fn validate(&self) -> Result<(), String> {
        let models: Vec<&str> = match self {
            Self::FullParallel { models } | Self::TtftRacing { models } => {
                models.iter().map(String::as_str).collect()
            }
            Self::Smart {
                primary_model,
                fallback_models,
                primary_timeout_ms,
            } => {
                if !(1..=300_000).contains(primary_timeout_ms) {
                    return Err("racing primary_timeout_ms must be between 1 and 300000".into());
                }
                std::iter::once(primary_model.as_str())
                    .chain(fallback_models.iter().map(String::as_str))
                    .collect()
            }
            Self::None => return Err("enabled racing requires a non-none strategy".into()),
        };
        if !(1..=5).contains(&models.len()) {
            return Err("racing requires between 1 and 5 models".into());
        }
        let mut seen = std::collections::HashSet::new();
        for model in models {
            if model.is_empty()
                || model.len() > 128
                || !model
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'/'))
            {
                return Err(
                    "racing model ids must be 1-128 ASCII letters, digits, '.', '_', '-' or '/'"
                        .into(),
                );
            }
            if !seen.insert(model) {
                return Err("racing model ids must be unique".into());
            }
        }
        Ok(())
    }
}
