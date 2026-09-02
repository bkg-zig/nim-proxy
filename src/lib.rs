//! Minimal headless CLI gateway for NVIDIA NIM.
//!
//! The whole application is one binary started from command-line arguments:
//! an OpenAI-compatible local endpoint in front of NVIDIA's hosted inference
//! API, with strict FIFO queueing, single-flight upstream execution, a
//! rolling-window RPM limiter, retries, and streaming — no dashboard, no
//! config store, no external services (see
//! `knowledge/decisions/headless-cli-gateway.md`).
//!
//! Request lifecycle (see `gate` and `proxy`):
//!
//! ```text
//! HTTP server -> FIFO single-flight gate (1) -> RPM dispatcher -> pool
//!            -> NVIDIA NIM
//! ```
//!
//! `main()` is a shim calling [`run`]; the logic lives in the library so the
//! fuzz targets under `fuzz/` can link the internals.

mod cli;
mod dispatch;
mod gate;
mod observation;
mod pool;
mod proxy;
mod routes;

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{any, get};
use axum::Router;
use bytes::Bytes;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use tokio::sync::Mutex as AsyncMutex;

use dispatch::Dispatcher;
use gate::Gate;
use pool::{Pool, PoolHandle};

// Recorder configuration and Rust-owned browser fixtures share this one
// production bucket registry; fixture code must never approximate it.
#[rustfmt::skip]
const HISTOGRAM_BUCKETS: &[(&str, &[f64])] = &[
    ("nimproxy_ttft_seconds",       &[0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]),
    ("nimproxy_tokens_per_second",  &[1.0, 2.0, 5.0, 10.0, 20.0, 40.0, 80.0, 160.0, 320.0]),
    ("nimproxy_queue_wait_seconds", &[0.001, 0.05, 0.25, 1.0, 5.0, 15.0, 60.0, 180.0, 600.0]),
    ("nimproxy_gate_wait_seconds",  &[0.001, 0.05, 0.25, 1.0, 5.0, 15.0, 60.0, 180.0, 600.0]),
    ("nimproxy_upstream_seconds",   &[0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0]),
    ("nimproxy_tpot_seconds",       &[0.005, 0.01, 0.02, 0.04, 0.08, 0.16, 0.32]),
    ("nimproxy_request_messages",   &[1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0]),
    ("nimproxy_request_tools",      &[0.0, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0]),
    ("nimproxy_request_max_tokens", &[128.0, 256.0, 512.0, 1024.0, 2048.0, 4096.0, 8192.0, 16384.0, 32768.0, 65536.0, 131072.0]),
    ("nimproxy_request_temperature", &[0.0, 0.2, 0.4, 0.6, 0.8, 1.0, 1.2, 1.5, 2.0]),
];

/// App-level configuration: fixed for the process lifetime, derived entirely
/// from the CLI invocation (plus library defaults). One immutable snapshot
/// every request sees via [`AppState::cfg`].
pub struct Config {
    pub base_url: String,
    /// Model injected when a request omits `model` (from `--model`).
    pub default_model: Option<String>,
    /// Model forced onto every request, overriding the client's choice
    /// (from `--force-model`). Exactly one of the two is ever set (the CLI
    /// rejects both together); `None`/`None` means strict passthrough.
    pub force_model: Option<String>,
    pub max_wait: Duration,
    pub heartbeat: Duration,
    pub models_ttl: Duration,
    /// Abort a stream when the upstream sends nothing for this long (0 = off).
    pub stream_idle: Duration,
    /// Overall deadline for a non-streaming upstream request (connect + body).
    /// Streaming has no overall cap (generation can be long) — it relies on
    /// `stream_idle` instead. Bounds a stalled buffered read holding a slot.
    pub request_timeout: Duration,
    /// Never modify request bodies (disables stream_options usage injection).
    pub strict_passthrough: bool,
    /// Cap on concurrent requests; bounds memory under floods. Conceptually
    /// separate from the single-flight gate: this bounds local memory/queue,
    /// the gate bounds upstream concurrency.
    pub max_inflight: usize,
}

pub struct AppState {
    pub cfg: Arc<Config>,
    /// The single-lane key pool built from `--nim-api-key` / `--rpm`.
    pub pool: PoolHandle,
    /// FIFO admission to RPM slots.
    pub dispatch: Dispatcher,
    /// Strict single-flight upstream serialization (complete-response barrier).
    pub gate: Gate,
    pub http: reqwest::Client,
    pub models_cache: AsyncMutex<Option<(Instant, Bytes)>>,
    /// Models that rejected stream_options injection; never inject for them again.
    pub no_inject: Mutex<std::collections::HashSet<String>>,
    /// Distinct sanitized model labels seen (bounds metric cardinality).
    pub model_labels: Mutex<std::collections::HashSet<String>>,
    /// Requests currently in flight; capped to bound memory under floods.
    pub inflight: AtomicUsize,
    /// Shared metrics registry rendered by `/metrics`.
    pub prometheus: PrometheusHandle,
}

impl AppState {
    /// One consistent config snapshot.
    pub fn cfg(&self) -> Arc<Config> {
        self.cfg.clone()
    }

    /// The current pool generation (observability only — reservations go
    /// through the dispatcher, which snapshots under the same lock).
    pub fn pool(&self) -> Arc<Pool> {
        self.pool.read().unwrap().clone()
    }
}

/// How long SIGINT/SIGTERM waits for in-flight work to drain before the
/// process exits anyway. Queued (not yet started) requests are abandoned:
/// the queue is deliberately in-memory only.
///
/// `NIMPROXY_SHUTDOWN_DRAIN_SECS` is an undocumented test knob (same posture
/// as the pool's window knob): it lets the e2e suite exercise the
/// bounded-drain exit in seconds. Production runs never set it and get the
/// 30s constant.
fn shutdown_drain() -> Duration {
    static OVERRIDE: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        std::env::var("NIMPROXY_SHUTDOWN_DRAIN_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(30))
    })
}

/// `nim-proxy --health`: probe our own /health endpoint and exit 0/1.
/// Exists because the scratch image has no shell or curl for HEALTHCHECK.
fn health_probe() -> ! {
    use std::io::{Read, Write};
    let port = std::env::args()
        .zip(std::env::args().skip(1))
        .find_map(|(flag, value)| (flag == "--port").then_some(value))
        .or_else(|| std::env::var("PORT").ok())
        .unwrap_or_else(|| cli::DEFAULT_PORT.to_string());
    let port: u16 = port.parse().unwrap_or(cli::DEFAULT_PORT);
    let ok = (|| -> std::io::Result<bool> {
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port))?;
        s.set_read_timeout(Some(Duration::from_secs(2)))?;
        s.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
        let mut buf = [0u8; 32];
        let n = s.read(&mut buf)?;
        Ok(String::from_utf8_lossy(&buf[..n]).contains("200"))
    })()
    .unwrap_or(false);
    std::process::exit(if ok { 0 } else { 1 });
}

fn startup_banner(args: &cli::Args) -> String {
    let mut host_note = String::new();
    if args.host == "0.0.0.0" || args.host == "::" || args.host == "[::]" {
        host_note = format!(
            "\nWARNING: --host {} exposes the proxy on every network interface.\n\
             Any client on the network can use your NVIDIA API key through it.\n",
            args.host
        );
    }
    format!(
        "\
nim-proxy {} started successfully.

OpenAI-compatible endpoint:
{}

Host:                       {}
Port:                       {}
RPM limit:                  {}
Maximum upstream concurrency: 1
Upstream:                   NVIDIA NIM ({})
Model:                      {}
{}The NVIDIA API key is never logged or exposed. Agents need only the endpoint
URL above (any bearer token is accepted locally). Press Ctrl-C to stop.
",
        env!("CARGO_PKG_VERSION"),
        args.endpoint_url(),
        args.host,
        args.port,
        args.rpm,
        args.base_url,
        args.model_policy(),
        host_note
    )
}

/// Print a failure the way a CLI tool should: message to stderr, non-zero
/// exit, no server started, no partial state.
fn bail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}

/// Full proxy entry point — everything `main()` used to be. Lives in the
/// library crate so the fuzz targets (fuzz/) can link the internals;
/// src/main.rs is a shim that calls this.
#[tokio::main]
pub async fn run() {
    if std::env::args().any(|a| a == "--health") {
        health_probe();
    }

    let args = match cli::parse(std::env::args_os().map(|a| a.to_string_lossy().into_owned())) {
        cli::Parsed::Run(args) => args,
        cli::Parsed::Help(text) => {
            print!("{text}");
            std::process::exit(0);
        }
        cli::Parsed::Version(text) => {
            print!("{text}");
            std::process::exit(0);
        }
        cli::Parsed::Error(text) => bail(&text),
    };

    tracing_subscriber::fmt()
        .compact()
        .with_target(false)
        // Logs belong on stderr: stdout carries exactly one thing — the
        // startup banner with the endpoint URL — so piping stdout stays
        // clean for scripts and SSH sessions.
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nim_proxy=info".into()),
        )
        .init();

    // A non-nvapi- key is almost always a pasted mistake worth a warning —
    // but self-hosted NIM servers issue other formats, so never refuse.
    if !args.nim_api_key.starts_with("nvapi-") {
        tracing::warn!(
            "the --nim-api-key does not start with 'nvapi-' — NVIDIA's hosted \
             keys do; proceeding anyway (self-hosted NIM keys may differ)"
        );
    }

    let cfg = Arc::new(Config {
        base_url: args.base_url.trim_end_matches('/').to_owned(),
        default_model: args.default_model.clone(),
        force_model: args.force_model.clone(),
        max_wait: Duration::from_secs(900),
        heartbeat: Duration::from_secs(10),
        models_ttl: Duration::from_secs(600),
        stream_idle: Duration::from_secs(300),
        request_timeout: Duration::from_secs(300),
        strict_passthrough: false,
        max_inflight: 512,
    });

    let mut builder = PrometheusBuilder::new();
    for (name, bounds) in HISTOGRAM_BUCKETS {
        builder = builder
            .set_buckets_for_metric(Matcher::Full((*name).into()), bounds)
            .unwrap();
    }
    let prometheus = builder.install_recorder().expect("prometheus recorder");
    metrics::describe_counter!(
        "nimproxy_usage_observations_total",
        "Final classified upstream usage observations by field and result."
    );
    metrics::describe_gauge!(
        "nimproxy_singleflight_active",
        "Whether the single upstream slot is currently occupied (0 or 1)."
    );
    metrics::describe_gauge!(
        "nimproxy_gate_depth",
        "Requests waiting for the single-flight upstream slot."
    );

    // One key, one lane: the pool's exact sliding window is the RPM limiter.
    let pool: PoolHandle = Arc::new(std::sync::RwLock::new(Arc::new(Pool::new(vec![
        pool::LaneSpec {
            key: args.nim_api_key.clone(),
            rpm: args.rpm as usize,
            enabled: true,
        },
    ]))));

    let state = Arc::new(AppState {
        dispatch: Dispatcher::new(pool.clone()),
        gate: Gate::new(),
        pool,
        http: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // No overall timeout: generations stream for a long time.
            .build()
            .expect("http client"),
        models_cache: AsyncMutex::new(None),
        no_inject: Mutex::new(std::collections::HashSet::new()),
        model_labels: Mutex::new(std::collections::HashSet::new()),
        inflight: AtomicUsize::new(0),
        prometheus,
        cfg,
    });

    let app = Router::new()
        .route(routes::HEALTH, get(|| async { "ok" }))
        .route(routes::METRICS, get(metrics_text))
        .route(routes::V1_WILDCARD, any(proxy::handle))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(state);

    let addr = format!("{}:{}", args.host, args.port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "nim-proxy cannot start: cannot listen on {addr} ({e}). \
                 Check --host/--port and whether the port is already in use."
            );
            std::process::exit(1);
        }
    };

    // The endpoint URL is the product: print it only once startup has truly
    // succeeded (arguments validated, listener bound).
    print!("{}", startup_banner(&args));
    use std::io::Write as _;
    let _ = std::io::stdout().flush();

    // Serve until SIGINT/SIGTERM, then drain in-flight work for at most
    // `shutdown_drain()`. The drain clock starts when the signal ARRIVES —
    // never at startup. Work still queued (not yet started upstream) is
    // abandoned: the queue is in-memory by design, and forcing it to persist
    // would drag in a datastore the product refuses to need.
    async fn shutdown_signal() {
        // Docker sends SIGTERM on stop; terminals send SIGINT.
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => {}
                _ = term.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = ctrl_c.await;
        }
    }

    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                shutdown_signal().await;
                tracing::info!(
                    "shutting down: draining in-flight requests (up to {}s); \
                     queued requests are abandoned — the queue is in-memory by design",
                    shutdown_drain().as_secs()
                );
            })
            .await
    });
    // Our own watcher for the same signals: it tells us when to start
    // bounding the drain window.
    shutdown_signal().await;
    match tokio::time::timeout(shutdown_drain(), server).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => {
            tracing::error!("server error: {e}");
            std::process::exit(1);
        }
        Ok(Err(join)) => {
            tracing::error!("server task failed: {join}");
            std::process::exit(1);
        }
        Err(_elapsed) => {
            tracing::error!(
                "graceful shutdown drain exceeded {}s; exiting with remaining \
                 work abandoned (in-flight upstream requests may be truncated)",
                shutdown_drain().as_secs()
            );
        }
    }
}

async fn metrics_text(State(state): State<Arc<AppState>>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )
        .body(axum::body::Body::from(state.prometheus.render()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_contains_the_endpoint_and_limits_not_the_key() {
        let args = cli::Args {
            nim_api_key: "nvapi-SECRET-VALUE".into(),
            rpm: 30,
            host: "127.0.0.1".into(),
            port: 8000,
            base_url: cli::DEFAULT_BASE_URL.into(),
            default_model: None,
            force_model: None,
        };
        let banner = startup_banner(&args);
        assert!(banner.contains("http://127.0.0.1:8000/v1"), "{banner}");
        assert!(banner.contains("RPM limit"), "{banner}");
        assert!(banner.contains("30"), "{banner}");
        assert!(banner.contains("concurrency"), "{banner}");
        assert!(banner.contains("NVIDIA NIM"), "{banner}");
        assert!(banner.contains("Model:"), "{banner}");
        assert!(
            banner.contains("passthrough (each client request names its own model)"),
            "{banner}"
        );
        assert!(
            !banner.contains("nvapi-SECRET-VALUE"),
            "the banner must never contain the API key"
        );
        assert!(
            !banner.contains("SECRET"),
            "the banner must never contain any part of the key"
        );
    }

    #[test]
    fn banner_shows_default_and_forced_model_policies() {
        let default = cli::Args {
            nim_api_key: "nvapi-x".into(),
            rpm: 30,
            host: "127.0.0.1".into(),
            port: 8000,
            base_url: cli::DEFAULT_BASE_URL.into(),
            default_model: Some("z-ai/glm-5.2".into()),
            force_model: None,
        };
        let banner = startup_banner(&default);
        assert!(
            banner.contains("z-ai/glm-5.2 (default — used when a request omits 'model')"),
            "{banner}"
        );

        let forced = cli::Args {
            nim_api_key: "nvapi-x".into(),
            rpm: 30,
            host: "127.0.0.1".into(),
            port: 8000,
            base_url: cli::DEFAULT_BASE_URL.into(),
            default_model: None,
            force_model: Some("meta/llama-3.3-70b-instruct".into()),
        };
        let banner = startup_banner(&forced);
        assert!(
            banner.contains(
                "meta/llama-3.3-70b-instruct (pinned — every request is rewritten to this model)"
            ),
            "{banner}"
        );
    }

    #[test]
    fn wildcard_host_banner_warns_and_uses_localhost_url() {
        let args = cli::Args {
            nim_api_key: "nvapi-x".into(),
            rpm: 30,
            host: "0.0.0.0".into(),
            port: 9123,
            base_url: cli::DEFAULT_BASE_URL.into(),
            default_model: None,
            force_model: None,
        };
        let banner = startup_banner(&args);
        assert!(banner.contains("http://localhost:9123/v1"), "{banner}");
        assert!(banner.contains("WARNING"), "{banner}");
    }
}
