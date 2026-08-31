//! Test harness: an in-process mock NIM upstream with scriptable behaviors
//! and per-request start/complete event tracking, plus a launcher that runs
//! the real proxy binary as a CLI child process against it.
//!
//! The event log is the proof surface for the gateway contract: every
//! upstream request logs `Start` when the mock handler begins and
//! `Complete` when its response body has been fully consumed by the proxy
//! (or `Aborted` when the connection was torn down first). FIFO,
//! single-flight, and the complete-response barrier are all asserted
//! directly against that log.

use std::collections::VecDeque;
use std::io::BufRead;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::{Stream, StreamExt};

/// One scripted response for the next chat-completions request. The queue is
/// consumed front-to-back; when empty, `Ok` is the default.
#[derive(Clone, Debug)]
pub enum Behavior {
    /// Respond normally (stream or JSON per the request's `stream` flag).
    Ok,
    /// 429 with this Retry-After (seconds).
    RateLimited(u64),
    /// A retryable server error.
    ServerError(u16),
    /// 400 only when the request carries stream_options (injection probe).
    BadRequestIfInjected,
    /// Wait before sending response headers, simulating an upstream that has
    /// accepted work but has not started its response.
    DelayHeaders(u64),
    /// A finite slow SSE stream: `n` data chunks spaced `interval_ms` apart,
    /// then usage + [DONE] + clean termination.
    StreamChunks { n: u64, interval_ms: u64 },
}

#[derive(Clone, Debug)]
pub enum EventKind {
    Start,
    Complete,
    Aborted,
}

/// One observed upstream lifecycle event.
#[derive(Clone, Debug)]
pub struct UpstreamEvent {
    pub kind: EventKind,
    /// Request sequence number at the mock (arrival order).
    pub seq: u64,
    pub at: Instant,
}

pub struct Hit {
    pub key: String,
    pub body: serde_json::Value,
    pub at: Instant,
}

#[derive(Default)]
pub struct MockState {
    pub hits: Mutex<Vec<Hit>>,
    pub script: Mutex<VecDeque<Behavior>>,
    /// Scriptable /v1/models responses (catalog fetch failures).
    pub models_script: Mutex<VecDeque<Behavior>>,
    pub models_hits: AtomicUsize,
    events: Mutex<Vec<UpstreamEvent>>,
    active: AtomicUsize,
    max_active: AtomicUsize,
    next_seq: AtomicU64,
}

impl MockState {
    pub fn push(&self, b: Behavior) {
        self.script.lock().unwrap().push_back(b);
    }
    /// Script the next /v1/models fetch (default: a normal catalog).
    pub fn push_models(&self, b: Behavior) {
        self.models_script.lock().unwrap().push_back(b);
    }
    pub fn hit_count(&self) -> usize {
        self.hits.lock().unwrap().len()
    }
    pub fn hit_bodies(&self) -> Vec<serde_json::Value> {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .map(|h| h.body.clone())
            .collect()
    }
    /// Conversation markers ("convo-A" etc.) in upstream arrival order.
    pub fn convos(&self) -> Vec<String> {
        self.hit_bodies()
            .iter()
            .map(|b| {
                b["messages"][1]["content"]
                    .as_str()
                    .unwrap_or("?")
                    .to_owned()
            })
            .collect()
    }
    pub fn hit_keys(&self) -> Vec<String> {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .map(|h| h.key.clone())
            .collect()
    }

    /// Arrival instants of every upstream hit, in order.
    pub fn hit_times(&self) -> Vec<Instant> {
        self.hits.lock().unwrap().iter().map(|h| h.at).collect()
    }

    fn record(&self, kind: EventKind, seq: u64) {
        if matches!(kind, EventKind::Start) {
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let mut seen_max = self.max_active.load(Ordering::SeqCst);
            while now > seen_max {
                match self.max_active.compare_exchange(
                    seen_max,
                    now,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(cur) => seen_max = cur,
                }
            }
        } else {
            self.active.fetch_sub(1, Ordering::SeqCst);
        }
        self.events.lock().unwrap().push(UpstreamEvent {
            kind,
            seq,
            at: Instant::now(),
        });
    }

    /// (seq, time) of every upstream request start, in order.
    pub fn starts(&self) -> Vec<(u64, Instant)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e.kind, EventKind::Start))
            .map(|e| (e.seq, e.at))
            .collect()
    }

    /// (seq, time) of every upstream completion (body fully consumed), in
    /// order.
    pub fn completes(&self) -> Vec<(u64, Instant)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e.kind, EventKind::Complete))
            .map(|e| (e.seq, e.at))
            .collect()
    }

    /// Highest number of simultaneously-active upstream requests ever seen.
    pub fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }

    /// For every upstream request i, assert complete(i) <= start(i+1).
    pub fn assert_completion_barrier(&self) {
        let starts = self.starts();
        let completes = self.completes();
        assert_eq!(
            starts.len(),
            completes.len(),
            "every upstream request must complete (no hangs, no leaks)"
        );
        for i in 0..starts.len().saturating_sub(1) {
            assert!(
                starts[i + 1].1 >= completes[i].1,
                "complete-response barrier violated: request {} started before request {} \
                 fully completed (start at {:#?} < complete at {:#?})",
                i + 1,
                i,
                starts[i + 1].1,
                completes[i].1
            );
        }
    }
}

pub struct MockNim {
    pub url: String,
    pub state: Arc<MockState>,
}

pub async fn start_mock() -> MockNim {
    let state = Arc::new(MockState::default());
    let app = Router::new()
        .route("/v1/models", get(mock_models))
        .route("/v1/chat/completions", post(mock_chat))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    MockNim { url, state }
}

async fn mock_models(State(state): State<Arc<MockState>>) -> Response {
    state.models_hits.fetch_add(1, Ordering::SeqCst);
    let behavior = state
        .models_script
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or(Behavior::Ok);
    match behavior {
        Behavior::RateLimited(secs) => Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header(header::RETRY_AFTER, secs.to_string())
            .body(Body::from(r#"{"error":"catalog rate limited"}"#))
            .unwrap()
            .into_response(),
        Behavior::ServerError(code) => Response::builder()
            .status(StatusCode::from_u16(code).unwrap())
            .body(Body::from(r#"{"error":"catalog boom"}"#))
            .unwrap()
            .into_response(),
        _ => axum::Json(serde_json::json!({
            "object": "list",
            "data": [{"id": "mock/model-a", "object": "model", "created": 0, "owned_by": "mock"}]
        }))
        .into_response(),
    }
}

async fn mock_chat(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let key = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();
    state.hits.lock().unwrap().push(Hit {
        key,
        body: parsed.clone(),
        at: Instant::now(),
    });
    let behavior = state
        .script
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or(Behavior::Ok);
    let wants_stream = parsed["stream"].as_bool().unwrap_or(false);
    let injected = parsed.get("stream_options").is_some();

    // The event lifecycle: Start here, Complete when the response body has
    // been consumed to its end, Aborted if the connection is torn down
    // first. The proxy's single-flight gate serializes these whole spans.
    let seq = state.next_seq.fetch_add(1, Ordering::SeqCst);
    state.record(EventKind::Start, seq);

    let usage = "\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2}";

    let resp: Response = match behavior {
        Behavior::RateLimited(secs) => Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header(header::RETRY_AFTER, secs.to_string())
            .body(Body::from(r#"{"error":"rate limited"}"#))
            .unwrap()
            .into_response(),
        Behavior::ServerError(code) => Response::builder()
            .status(StatusCode::from_u16(code).unwrap())
            .body(Body::from(r#"{"error":"boom"}"#))
            .unwrap()
            .into_response(),
        Behavior::BadRequestIfInjected if injected => Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from(r#"{"error":{"message":"bad stream_options"}}"#))
            .unwrap()
            .into_response(),
        Behavior::DelayHeaders(ms) => {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            axum::Json(serde_json::json!({
                "id": "mock-delayed", "object": "chat.completion",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": "delayed"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1}
            }))
            .into_response()
        }
        Behavior::StreamChunks { n, interval_ms } => {
            let chunks: Vec<Result<Bytes, std::io::Error>> = (0..n)
                .map(|i| {
                    Ok(Bytes::from(format!(
                        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"chunk-{i}\"}}}}]}}\n\n"
                    )))
                })
                .chain([Ok(Bytes::from(format!(
                    "data: {{\"choices\":[],{usage}}}\n\n"
                )))])
                .chain([Ok(Bytes::from("data: [DONE]\n\n"))])
                .collect();
            // Emit the chunks on the requested cadence so the stream stays
            // open (and the single-flight permit held) for a known duration.
            let paced = futures_util::stream::iter(chunks).then(move |chunk| async move {
                tokio::time::sleep(Duration::from_millis(interval_ms)).await;
                chunk
            });
            sse(Body::from_stream(paced))
        }
        Behavior::Ok | Behavior::BadRequestIfInjected => {
            if wants_stream {
                let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
                    Ok(Bytes::from(
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hello\"}}]}\n\n",
                    )),
                    Ok(Bytes::from(
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":\"stop\"}]}\n\n",
                    )),
                    Ok(Bytes::from(format!("data: {{\"choices\":[],{usage}}}\n\n"))),
                    Ok(Bytes::from("data: [DONE]\n\n")),
                ];
                sse(Body::from_stream(futures_util::stream::iter(chunks)))
            } else {
                axum::Json(serde_json::json!({
                    "id": "mock-1", "object": "chat.completion",
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello world"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 11, "completion_tokens": 2}
                }))
                .into_response()
            }
        }
    };
    tracked(state, seq, resp)
}

/// Wrap a response body so its full consumption is observable: `Complete` is
/// recorded when the stream is polled to its end, `Aborted` when the
/// connection is dropped first. All mock bodies go through this, so a
/// "complete" always means the proxy actually drained the upstream response.
fn tracked(state: Arc<MockState>, seq: u64, resp: Response) -> Response {
    let (parts, body) = resp.into_parts();
    let stream = TrackedStream {
        inner: body.into_data_stream(),
        state,
        seq,
        finished: false,
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

fn sse(body: Body) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .unwrap()
}

struct TrackedStream<S> {
    inner: S,
    state: Arc<MockState>,
    seq: u64,
    finished: bool,
}

impl<S: Stream + Unpin> Stream for TrackedStream<S> {
    type Item = S::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match std::pin::Pin::new(&mut self.inner).poll_next(cx) {
            std::task::Poll::Ready(None) => {
                if !self.finished {
                    self.finished = true;
                    self.state.record(EventKind::Complete, self.seq);
                }
                std::task::Poll::Ready(None)
            }
            other => other,
        }
    }
}

impl<S> Drop for TrackedStream<S> {
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;
            self.state.record(EventKind::Aborted, self.seq);
        }
    }
}

/// The real proxy binary running as a CLI child process, with its stdout and
/// stderr captured into shared line buffers (so the banner and any leak can
/// be asserted without blocking the pipes).
pub struct Proxy {
    pub port: u16,
    pub child: std::process::Child,
    pub stdout: Arc<Mutex<Vec<String>>>,
    pub stderr: Arc<Mutex<Vec<String>>>,
}

impl Proxy {
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }

    /// Lines printed so far on stdout (the startup banner; never logs).
    pub fn stdout_text(&self) -> String {
        self.stdout.lock().unwrap().join("\n")
    }

    /// Lines printed so far on stderr (logs).
    pub fn stderr_text(&self) -> String {
        self.stderr.lock().unwrap().join("\n")
    }

    /// Send SIGTERM and await the process's exit (bounded by `max_wait`).
    /// Awaits — never blocks the test runtime — so the in-process mock and
    /// in-flight clients keep making progress while the proxy drains.
    pub async fn terminate(mut self, max_wait: Duration) -> std::process::ExitStatus {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let deadline = Instant::now() + max_wait;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "proxy did not exit after SIGTERM; stdout:\n{}\nstderr:\n{}",
                self.stdout_text(),
                self.stderr_text()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        // A SIGKILLed process never flushes its coverage profile. Under
        // `cargo llvm-cov` (detected via LLVM_PROFILE_FILE) ask for a graceful
        // SIGTERM — which the proxy handles — and wait briefly so the child
        // writes its profile; otherwise kill immediately to keep teardown fast.
        if std::env::var_os("LLVM_PROFILE_FILE").is_some() {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status();
            for _ in 0..150 {
                if let Ok(Some(_)) = self.child.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The NIM key every test proxy uses (asserted upstream, never leaked).
pub const TEST_KEY: &str = "nvapi-e2e-test-key-0123456789";

pub struct ProxyOpts {
    pub rpm: u32,
    pub window_secs: Option<u64>,
    /// Shutdown-drain bound override (test knob; production default 30s).
    pub shutdown_drain_secs: Option<u64>,
    pub extra_args: Vec<String>,
}

impl Default for ProxyOpts {
    fn default() -> Self {
        Self {
            rpm: 30,
            window_secs: None,
            shutdown_drain_secs: None,
            extra_args: Vec::new(),
        }
    }
}

/// Start the proxy binary with a validated CLI invocation against `upstream`.
pub async fn start_proxy(upstream: &str, opts: ProxyOpts) -> Proxy {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut args = vec![
        "--nim-api-key".to_owned(),
        TEST_KEY.to_owned(),
        "--rpm".to_owned(),
        opts.rpm.to_string(),
        "--port".to_owned(),
        port.to_string(),
        "--base-url".to_owned(),
        upstream.to_owned(),
    ];
    args.extend(opts.extra_args);
    let mut envs: Vec<(String, String)> = vec![("RUST_LOG".into(), "nim_proxy=info".into())];
    if let Some(secs) = opts.window_secs {
        envs.push(("NIMPROXY_WINDOW_SECS".into(), secs.to_string()));
    }
    if let Some(secs) = opts.shutdown_drain_secs {
        envs.push(("NIMPROXY_SHUTDOWN_DRAIN_SECS".into(), secs.to_string()));
    }
    spawn_and_wait_healthy(port, &args, &envs).await
}

async fn spawn_and_wait_healthy(port: u16, args: &[String], envs: &[(String, String)]) -> Proxy {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_nim-proxy"));
    cmd.env_clear()
        .current_dir(std::env::temp_dir()) // dodge any local .env
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    // Under `cargo llvm-cov` the spawned server must write its own coverage
    // profile, but env_clear() above dropped the profile path — forward it back
    // when present (a no-op in normal test runs).
    if let Ok(v) = std::env::var("LLVM_PROFILE_FILE") {
        cmd.env("LLVM_PROFILE_FILE", v);
    }
    let mut child = cmd.spawn().expect("spawn proxy");
    let stdout = Arc::new(Mutex::new(Vec::new()));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    if let Some(out) = child.stdout.take() {
        let sink = stdout.clone();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines() {
                match line {
                    Ok(line) => sink.lock().unwrap().push(line),
                    Err(_) => break,
                }
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let sink = stderr.clone();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(err).lines() {
                match line {
                    Ok(line) => sink.lock().unwrap().push(line),
                    Err(_) => break,
                }
            }
        });
    }
    let proxy = Proxy {
        port,
        child,
        stdout,
        stderr,
    };

    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(r) = client.get(proxy.url("/health")).send().await {
            if r.status().is_success() {
                return proxy;
            }
        }
        assert!(
            Instant::now() < deadline,
            "proxy did not become healthy; stdout:\n{}\nstderr:\n{}",
            proxy.stdout_text(),
            proxy.stderr_text()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Run the proxy binary once to completion (CLI validation cases) and return
/// (exit code, stdout, stderr).
pub fn run_cli_once(args: &[&str]) -> (Option<i32>, String, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_nim-proxy"));
    cmd.env_clear().args(args);
    if let Ok(v) = std::env::var("LLVM_PROFILE_FILE") {
        cmd.env("LLVM_PROFILE_FILE", v);
    }
    let out = cmd.output().expect("run proxy once");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Read an SSE response to completion (until [DONE], an error event, or EOF);
/// returns the full body text.
pub async fn read_sse(resp: reqwest::Response) -> String {
    let mut out = String::new();
    let mut stream = resp.bytes_stream();
    let deadline = Instant::now() + Duration::from_secs(20);
    while let Ok(Some(chunk)) = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        stream.next(),
    )
    .await
    {
        out.push_str(&String::from_utf8_lossy(&chunk.expect("stream chunk")));
        if out.contains("data: [DONE]") || out.contains("\"proxy_error\"") {
            break;
        }
    }
    out
}

/// A chat-completions body for conversation `convo` (unique marker).
pub fn chat_body(convo: &str, stream: bool) -> serde_json::Value {
    serde_json::json!({
        "model": "mock/model-a",
        "stream": stream,
        "messages": [
            {"role": "system", "content": "you are a test"},
            {"role": "user", "content": convo}
        ]
    })
}

/// Fire one buffered chat completion; returns the response.
pub async fn chat(client: &reqwest::Client, proxy: &Proxy, convo: &str) -> reqwest::Response {
    client
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body(convo, false))
        .send()
        .await
        .expect("chat request")
}

/// Fire one streaming chat completion; returns the SSE response.
pub async fn chat_stream(
    client: &reqwest::Client,
    proxy: &Proxy,
    convo: &str,
) -> reqwest::Response {
    client
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body(convo, true))
        .send()
        .await
        .expect("stream request")
}
