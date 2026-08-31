//! End-to-end contract tests for the headless CLI gateway.
//!
//! Every test drives the REAL proxy binary (spawned via its CLI arguments,
//! exactly like production) against an in-process mock NIM upstream whose
//! event log records when each upstream request starts and when its response
//! body has been fully consumed. That log is the proof surface for the four
//! invariants of the gateway contract:
//!
//! 1. FIFO: `queue_order(A) < queue_order(B) => upstream_start(A) < upstream_start(B)`
//! 2. Single-flight: `max(simultaneous upstream requests) == 1`
//! 3. Complete-response barrier: `upstream_start(N+1) >= upstream_complete(N)`
//!    — including the full SSE stream, not the first chunk
//! 4. RPM: the rolling-window upstream count never exceeds the configured limit
//!
//! Retries (429/Retry-After/5xx) stay inside the retrying request's
//! lifecycle, and a downstream client disconnect cannot release the
//! single-flight slot before the upstream work actually finishes.

mod support;

use std::time::{Duration, Instant};

use futures_util::future::join_all;
use futures_util::StreamExt;
use support::*;

/// Bounded wait helper so a broken invariant hangs a test for seconds, not
/// the whole suite.
async fn soon<T>(what: &str, fut: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(Duration::from_secs(60), fut).await {
        Ok(value) => value,
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

// ---------------------------------------------------------------------------
// CLI surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cli_help_and_version_print_and_exit_zero() {
    let (code, stdout, _) = run_cli_once(&["--help"]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("USAGE"), "{stdout}");
    assert!(stdout.contains("--nim-api-key"), "{stdout}");
    assert!(stdout.contains("--rpm"), "{stdout}");
    assert!(stdout.contains("--host"), "{stdout}");
    assert!(stdout.contains("--port"), "{stdout}");

    let (code, stdout, _) = run_cli_once(&["--version"]);
    assert_eq!(code, Some(0));
    assert!(stdout.starts_with("nim-proxy "), "{stdout}");
}

#[tokio::test]
async fn cli_rejects_invalid_invocations_with_clear_errors() {
    // Missing key.
    let (code, _, stderr) = run_cli_once(&["--rpm", "30", "--port", "8000"]);
    assert_eq!(code, Some(2), "missing key must exit 2");
    assert!(stderr.contains("--nim-api-key is required"), "{stderr}");

    // Zero RPM.
    let (code, _, stderr) = run_cli_once(&["--nim-api-key", "nvapi-x", "--rpm", "0"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("--rpm must be at least 1"), "{stderr}");

    // Garbage RPM.
    let (code, _, stderr) = run_cli_once(&["--nim-api-key", "nvapi-x", "--rpm", "fast"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("--rpm must be a whole number"), "{stderr}");

    // Invalid port.
    let (code, _, stderr) = run_cli_once(&["--nim-api-key", "nvapi-x", "--port", "99999"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("--port must be a port number"), "{stderr}");

    // Port zero.
    let (code, _, stderr) = run_cli_once(&["--nim-api-key", "nvapi-x", "--port", "0"]);
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("--port must be between 1 and 65535"),
        "{stderr}"
    );

    // Unknown flag.
    let (code, _, stderr) = run_cli_once(&["--nim-api-key", "nvapi-x", "--max-wait", "5"]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("unknown option '--max-wait'"), "{stderr}");

    // Empty key.
    let (code, _, stderr) = run_cli_once(&["--nim-api-key", ""]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("must not be empty"), "{stderr}");
}

// ---------------------------------------------------------------------------
// Startup output and secret hygiene
// ---------------------------------------------------------------------------

#[tokio::test]
async fn startup_prints_endpoint_and_limits_but_never_the_key() {
    let mock = start_mock().await;
    let proxy = start_proxy(
        &mock.url,
        ProxyOpts {
            rpm: 30,
            ..Default::default()
        },
    )
    .await;
    // Give the banner a moment to land.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let out = proxy.stdout_text();
    assert!(out.contains("started successfully"), "{out}");
    assert!(out.contains("OpenAI-compatible endpoint:"), "{out}");
    assert!(
        out.contains(&format!("http://127.0.0.1:{}/v1", proxy.port)),
        "the endpoint URL with the actual port must be prominent: {out}"
    );
    assert!(out.contains("RPM limit"), "{out}");
    assert!(out.contains("30"), "{out}");
    assert!(out.contains("concurrency"), "{out}");
    assert!(out.contains("1"), "{out}");
    assert!(out.contains("NVIDIA NIM"), "{out}");

    // Generate traffic (including an upstream 429 retry and metrics) so any
    // leak has a chance to surface, then sweep every output channel.
    mock.state.push(Behavior::RateLimited(0));
    let client = reqwest::Client::new();
    let resp = chat(&client, &proxy, "leak-probe").await;
    assert_eq!(resp.status(), 200);
    let metrics = client
        .get(proxy.url("/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    for (channel, text) in [
        ("stdout", proxy.stdout_text()),
        ("stderr", proxy.stderr_text()),
        ("metrics", metrics),
    ] {
        assert!(
            !text.contains(TEST_KEY),
            "the NVIDIA key leaked into {channel}"
        );
        assert!(
            !text.contains("nvapi-e2e"),
            "partial key material leaked into {channel}"
        );
    }
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn wildcard_host_banner_warns_and_endpoint_stays_usable() {
    let mock = start_mock().await;
    let proxy = start_proxy(
        &mock.url,
        ProxyOpts {
            extra_args: vec!["--host".to_owned(), "0.0.0.0".to_owned()],
            ..Default::default()
        },
    )
    .await;
    let out = proxy.stdout_text();
    assert!(
        out.contains(
            "http://localhost:8000/v1"
                .replace("8000", &proxy.port.to_string())
                .as_str()
        ),
        "{out}"
    );
    assert!(out.contains("WARNING"), "{out}");
    assert!(out.contains("0.0.0.0"), "{out}");

    // Still reachable on loopback.
    let client = reqwest::Client::new();
    let resp = chat(&client, &proxy, "wildcard").await;
    assert_eq!(resp.status(), 200);
    proxy.terminate(Duration::from_secs(10)).await;
}

// ---------------------------------------------------------------------------
// OpenAI-compatible surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn buffered_requests_pass_through_and_use_the_cli_key_upstream() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    let client = reqwest::Client::new();

    // The downstream agent presents a bogus bearer (or none): the local
    // endpoint is open, and the real credential is applied upstream only.
    let resp = client
        .post(proxy.url("/v1/chat/completions"))
        .header("authorization", "Bearer agent-side-nonsense")
        .json(&chat_body("passthrough", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "application/json"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "hello world");

    let keys = mock.state.hit_keys();
    assert_eq!(keys, vec![TEST_KEY.to_owned()]);
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn streaming_requests_pass_through_with_usage_injection() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    let client = reqwest::Client::new();

    let resp = chat_stream(&client, &proxy, "sse").await;
    assert_eq!(resp.status(), 200);
    assert!(resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .starts_with("text/event-stream"));
    let text = read_sse(resp).await;
    assert!(text.contains("hello"), "{text}");
    assert!(text.contains("data: [DONE]"), "{text}");

    // The proxy injected stream_options.include_usage so usage is exact.
    let bodies = mock.state.hit_bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies[0]["stream_options"]["include_usage"],
        serde_json::json!(true)
    );
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn injection_falls_back_when_the_model_rejects_stream_options() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    mock.state.push(Behavior::BadRequestIfInjected);
    let client = reqwest::Client::new();

    let resp = chat_stream(&client, &proxy, "injection-fallback").await;
    let text = read_sse(resp).await;
    assert!(
        text.contains("data: [DONE]"),
        "client still gets a stream: {text}"
    );

    let bodies = mock.state.hit_bodies();
    assert_eq!(bodies.len(), 2, "one rejected attempt, one clean retry");
    assert!(bodies[0].get("stream_options").is_some());
    assert!(bodies[1].get("stream_options").is_none());
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn models_catalog_is_cached_and_cheap() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    let client = reqwest::Client::new();

    for _ in 0..3 {
        let resp = client.get(proxy.url("/v1/models")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["object"], "list");
    }
    assert_eq!(
        mock.state
            .models_hits
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "catalog polls must not burn rate budget"
    );
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn metrics_and_health_expose_the_headless_surface() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    let client = reqwest::Client::new();

    let health = client.get(proxy.url("/health")).send().await.unwrap();
    assert_eq!(health.status(), 200);

    let resp = chat(&client, &proxy, "metrics-probe").await;
    assert_eq!(resp.status(), 200);

    let metrics = client.get(proxy.url("/metrics")).send().await.unwrap();
    assert_eq!(metrics.status(), 200);
    let text = metrics.text().await.unwrap();
    assert!(text.contains("nimproxy_requests_total"), "{text}");
    assert!(text.contains("nimproxy_singleflight_active"), "{text}");
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn unknown_v1_paths_proxy_verbatim() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    let client = reqwest::Client::new();

    // The mock has no such route: strict passthrough relays its 404.
    let resp = client
        .get(proxy.url("/v1/nothing-here"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    proxy.terminate(Duration::from_secs(10)).await;
}

// ---------------------------------------------------------------------------
// FIFO
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_requests_reach_nvidia_in_fifo_order() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    let client = reqwest::Client::new();

    // Establish a known arrival order: spawn each request, then pause long
    // enough for it to reach the proxy's queue before the next one spawns.
    const N: usize = 8;
    let mut handles = Vec::new();
    for i in 0..N {
        let c = client.clone();
        let url = proxy.url("/v1/chat/completions");
        let convo = format!("convo-{i}");
        handles.push(tokio::spawn(async move {
            c.post(&url).json(&chat_body(&convo, false)).send().await
        }));
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    let responses = soon("all FIFO responses", join_all(handles)).await;
    for resp in responses {
        assert_eq!(resp.unwrap().unwrap().status(), 200);
    }

    let expected: Vec<String> = (0..N).map(|i| format!("convo-{i}")).collect();
    assert_eq!(
        mock.state.convos(),
        expected,
        "upstream must receive requests in arrival order"
    );
    mock.state.assert_completion_barrier();
    assert_eq!(mock.state.max_active(), 1, "single-flight");
    proxy.terminate(Duration::from_secs(10)).await;
}

// ---------------------------------------------------------------------------
// Single-flight + completion barrier
// ---------------------------------------------------------------------------

#[tokio::test]
async fn slow_upstream_never_sees_two_simultaneous_requests() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    for _ in 0..6 {
        mock.state.push(Behavior::DelayHeaders(150));
    }
    let client = reqwest::Client::new();

    // Six simultaneous requests against a 150ms upstream: they must queue.
    let mut handles = Vec::new();
    for i in 0..6 {
        let c = client.clone();
        let url = proxy.url("/v1/chat/completions");
        let convo = format!("slow-{i}");
        handles.push(tokio::spawn(async move {
            c.post(&url).json(&chat_body(&convo, false)).send().await
        }));
    }
    let responses = soon("all slow responses", join_all(handles)).await;
    for resp in responses {
        assert_eq!(resp.unwrap().unwrap().status(), 200);
    }

    assert_eq!(
        mock.state.max_active(),
        1,
        "maximum simultaneous upstream requests must be exactly 1"
    );
    mock.state.assert_completion_barrier();
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn next_request_waits_for_the_previous_complete_response() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    for _ in 0..8 {
        mock.state.push(Behavior::DelayHeaders(40));
    }
    let client = reqwest::Client::new();

    let mut handles = Vec::new();
    for i in 0..8 {
        let c = client.clone();
        let url = proxy.url("/v1/chat/completions");
        let convo = format!("barrier-{i}");
        handles.push(tokio::spawn(async move {
            c.post(&url).json(&chat_body(&convo, false)).send().await
        }));
    }
    let responses = soon("all barrier responses", join_all(handles)).await;
    for resp in responses {
        assert_eq!(resp.unwrap().unwrap().status(), 200);
    }

    // start(N+1) >= complete(N) for every consecutive pair — the whole
    // point of the complete-response barrier.
    mock.state.assert_completion_barrier();
    assert_eq!(mock.state.max_active(), 1);
    proxy.terminate(Duration::from_secs(10)).await;
}

// ---------------------------------------------------------------------------
// Streaming barrier
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_holds_the_slot_until_the_stream_fully_terminates() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    // A's SSE stream: 8 chunks at 80ms (~640ms) before [DONE] and clean
    // stream termination.
    mock.state.push(Behavior::StreamChunks {
        n: 8,
        interval_ms: 80,
    });
    let client = reqwest::Client::new();

    // A streams; B arrives 100ms later (long before A's stream ends).
    let a = {
        let c = client.clone();
        let url = proxy.url("/v1/chat/completions");
        tokio::spawn(async move { c.post(&url).json(&chat_body("stream-A", true)).send().await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let b = chat(&client, &proxy, "stream-B").await;

    let a_resp = soon("stream A", a).await.unwrap().unwrap();
    let a_text = read_sse(a_resp).await;
    assert!(a_text.contains("data: [DONE]"), "{a_text}");
    assert_eq!(b.status(), 200);

    // B must not START upstream until A's stream fully terminated upstream
    // (not first-chunk, not first-token: the tracked stream end).
    let starts = mock.state.starts();
    let completes = mock.state.completes();
    assert_eq!(starts.len(), 2);
    assert_eq!(completes.len(), 2);
    assert!(
        starts[1].1 >= completes[0].1,
        "B started before A's SSE stream terminated: start {:#?} vs complete {:#?}",
        starts[1].1,
        completes[0].1
    );
    assert_eq!(mock.state.max_active(), 1);
    proxy.terminate(Duration::from_secs(10)).await;
}

// ---------------------------------------------------------------------------
// RPM: rolling window
// ---------------------------------------------------------------------------

/// Largest count of `times` falling inside any window of `win`.
fn max_rolling(times: &[Instant], win: Duration) -> usize {
    let mut max = 0;
    for (i, t) in times.iter().enumerate() {
        let end = *t + win;
        let count = times[i..].iter().take_while(|u| **u < end).count();
        max = max.max(count);
    }
    max
}

#[tokio::test]
async fn many_concurrent_clients_fifo_single_flight_and_rolling_rpm() {
    let mock = start_mock().await;
    // Scaled window (3s) so the 30-RPM boundary is exercisable in seconds;
    // the production 61s window's semantics are covered by the pool unit
    // tests plus `rpm_holds_at_the_real_default_window` below.
    let proxy = start_proxy(
        &mock.url,
        ProxyOpts {
            rpm: 30,
            window_secs: Some(3),
            ..Default::default()
        },
    )
    .await;
    let client = reqwest::Client::new();

    // 100 simultaneous clients against an extremely fast upstream.
    const N: usize = 100;
    let mut handles = Vec::new();
    for i in 0..N {
        let c = client.clone();
        let url = proxy.url("/v1/chat/completions");
        let convo = format!("flood-{i}");
        handles.push(tokio::spawn(async move {
            c.post(&url).json(&chat_body(&convo, false)).send().await
        }));
    }
    let responses = soon("all 100 responses", join_all(handles)).await;
    for (i, resp) in responses.into_iter().enumerate() {
        assert_eq!(
            resp.unwrap().unwrap().status(),
            200,
            "every queued request must be served, never rejected (client {i})"
        );
    }

    // FIFO: upstream start order is a permutation-free, strictly increasing
    // arrival sequence (with simultaneous spawns any spawn order is legal,
    // but the proxy must serialize them one at a time in queue order).
    let starts = mock.state.starts();
    assert_eq!(starts.len(), N);
    for i in 0..starts.len().saturating_sub(1) {
        assert!(
            starts[i].1 <= starts[i + 1].1,
            "upstream starts must be ordered"
        );
    }

    // Single-flight.
    assert_eq!(mock.state.max_active(), 1);

    // Rolling window: the pool's window is 3s; upstream arrival lags the
    // reserve timestamp by milliseconds, so a 2.9s observation window with a
    // 100ms margin can never hold more than the 30-RPM budget.
    let times: Vec<Instant> = starts.iter().map(|(_, at)| *at).collect();
    let observed = max_rolling(&times, Duration::from_millis(2900));
    assert!(
        observed <= 30,
        "rolling-window upstream count exceeded the RPM limit: {observed} in 2.9s"
    );

    // Complete-response barrier across the whole flood.
    mock.state.assert_completion_barrier();
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn rpm_holds_at_the_real_default_window() {
    let mock = start_mock().await;
    // No window override: the production 61s rolling window. A short drain
    // bound keeps the documented bounded-exit observable without a 30s wait.
    let proxy = start_proxy(
        &mock.url,
        ProxyOpts {
            rpm: 30,
            shutdown_drain_secs: Some(2),
            ..Default::default()
        },
    )
    .await;
    let client = reqwest::Client::new();

    // 35 fast requests: 30 fit in the rolling window; the 31st..35th must
    // still be waiting (not upstream, not rejected) 5 seconds later.
    const N: usize = 35;
    let mut handles = Vec::new();
    for i in 0..N {
        let c = client.clone();
        let url = proxy.url("/v1/chat/completions");
        let convo = format!("hold-{i}");
        handles.push(tokio::spawn(async move {
            c.post(&url).json(&chat_body(&convo, false)).send().await
        }));
    }
    tokio::time::sleep(Duration::from_secs(5)).await;

    assert_eq!(
        mock.state.hit_count(),
        30,
        "the 31st request must wait for the rolling window, not go upstream"
    );
    assert_eq!(mock.state.max_active(), 1);
    mock.state.assert_completion_barrier();

    // The waiting clients are parked (their futures are still pending), and
    // the 30 admitted ones all succeeded.
    let mut done = 0;
    for h in &handles {
        if h.is_finished() {
            done += 1;
        }
    }
    assert_eq!(done, 30, "exactly the 30 window-admitted requests finished");

    // Abandon the 5 queued clients and shut down. The queue is in-memory:
    // SIGTERM drains what it can within the bounded window, then exits
    // cleanly (documented behavior — queued work is abandoned, not hung).
    for h in handles {
        h.abort();
    }
    drop(client);
    let status = proxy.terminate(Duration::from_secs(8)).await;
    assert!(
        status.success(),
        "bounded drain must exit cleanly even with queued work, got {status:?}"
    );
}

// ---------------------------------------------------------------------------
// Retries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_429_retry_prevents_b_from_bypassing() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    // A's first attempt is rate-limited (Retry-After 1s); the retry and every
    // later request succeed.
    mock.state.push(Behavior::RateLimited(1));
    let client = reqwest::Client::new();

    let a = {
        let c = client.clone();
        let url = proxy.url("/v1/chat/completions");
        tokio::spawn(async move { c.post(&url).json(&chat_body("A", false)).send().await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let b = chat(&client, &proxy, "B").await;
    let a_resp = soon("A after retry", a).await.unwrap().unwrap();

    assert_eq!(a_resp.status(), 200, "A must succeed via retry");
    assert_eq!(b.status(), 200, "B must succeed after A");

    // The retry is part of A's lifecycle: upstream order is A, A, B — never
    // A, B, A.
    assert_eq!(mock.state.convos(), vec!["A", "A", "B"]);
    mock.state.assert_completion_barrier();
    assert_eq!(
        mock.state.max_active(),
        1,
        "single-flight holds across the retry gap too"
    );
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn retry_after_header_is_respected() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    mock.state.push(Behavior::RateLimited(1));
    let client = reqwest::Client::new();

    let started = Instant::now();
    let resp = chat(&client, &proxy, "retry-after").await;
    assert_eq!(resp.status(), 200);
    let elapsed = started.elapsed();

    assert_eq!(mock.state.hit_count(), 2);
    let times = mock.state.hit_times();
    let gap = times[1].duration_since(times[0]);
    assert!(
        gap >= Duration::from_millis(900),
        "the upstream Retry-After must gate the retry: gap was {gap:?} (total {elapsed:?})"
    );
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn server_error_5xx_retries_preserve_fifo_and_single_flight() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    // A's first attempt dies with a 503; the retry succeeds.
    mock.state.push(Behavior::ServerError(503));
    let client = reqwest::Client::new();

    let a = {
        let c = client.clone();
        let url = proxy.url("/v1/chat/completions");
        tokio::spawn(async move { c.post(&url).json(&chat_body("A", false)).send().await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let b = chat(&client, &proxy, "B").await;
    let a_resp = soon("A after 503 retry", a).await.unwrap().unwrap();

    assert_eq!(a_resp.status(), 200);
    assert_eq!(b.status(), 200);

    assert_eq!(mock.state.convos(), vec!["A", "A", "B"]);
    mock.state.assert_completion_barrier();
    assert_eq!(mock.state.max_active(), 1);
    proxy.terminate(Duration::from_secs(10)).await;
}

// ---------------------------------------------------------------------------
// Client disconnect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_disconnect_does_not_release_the_slot_early() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    // A's stream: 10 chunks at 150ms (~1.5s) then termination.
    mock.state.push(Behavior::StreamChunks {
        n: 10,
        interval_ms: 150,
    });
    let client = reqwest::Client::new();

    // A connects and reads its first chunk...
    let a_resp = chat_stream(&client, &proxy, "disconnect-A").await;
    let mut a_stream = a_resp.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), a_stream.next())
        .await
        .expect("first chunk")
        .expect("stream ok")
        .expect("first chunk bytes");
    assert!(!first.is_empty());

    // ...then the downstream client vanishes mid-stream.
    drop(a_stream);

    // B arrives immediately — while A's upstream stream is still running.
    let b_started = Instant::now();
    let b = chat(&client, &proxy, "disconnect-B").await;
    assert_eq!(b.status(), 200);

    // The single-flight permit was NOT released when the client left: B
    // could only start upstream after A's stream fully terminated. The
    // barrier assertion proves it against the mock's event log.
    let starts = mock.state.starts();
    let completes = mock.state.completes();
    assert_eq!(starts.len(), 2);
    assert_eq!(
        completes.len(),
        2,
        "the proxy must drain A's upstream stream to its real termination, \
         not abandon it on client disconnect"
    );
    assert!(
        starts[1].1 >= completes[0].1,
        "B reached NVIDIA before A's aborted-but-still-running stream finished: \
         start {:#?} vs complete {:#?} (B waited {:?})",
        starts[1].1,
        completes[0].1,
        b_started.elapsed()
    );
    assert_eq!(mock.state.max_active(), 1);
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn queued_client_disconnect_does_not_break_the_queue() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    // A holds the slot for ~1s.
    mock.state.push(Behavior::StreamChunks {
        n: 8,
        interval_ms: 125,
    });
    let client = reqwest::Client::new();

    let a = {
        let c = client.clone();
        let url = proxy.url("/v1/chat/completions");
        tokio::spawn(async move { c.post(&url).json(&chat_body("queue-A", true)).send().await })
    };
    tokio::time::sleep(Duration::from_millis(60)).await;
    // B queues behind A, then its client drops while queued. The proxy task
    // is detached, so B's request still takes its turn upstream (bounded,
    // honest behavior); what matters is that C — queued behind B — still
    // completes in order.
    let b_resp = chat_stream(&client, &proxy, "queue-B").await;
    drop(b_resp);
    let c = chat(&client, &proxy, "queue-C").await;
    assert_eq!(c.status(), 200);

    let a_resp = soon("A completes", a).await.unwrap().unwrap();
    assert!(read_sse(a_resp).await.contains("data: [DONE]"));

    mock.state.assert_completion_barrier();
    assert_eq!(mock.state.max_active(), 1);
    proxy.terminate(Duration::from_secs(10)).await;
}

// ---------------------------------------------------------------------------
// Graceful shutdown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sigterm_drains_in_flight_work_and_exits_cleanly() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    // A 2s stream is in flight when SIGTERM arrives.
    mock.state.push(Behavior::StreamChunks {
        n: 20,
        interval_ms: 100,
    });
    let client = reqwest::Client::new();

    let stream = {
        let c = client.clone();
        let url = proxy.url("/v1/chat/completions");
        tokio::spawn(async move {
            let resp = c
                .post(&url)
                .json(&chat_body("shutdown", true))
                .send()
                .await?;
            Ok::<_, reqwest::Error>(read_sse(resp).await)
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    // SIGTERM: the proxy must stop accepting work, let the in-flight stream
    // finish, and exit zero — not hang, not corrupt.
    let status = proxy.terminate(Duration::from_secs(10)).await;
    assert!(
        status.success(),
        "graceful shutdown must exit 0, got {status:?}"
    );

    let body = soon("in-flight stream drained", stream)
        .await
        .expect("stream task")
        .expect("in-flight stream must complete during drain");
    assert!(
        body.contains("data: [DONE]"),
        "the in-flight generation finished: {body}"
    );
}

// ---------------------------------------------------------------------------
// Error paths and deadlines
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invalid_deadline_header_is_rejected_with_400() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    let client = reqwest::Client::new();

    for bad in ["12x3", "-5", ""] {
        let resp = client
            .post(proxy.url("/v1/chat/completions"))
            .header("x-nim-proxy-deadline-ms", bad)
            .json(&chat_body("deadline-bad", false))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "deadline '{bad}' must be rejected");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["code"], "invalid_deadline");
    }
    // Duplicated headers are ambiguous and rejected too.
    let resp = client
        .post(proxy.url("/v1/chat/completions"))
        .header("x-nim-proxy-deadline-ms", "500")
        .header("x-nim-proxy-deadline-ms", "900")
        .json(&chat_body("deadline-dup", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert_eq!(mock.state.hit_count(), 0, "nothing goes upstream");
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn request_deadline_aborts_when_upstream_is_unreachable() {
    // A dead upstream (nothing listens there) plus a 1.5s client deadline:
    // the proxy retries the connection, the deadline fires, and the client
    // gets a clear 504 instead of a hang.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    // The listener is dropped at the end of that block: the port stays unbound,
    // so connections refuse.
    let proxy = start_proxy(&format!("http://127.0.0.1:{port}"), ProxyOpts::default()).await;
    let client = reqwest::Client::new();

    let started = Instant::now();
    let resp = client
        .post(proxy.url("/v1/chat/completions"))
        .header("x-nim-proxy-deadline-ms", "1500")
        .json(&chat_body("dead-upstream", false))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 504);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "deadline_exceeded");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the deadline must bound the retry loop"
    );
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn non_retryable_upstream_error_surfaces_in_the_sse_stream() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    // 404 is not retryable and not the 400-injection-fallback case: the
    // failure must reach the already-committed SSE client as an in-stream
    // error event instead.
    mock.state.push(Behavior::ServerError(404));
    let client = reqwest::Client::new();

    let resp = chat_stream(&client, &proxy, "not-found").await;
    assert_eq!(resp.status(), 200);
    let text = read_sse(resp).await;
    assert!(text.contains("proxy_error"), "{text}");
    assert!(text.contains("upstream error 404"), "{text}");
    proxy.terminate(Duration::from_secs(10)).await;
}

#[tokio::test]
async fn models_catalog_failure_is_relayed_not_swallowed() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;
    mock.state.push_models(Behavior::ServerError(503));
    let client = reqwest::Client::new();

    let resp = client.get(proxy.url("/v1/models")).send().await.unwrap();
    assert_eq!(resp.status(), 503);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "catalog boom");

    // And the catalog recovers on the next poll once the upstream does.
    let resp = client.get(proxy.url("/v1/models")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "list");
    proxy.terminate(Duration::from_secs(10)).await;
}

// ---------------------------------------------------------------------------
// Health probe mode (Docker HEALTHCHECK path)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_probe_mode_reports_process_health() {
    let mock = start_mock().await;
    let proxy = start_proxy(&mock.url, ProxyOpts::default()).await;

    // `nim-proxy --health --port P` against a live proxy exits 0...
    let (code, _, _) = run_cli_once(&["--health", "--port", &proxy.port.to_string()]);
    assert_eq!(code, Some(0), "the probe must see a healthy server");

    // ...and against a dead port exits 1.
    let dead_port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    // (the probe listener is dropped above, leaving the port dead)
    let (code, _, _) = run_cli_once(&["--health", "--port", &dead_port.to_string()]);
    assert_eq!(code, Some(1), "the probe must fail on a dead server");
    proxy.terminate(Duration::from_secs(10)).await;
}
