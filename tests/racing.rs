//! Real-proxy regression coverage for the opt-in model racing data plane.
//! Model-specific gates make completion order explicit; dropped body guards
//! prove cancellation, rather than inferring it from a quick client response.

#[allow(dead_code)]
mod support;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;

use support::{chat_body, login, metrics, start_proxy_with, Proxy, StoreOpts};

const ORIGINAL: &str = "mock/model-a";
const SLOW: &str = "mock/slow";
const FAST: &str = "mock/fast";
const THIRD: &str = "mock/third";

#[derive(Clone)]
struct ModelReply {
    status: StatusCode,
    content_type: &'static str,
    before: Vec<Bytes>,
    gate: Option<Arc<Semaphore>>,
    after: Vec<Bytes>,
    hang: bool,
}

impl ModelReply {
    fn json(model: &str) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "application/json",
            before: Vec::new(),
            gate: None,
            after: vec![Bytes::from(
                json!({
                    "id": format!("reply-{model}"), "model": model,
                    "object": "chat.completion",
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": model}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                })
                .to_string(),
            )],
            hang: false,
        }
    }

    fn gated(model: &str) -> (Self, Arc<Semaphore>) {
        let gate = Arc::new(Semaphore::new(0));
        let mut reply = Self::json(model);
        reply.gate = Some(gate.clone());
        (reply, gate)
    }

    fn failure() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            content_type: "application/json",
            before: Vec::new(),
            gate: None,
            after: vec![Bytes::from_static(
                b"{\"error\":{\"message\":\"scripted non-retryable failure\"}}",
            )],
            hang: false,
        }
    }
}

#[derive(Default)]
struct Observations {
    calls: Vec<(String, String)>,
    emitted: HashMap<String, usize>,
    cancelled: HashMap<String, usize>,
}

struct MockState {
    replies: HashMap<String, ModelReply>,
    observations: Mutex<Observations>,
    changed: Notify,
}

struct ModelMock {
    url: String,
    state: Arc<MockState>,
    server: JoinHandle<()>,
}

impl Drop for ModelMock {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl ModelMock {
    async fn start(replies: Vec<(&str, ModelReply)>) -> Self {
        let state = Arc::new(MockState {
            replies: replies
                .into_iter()
                .map(|(name, reply)| (name.to_owned(), reply))
                .collect(),
            observations: Mutex::new(Observations::default()),
            changed: Notify::new(),
        });
        let app = Router::new()
            .route("/v1/chat/completions", post(mock_chat))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, state, server }
    }

    async fn wait_for(&self, label: &str, predicate: impl Fn(&Observations) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        loop {
            let changed = self.state.changed.notified();
            if predicate(&self.state.observations.lock().unwrap()) {
                return;
            }
            assert!(
                tokio::time::timeout_at(deadline, changed).await.is_ok(),
                "mock did not observe {label}; calls: {:?}",
                self.state.observations.lock().unwrap().calls
            );
        }
    }

    async fn called(&self, models: &[&str]) {
        self.wait_for("all expected model calls", |seen| {
            models
                .iter()
                .all(|model| seen.calls.iter().any(|(called, _)| called.as_str() == *model))
        })
        .await;
    }

    async fn cancelled(&self, model: &str) {
        self.wait_for("cancelled upstream body", |seen| {
            seen.cancelled.get(model).copied().unwrap_or_default() > 0
        })
        .await;
    }

    fn calls(&self) -> Vec<String> {
        self.state
            .observations
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|(model, _)| model.clone())
            .collect()
    }
}

struct BodyGuard {
    state: Arc<MockState>,
    model: String,
    complete: bool,
}

impl Drop for BodyGuard {
    fn drop(&mut self) {
        if !self.complete {
            *self
                .state
                .observations
                .lock()
                .unwrap()
                .cancelled
                .entry(self.model.clone())
                .or_default() += 1;
            self.state.changed.notify_one();
        }
    }
}

struct ReplyBody {
    guard: BodyGuard,
    before: VecDeque<Bytes>,
    gate: Option<Arc<Semaphore>>,
    after: VecDeque<Bytes>,
    hang: bool,
}

async fn mock_chat(
    State(state): State<Arc<MockState>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let model = body["model"].as_str().unwrap().to_owned();
    let key = headers
        .get(header::AUTHORIZATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    state
        .observations
        .lock()
        .unwrap()
        .calls
        .push((model.clone(), key));
    state.changed.notify_one();
    let reply = state
        .replies
        .get(&model)
        .cloned()
        .unwrap_or_else(ModelReply::failure);
    let response_body = ReplyBody {
        guard: BodyGuard {
            state,
            model,
            complete: false,
        },
        before: reply.before.into(),
        gate: reply.gate,
        after: reply.after.into(),
        hang: reply.hang,
    };
    let stream = futures_util::stream::unfold(response_body, |mut body| async move {
        let next = if let Some(bytes) = body.before.pop_front() {
            Some(bytes)
        } else {
            if let Some(gate) = body.gate.take() {
                gate.acquire_owned().await.unwrap().forget();
            }
            body.after.pop_front()
        };
        if let Some(bytes) = next {
            *body
                .guard
                .state
                .observations
                .lock()
                .unwrap()
                .emitted
                .entry(body.guard.model.clone())
                .or_default() += 1;
            body.guard.state.changed.notify_one();
            Some((Ok::<_, std::io::Error>(bytes), body))
        } else if body.hang {
            std::future::pending().await
        } else {
            body.guard.complete = true;
            drop(body);
            None
        }
    });
    Response::builder()
        .status(reply.status)
        .header(header::CONTENT_TYPE, reply.content_type)
        .body(Body::from_stream(stream))
        .unwrap()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap()
}

async fn configure(proxy: &Proxy, strategy: Option<Value>) {
    let cookie = login(proxy).await;
    let response = client()
        .post(proxy.url("/api/racing/config"))
        .header(header::COOKIE, cookie)
        .json(&json!({"enabled": strategy.is_some(), "strategy": strategy}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, StatusCode::OK, "racing configuration: {body}");
}

fn parallel(models: &[&str]) -> Value {
    json!({"type": "full_parallel", "models": models})
}

fn request(proxy: &Proxy, stream: bool, deadline_ms: Option<u64>) -> JoinHandle<(StatusCode, String)> {
    let url = proxy.url("/v1/chat/completions");
    tokio::spawn(async move {
        let mut request = client().post(url).json(&chat_body("racing regression", stream));
        if let Some(deadline) = deadline_ms {
            request = request.header("x-nim-proxy-deadline-ms", deadline.to_string());
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        (status, response.text().await.unwrap())
    })
}

fn winner(status: StatusCode, body: &str, model: &str) {
    assert_eq!(status, StatusCode::OK, "winner response: {body}");
    let body: Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["model"], model);
}

async fn permits_released(proxy: &Proxy, models: &[&str]) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let text = metrics(proxy).await;
        if models.iter().all(|model| {
            text.contains(&format!("nimproxy_model_inflight{{model=\"{model}\"}} 0"))
        }) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "model permits were not released: {text}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn disabled_racing_preserves_requested_model_and_body() {
    let mock = ModelMock::start(vec![
        (ORIGINAL, ModelReply::json(ORIGINAL)),
        (FAST, ModelReply::json(FAST)),
    ])
    .await;
    let proxy = start_proxy_with(&mock.url, StoreOpts::default(), &[]).await;
    configure(&proxy, Some(parallel(&[FAST]))).await;
    configure(&proxy, None).await;
    let (status, body) = request(&proxy, false, None).await.unwrap();
    winner(status, &body, ORIGINAL);
    assert_eq!(mock.calls(), [ORIGINAL]);
}

#[tokio::test]
async fn fastest_success_wins_and_drops_the_slow_upstream() {
    let (slow, _slow_gate) = ModelReply::gated(SLOW);
    let (fast, fast_gate) = ModelReply::gated(FAST);
    let mock = ModelMock::start(vec![(SLOW, slow), (FAST, fast)]).await;
    let proxy = start_proxy_with(&mock.url, StoreOpts::default(), &[]).await;
    configure(&proxy, Some(parallel(&[SLOW, FAST]))).await;
    let response = request(&proxy, false, None);
    mock.called(&[SLOW, FAST]).await;
    fast_gate.add_permits(1);
    let (status, body) = response.await.unwrap();
    winner(status, &body, FAST);
    mock.cancelled(SLOW).await;
    permits_released(&proxy, &[SLOW, FAST]).await;
}

#[tokio::test]
async fn failed_candidate_does_not_beat_a_later_success() {
    let (fast, fast_gate) = ModelReply::gated(FAST);
    let mock = ModelMock::start(vec![(SLOW, ModelReply::failure()), (FAST, fast)]).await;
    let proxy = start_proxy_with(&mock.url, StoreOpts::default(), &[]).await;
    configure(&proxy, Some(parallel(&[SLOW, FAST]))).await;
    let response = request(&proxy, false, None);
    mock.called(&[SLOW, FAST]).await;
    fast_gate.add_permits(1);
    let (status, body) = response.await.unwrap();
    winner(status, &body, FAST);
    assert_eq!(mock.calls().len(), 2, "non-retryable failure is not retried");
}

#[tokio::test]
async fn smart_timely_primary_does_not_call_fallbacks() {
    let (primary, gate) = ModelReply::gated(SLOW);
    let mock = ModelMock::start(vec![(SLOW, primary), (FAST, ModelReply::json(FAST))]).await;
    let proxy = start_proxy_with(&mock.url, StoreOpts::default(), &[]).await;
    configure(
        &proxy,
        Some(json!({"type": "smart", "primary_model": SLOW, "fallback_models": [FAST], "primary_timeout_ms": 4000})),
    )
    .await;
    let response = request(&proxy, false, None);
    mock.called(&[SLOW]).await;
    gate.add_permits(1);
    let (status, body) = response.await.unwrap();
    winner(status, &body, SLOW);
    assert_eq!(mock.calls(), [SLOW]);
}

#[tokio::test]
async fn smart_timeout_drops_primary_and_uses_fallback() {
    let (primary, _gate) = ModelReply::gated(SLOW);
    let mock = ModelMock::start(vec![(SLOW, primary), (FAST, ModelReply::json(FAST))]).await;
    let proxy = start_proxy_with(&mock.url, StoreOpts::default(), &[]).await;
    configure(
        &proxy,
        Some(json!({"type": "smart", "primary_model": SLOW, "fallback_models": [FAST], "primary_timeout_ms": 150})),
    )
    .await;
    let response = request(&proxy, false, None);
    mock.called(&[SLOW, FAST]).await;
    let (status, body) = response.await.unwrap();
    winner(status, &body, FAST);
    mock.cancelled(SLOW).await;
    permits_released(&proxy, &[SLOW, FAST]).await;
}

async fn semantic_stream_winner(delta: Value) {
    let loser_initial = ": loser-preface\n\ndata: {\"id\":\"loser-role\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n";
    let winner_initial = ": winner-preface\n\ndata: {\"id\":\"winner-role\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n";
    let token = format!("data: {}\n\n", json!({"id": "winner-token", "choices": [{"index": 0, "delta": delta}]}));
    let tail = "data: {\"id\":\"winner-tail\",\"choices\":[{\"delta\":{\"content\":\"after winner\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    let gate = Arc::new(Semaphore::new(0));
    let slow = ModelReply {
        status: StatusCode::OK,
        content_type: "text/event-stream",
        before: vec![Bytes::from(loser_initial)],
        gate: None,
        after: Vec::new(),
        hang: true,
    };
    // Split the winning JSON across transport chunks as well as preserving
    // the earlier role-only event. A network chunk is not an SSE event/token.
    let split = token.len() / 2;
    let fast = ModelReply {
        status: StatusCode::OK,
        content_type: "text/event-stream",
        before: vec![Bytes::from(winner_initial)],
        gate: Some(gate.clone()),
        after: vec![
            Bytes::copy_from_slice(&token.as_bytes()[..split]),
            Bytes::copy_from_slice(&token.as_bytes()[split..]),
            Bytes::from(tail),
        ],
        hang: false,
    };
    let mock = ModelMock::start(vec![(SLOW, slow), (FAST, fast)]).await;
    let proxy = start_proxy_with(&mock.url, StoreOpts::default(), &[]).await;
    configure(
        &proxy,
        Some(json!({"type": "ttft_racing", "models": [SLOW, FAST]})),
    )
    .await;
    let response = request(&proxy, true, None);
    mock.wait_for("both role-only prefixes", |seen| {
        [SLOW, FAST]
            .iter()
            .all(|model| seen.emitted.get(*model).copied().unwrap_or_default() >= 1)
    })
    .await;
    gate.add_permits(1);
    let (status, body) = response.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(&format!("{winner_initial}{token}{tail}")),
        "the complete winning stream must survive selection byte-for-byte: {body}"
    );
    assert!(!body.contains("loser-"), "loser bytes escaped: {body}");
    mock.cancelled(SLOW).await;
    permits_released(&proxy, &[SLOW, FAST]).await;
}

#[tokio::test]
async fn ttft_ignores_comments_and_empty_roles_until_content() {
    semantic_stream_winner(json!({"content": "answer"})).await;
}

#[tokio::test]
async fn ttft_accepts_reasoning_as_first_meaningful_delta() {
    semantic_stream_winner(json!({"reasoning_content": "thinking"})).await;
}

#[tokio::test]
async fn ttft_accepts_tool_call_as_first_meaningful_delta() {
    semantic_stream_winner(json!({"tool_calls": [{"index": 0, "id": "call-1", "type": "function", "function": {"name": "weather", "arguments": "{"}}]})).await;
}

#[tokio::test]
async fn all_failed_candidates_return_one_race_error() {
    let mock = ModelMock::start(vec![
        (SLOW, ModelReply::failure()),
        (FAST, ModelReply::failure()),
    ])
    .await;
    let proxy = start_proxy_with(&mock.url, StoreOpts::default(), &[]).await;
    configure(&proxy, Some(parallel(&[SLOW, FAST]))).await;
    let (status, body) = request(&proxy, false, None).await.unwrap();
    assert_eq!(status, StatusCode::BAD_GATEWAY, "all failed: {body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["error"]["code"], "race_failed");
    let mut calls = mock.calls();
    calls.sort();
    assert_eq!(calls, [FAST, SLOW]);
    permits_released(&proxy, &[SLOW, FAST]).await;
}

#[tokio::test]
async fn deadline_cancels_inflight_and_queued_candidates_without_bypassing_rpm() {
    let (slow, _slow_gate) = ModelReply::gated(SLOW);
    let (fast, _fast_gate) = ModelReply::gated(FAST);
    let (third, _third_gate) = ModelReply::gated(THIRD);
    let mock = ModelMock::start(vec![(SLOW, slow), (FAST, fast), (THIRD, third)]).await;
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            nim_keys: vec![("one-rpm-test-key".into(), 1)],
            max_inflight: 1,
            ..StoreOpts::default()
        },
        &[],
    )
    .await;
    configure(&proxy, Some(parallel(&[SLOW, FAST, THIRD]))).await;
    let (status, body) = request(&proxy, false, Some(300)).await.unwrap();
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "deadline: {body}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["error"]["code"], "deadline_exceeded");
    let calls = mock.calls();
    assert_eq!(calls.len(), 1, "one RPM permits exactly one upstream send");
    mock.cancelled(&calls[0]).await;
    assert_eq!(
        mock.state.observations.lock().unwrap().calls[0].1,
        "Bearer one-rpm-test-key"
    );
    // A cancelled generation must not refund an already sent RPM slot. A
    // following request is admitted at the client cap, then expires in queue.
    configure(&proxy, None).await;
    let (status, body) = request(&proxy, false, Some(150)).await.unwrap();
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "after deadline: {body}");
    assert_eq!(mock.calls().len(), 1, "sent RPM cannot be double spent");
    permits_released(&proxy, &[SLOW, FAST, THIRD, ORIGINAL]).await;
}

#[tokio::test]
async fn disconnect_releases_race_inflight_and_governor_permits() {
    let (slow, _slow_gate) = ModelReply::gated(SLOW);
    let mut fast = ModelReply::json(FAST);
    fast.content_type = "text/event-stream";
    fast.after = vec![Bytes::from_static(
        b"data: {\"choices\":[{\"delta\":{\"content\":\"selected winner\"}}]}\n\n",
    )];
    fast.hang = true;
    let mock = ModelMock::start(vec![
        (SLOW, slow),
        (FAST, fast),
        (ORIGINAL, ModelReply::json(ORIGINAL)),
    ])
    .await;
    let proxy = start_proxy_with(
        &mock.url,
        StoreOpts {
            max_inflight: 1,
            stream_idle_secs: 0,
            ..StoreOpts::default()
        },
        &[],
    )
    .await;
    let cookie = login(&proxy).await;
    for model in [SLOW, FAST] {
        let set_governor = client()
            .post(proxy.url("/api/settings/governor"))
            .header(header::COOKIE, &cookie)
            .json(&json!({"enabled": true, "set_override": {"model": model, "cap": 1}}))
            .send()
            .await
            .unwrap();
        assert_eq!(set_governor.status(), StatusCode::OK);
    }
    configure(
        &proxy,
        Some(json!({"type": "ttft_racing", "models": [SLOW, FAST]})),
    )
    .await;
    let response = client()
        .post(proxy.url("/v1/chat/completions"))
        .json(&chat_body("disconnect winner", true))
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();
    let mut body = String::new();
    tokio::time::timeout(Duration::from_secs(4), async {
        while !body.contains("selected winner") {
            body.push_str(&String::from_utf8_lossy(
                &stream.next().await.unwrap().unwrap(),
            ));
        }
    })
    .await
    .expect("winner should be selected while loser is blocked");
    mock.cancelled(SLOW).await;
    let (status, _) = request(&proxy, false, None).await.unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "open winner holds the one client inflight permit");
    drop(stream);
    mock.cancelled(FAST).await;
    permits_released(&proxy, &[SLOW, FAST]).await;
    configure(&proxy, None).await;
    let (status, body) = request(&proxy, false, Some(1000)).await.unwrap();
    winner(status, &body, ORIGINAL);
}
