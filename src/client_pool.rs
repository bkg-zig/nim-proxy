//! Connection pool and HTTP client optimizations for speed
//!
//! Optimizations:
//! - Reuse HTTP connections with keep-alive
//! - Connection pooling per host
//! - TCP optimization (nodelay, keepalive)
//! - DNS caching

use std::sync::Arc;
use std::time::Duration;

use reqwest::{Client, ClientBuilder};

/// Build an optimized HTTP client for NIM API requests
pub fn build_optimized_client() -> Result<Client, reqwest::Error> {
    ClientBuilder::new()
        // Connection pooling - reuse connections
        .pool_max_idle_per_host(10)
        .pool_idle_timeout(Duration::from_secs(90))

        // TCP optimizations
        .tcp_keepalive(Duration::from_secs(60))
        .tcp_nodelay(true)

        // Timeouts
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))

        // HTTP/2 for better multiplexing
        .http2_prior_knowledge()

        // Enable compression
        .gzip(true)
        .brotli(true)

        .build()
}

/// Shared client pool for all requests
pub struct ClientPool {
    client: Arc<Client>,
}

impl ClientPool {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: Arc::new(build_optimized_client()?),
        })
    }

    pub fn get(&self) -> Arc<Client> {
        self.client.clone()
    }
}

impl Default for ClientPool {
    fn default() -> Self {
        Self::new().expect("Failed to create HTTP client")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let pool = ClientPool::new();
        assert!(pool.is_ok());
    }

    #[test]
    fn test_client_reuse() {
        let pool = ClientPool::new().unwrap();
        let client1 = pool.get();
        let client2 = pool.get();

        // Should return the same underlying client (Arc clones point to same data)
        assert!(Arc::ptr_eq(&client1, &client2));
    }
}
