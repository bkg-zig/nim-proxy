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

        // Bound connection setup; request workflows own generation deadlines.
        .connect_timeout(Duration::from_secs(10))

        // Negotiate HTTP normally and use the compression feature already enabled.
        .gzip(true)

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

    #[tokio::test]
    async fn client_accepts_http1_upstream() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let size = socket.read(&mut request).await.unwrap();
            assert!(request[..size].starts_with(b"GET / HTTP/1.1\r\n"));
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
        });

        let response = build_optimized_client()
            .unwrap()
            .get(format!("http://{address}/"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .unwrap();
        assert_eq!(response.version(), reqwest::Version::HTTP_11);
        assert_eq!(response.text().await.unwrap(), "ok");
        upstream.await.unwrap();
    }
}
