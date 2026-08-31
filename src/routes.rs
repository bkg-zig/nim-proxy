//! Fixed route paths for the headless gateway surface.
//!
//! Three routes, no more: the health probe, the metrics exposition, and the
//! OpenAI-compatible API wildcard. There is no dashboard, no setup wizard,
//! and no asset surface — the CLI invocation is the entire operator story.

pub const METRICS: &str = "/metrics";
pub const HEALTH: &str = "/health";
pub const V1_WILDCARD: &str = "/v1/{*path}";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_are_exactly_the_headless_surface() {
        // The contract of this build: anything beyond these three paths is
        // served by nothing (and answers 404 through the router fallback).
        assert_eq!(HEALTH, "/health");
        assert_eq!(METRICS, "/metrics");
        assert_eq!(V1_WILDCARD, "/v1/{*path}");
    }
}
