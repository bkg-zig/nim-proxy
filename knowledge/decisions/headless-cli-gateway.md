# Decision: headless CLI gateway

Date: 2026-08-31
Status: accepted
Supersedes: `ui-managed-config-store.md`, `auth-posture-and-dashboard-password.md`, `dashboard-operator-console-redesign.md` (operationally; those pages remain as history)

## Context

The fork's product goal changed: a minimal, standalone, headless CLI gateway
for a single NVIDIA NIM key, deployable on a headless server or inside an
existing container/VM with one command. The dashboard-first posture
(config store, wizard, sessions, history persistence, multi-key pool,
governor, compose files) directly conflicts with that goal: it requires a
browser for first-run setup, a writable `DATA_DIR`, and a second mental
model to operate.

## Decision

1. **CLI is the only configuration surface.** `--nim-api-key`, `--rpm`,
   `--host` (default `127.0.0.1`), `--port` (default 8000), optional
   `--base-url`, `--help`, `--version`. Validation errors exit 2 before
   any server starts. No config file, no `.env` (dotenvy removed), no env
   store, no wizard. The invocation is the deployment.
2. **The banner is the product.** After bind, stdout prints the
   OpenAI-compatible endpoint URL (`http://127.0.0.1:8000/v1`) plus
   non-secret run facts (host, port, RPM, concurrency 1, upstream). All
   tracing logs moved to stderr so stdout stays pipeable. The NVIDIA key
   is never printed anywhere.
3. **Strict single-flight serialization** (new `src/gate.rs`): one FIFO
   permit; held for a request's whole upstream lifecycle — RPM wait, send,
   retries, and the complete response (buffered body fully read; SSE
   stream fully terminated). Retries stay inside the holder's tenure, so a
   429 backoff can never let a queued request overtake. Client disconnect
   does not release the permit: the abandoned upstream stream is drained
   to its real termination (bounded by `stream_idle`) first — the barrier
   holds even for dead clients.
4. **max_inflight stays, conceptually separate.** Flood protection (512
   concurrent in-flight, 503 past it) bounds local memory; the gate bounds
   upstream concurrency. Different problems, different mechanisms — a
   queued request is *waiting*, never rejected by the gate itself.
5. **Open local endpoint.** `/v1` accepts any bearer (or none); the real
   credential is upstream-only. Safe by default via loopback binding;
   `--host 0.0.0.0` prints an explicit warning and a usable `localhost`
   URL in the banner.
6. **Bounded graceful shutdown.** SIGINT/SIGTERM stops accepts, drains
   in-flight work up to 30s, then exits 0; queued (unstarted) work is
   abandoned — the queue is in-memory by design. The drain clock starts
   at signal time, never at startup.
7. **Removed with the pivot:** dashboard/presentation, settings API,
   setup wizard, login/sessions/operator users, i18n machinery, OpenAPI
   generation, metrics history, `DATA_DIR` store, governor (single-flight
   makes worker-concurrency adaptation moot), multi-lane pool (one CLI
   key = one lane), Docker Compose. The scratch Dockerfile remains
   optional (ENTRYPOINT forwards CLI args). Dependencies dropped: utoipa,
   dotenvy, hmac, sha2, subtle, getrandom, serde.
8. **Test knobs, same posture as `HISTORY_SAMPLE_SECS` was:**
   `NIMPROXY_WINDOW_SECS` (pool window) and
   `NIMPROXY_SHUTDOWN_DRAIN_SECS` let the e2e suite exercise boundary
   behavior in seconds. Undocumented in the README, load-tested constants
   in production.

## Consequences

- The wire contract is preserved where it matters: `nimproxy_*` metric
  names/labels and the `{"error":{message,type,code}}` envelope are
  unchanged; `/health` and `/metrics` remain; `/v1/*` proxies verbatim.
- The e2e suite now asserts the four gateway invariants (FIFO,
  single-flight, complete-response barrier, rolling RPM) directly against
  a mock upstream whose event log records request start and full body
  consumption — the barrier is provable, not aspirational.
- Multi-key users must stay on v0.6.x or run one process per key.
- `knowledge/architecture/*` pages about the dashboard, history, and
  config store are now historical records, not current behavior.

## Alternatives considered

- **Keep the dashboard behind a flag**: rejected — two code paths to
  test, a browser dependency that resurfaces, and the store's
  fail-closed boot posture has nothing to be closed about when the CLI
  carries the entire configuration.
- **A semaphore around upstream calls:** rejected — tokio's semaphore is
  not FIFO-fair and does not model "held until the response is fully
  relayed"; a dedicated gate with an explicit permit lifetime is provable
  and testable.
- **Cancel upstream on client disconnect:** possible, but proving
  "actually terminated" from the client side is racy; draining to the
  stream's real end is verifiable from the server's event log and matches
  the conservative spirit of the rate limiter.
