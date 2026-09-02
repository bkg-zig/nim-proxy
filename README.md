<div align="center">

<img src="docs/assets/logo.png" alt="nim-proxy" width="140">

# nim-proxy

**A minimal, headless OpenAI-compatible gateway for the [NVIDIA NIM API](https://build.nvidia.com).**
One binary, one command, one job: make NVIDIA's rate limit invisible to your agents.

[![CI](https://github.com/miztertea/nim-proxy/actions/workflows/ci.yml/badge.svg)](https://github.com/miztertea/nim-proxy/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/miztertea/nim-proxy)](https://github.com/miztertea/nim-proxy/releases/latest)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/miztertea/nim-proxy/badge)](https://scorecard.dev/viewer/?uri=github.com/miztertea/nim-proxy)
[![OpenSSF Best Practices](https://www.bestpractices.dev/projects/13484/badge)](https://www.bestpractices.dev/projects/13484)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

</div>

---

nim-proxy is a local endpoint that another AI agent (OpenCode, Codex, n8n,
or anything that speaks the OpenAI API) can use as its inference backend.
The agent knows nothing about NVIDIA: not the URL, not the key, not the
rate limit, not the queue. The proxy handles all of it.

```text
                 one nim-proxy process
                        |
                 local HTTP server   http://127.0.0.1:8000/v1
                        |
                 in-memory FIFO queue   (requests wait, never rejected)
                        |
                 single-flight gate    (exactly 1 upstream request at a time)
                        |
                 rolling-window RPM    (default 30, configurable)
                        |
                        v
                 NVIDIA NIM API        (integrate.api.nvidia.com)
```

## Quick start

**1. Get an API key.** Sign up at [build.nvidia.com](https://build.nvidia.com)
(free; keys look like `nvapi-…`).

**2. Start the proxy.** No Docker, no config file, no setup wizard, no
dashboard, no database — the command line is the whole configuration:

```sh
./nim-proxy --nim-api-key "nvapi-..." --rpm 30 --port 8000
```

It prints the one thing you need — the endpoint — to stdout:

```
nim-proxy 0.7.1 started successfully.

OpenAI-compatible endpoint:
http://127.0.0.1:8000/v1

Host:                       127.0.0.1
Port:                       8000
RPM limit:                  30
Maximum upstream concurrency: 1
Upstream:                   NVIDIA NIM (https://integrate.api.nvidia.com)
Model:                      passthrough (each client request names its own model)
```

(All logs go to stderr; stdout carries only the banner. The NVIDIA key is
never printed anywhere.)

**3. Point an agent at it.** Configure the agent's OpenAI-compatible
backend with the base URL and any API key value it will accept locally:

| Agent setting | Value |
|---|---|
| Base URL | `http://127.0.0.1:8000/v1` |
| API key | anything (e.g. `local`) — the proxy applies the real credential upstream |

```sh
OPENAI_BASE_URL=http://127.0.0.1:8000/v1 OPENAI_API_KEY=local opencode
```

That's it. For step-by-step setup of specific agents (OpenCode, Hermes
Agent, MiMo Code, Prime Agent) and the full model-name story, see
**[USAGE.md](USAGE.md)** — it is also attached to every release.

The proxy transparently provides:

- **Strict FIFO** — requests reach NVIDIA in arrival order; later requests
  never overtake earlier ones.
- **Single-flight** — at most **one** NVIDIA request exists at any instant.
  If 100 requests arrive, one executes and 99 *wait* (they are never
  rejected with 503).
- **Complete-response barrier** — the next request starts only after the
  previous one has *completely finished* upstream: full body for buffered
  responses, full SSE stream termination for streaming ones. Retries
  (429/500/502/503/504, Retry-After, network errors) are part of the same
  request's lifecycle, so a retrying request cannot be bypassed either.
- **Rolling-window rate limit** — the default `--rpm 30` is deliberately
  conservative vs. NVIDIA's ~40-RPM free tier; the exact sliding window
  (with a 1s delivery-jitter margin) never lets the upstream see more.
- **Client disconnect safety** — if your agent hangs up mid-stream, the
  abandoned upstream generation is drained to its real end before the next
  request starts. The barrier always holds.
- **Streaming, heartbeats, catalog caching** — SSE streams pass through
  untouched; waiting clients receive `: heartbeat` comments so they don't
  time out; `/v1/models` is cached so catalog polls cost zero rate budget.

## Install

Build a standalone binary (no runtime dependencies):

```sh
cargo build --release
# target/release/nim-proxy
```

Or download the prebuilt static binary from this repository's
[releases page](https://github.com/katesean/nim-proxy/releases) and copy it
to any headless Linux host:

```sh
curl -LO https://github.com/katesean/nim-proxy/releases/latest/download/nim-proxy-0.7.1-linux-amd64.tar.gz
tar xzf nim-proxy-0.7.1-linux-amd64.tar.gz
install -m755 nim-proxy-amd64 /usr/local/bin/nim-proxy
```

Copy that one file to any headless Linux host, VM, or *existing* container
and run it — it needs nothing else: no Docker, no Redis, no database, no
browser, no TTY, no DISPLAY. SSH-only servers work exactly like local ones.

An (optional) ~5 MB scratch image is also attached to each release. If you
already have a container environment, pass the CLI arguments straight
through — including `--host 0.0.0.0`, which published ports (`-p`) require,
since the loopback default binds the container's own 127.0.0.1:
`docker run -p 8000:8000 ghcr.io/katesean/nim-proxy --nim-api-key nvapi-… --rpm 30 --host 0.0.0.0`
Docker itself is entirely optional.

## Model names — who specifies what

By default **the consumer (the AI agent) specifies the model** in each
request, exactly like the OpenAI API it already speaks: the agent's config
sets a model id, the request body carries `"model": "…"`, and nim-proxy
passes it to NVIDIA verbatim. This is the recommended posture — each agent
picks the NIM model it wants.

Two CLI flags change that, for the cases where the proxy side should own
the decision:

| Flag | Effect |
|---|---|
| *(none)* | Pass-through: each request's `model` field reaches NVIDIA verbatim. |
| `--model z-ai/glm-5.2` | Default: fills `model` in **only when a request omits it**. An explicit client model always wins. |
| `--force-model z-ai/glm-5.2` | Pinned: **every** request is rewritten to this model, regardless of what agents ask for. |

Model ids must match NVIDIA's catalog exactly — list what your key serves
with `curl http://127.0.0.1:8000/v1/models` (cached; costs no rate budget).
See [USAGE.md](USAGE.md) for concrete configuration of OpenCode, Hermes
Agent, MiMo Code, and Prime Agent, including their model-name settings.

## CLI reference

```
nim-proxy --nim-api-key <KEY> [--rpm <RPM>] [--host <HOST>] [--port <PORT>] [--base-url <URL>]
          [--model <MODEL>] [--force-model <MODEL>]
```

| Flag | Default | Meaning |
|---|---|---|
| `--nim-api-key` | *(required)* | NVIDIA NIM API key (`nvapi-…`). Applied upstream only; never logged. |
| `--rpm` | `30` | Rolling-window requests-per-minute limit. Must be ≥ 1. |
| `--host` | `127.0.0.1` | Listen address. `0.0.0.0` binds all interfaces (prints a warning — only for trusted networks). |
| `--port` | `8000` | Listen port. |
| `--base-url` | `https://integrate.api.nvidia.com` | Upstream NIM base URL (self-hosted NIM friendly). |
| `--model` | *(none)* | Default model id injected when a request omits `model`; an explicit client model wins. |
| `--force-model` | *(none)* | Pin every request to this model id, overriding each client's choice. Mutually exclusive with `--model`. |
| `-h`, `--help` | | Print usage and exit 0. |
| `-V`, `--version` | | Print the version and exit 0. |

Invalid usage (missing key, `--rpm 0`, port out of range, unknown flag)
exits with code 2 and a clear error **before** any server starts.

### Endpoints

| Path | Purpose |
|---|---|
| `/v1/*` | OpenAI-compatible API, proxied verbatim to NIM (chat completions, streaming, embeddings, models, …). |
| `/health` | Liveness probe (`200 ok`). |
| `/metrics` | Prometheus exposition (`nimproxy_*` series: queue depth and waits, gate depth and waits, TTFT, tokens/s, upstream seconds, usage observations). |

### Security posture

- The process binds to **127.0.0.1 by default**. The local endpoint accepts
  any bearer token (or none) — the real credential never leaves the proxy's
  upstream calls.
- Binding `--host 0.0.0.0` (for an agent in another container or network
  namespace) exposes the endpoint to the network: anyone who can reach it
  can spend your NVIDIA quota. The banner warns; use it only on trusted
  networks, and put TLS in front if it must be remote.
- The NVIDIA key is never written to stdout, stderr, logs, metrics, error
  bodies, or panic messages.

### Patience and shutdown

- A request waits in the FIFO queue for up to 15 minutes (the same
  "patience" bound as v0.6.x) before answering 504; under sustained load
  at 30 RPM a 100-deep queue drains in ~4 minutes. Flood protection beyond
  512 concurrent in-flight requests answers 503 — that cap bounds local
  memory and is conceptually separate from the single-flight gate.
- **SIGINT/SIGTERM** stops accepting new work, lets in-flight requests
  finish (up to 30 seconds), and exits 0. Requests still *queued* (not yet
  started upstream) are then abandoned — the queue is in-memory by design,
  and this is documented behavior, not data loss.

## Design notes

- The queue, the gate, and the rate window are all in-process and
  in-memory. One process is the whole deployment; there is nothing to
  operate.
- `max_inflight` (flood protection, default 512) and the single-flight gate
  are deliberately separate mechanisms: one bounds local memory under a
  connection flood, the other bounds upstream concurrency to one.
- The rate limiter is an exact rolling window (not a token bucket, not a
  fixed-minute counter) with a 1-second jitter margin — matching how NIM
  meters arrivals. See `knowledge/decisions/sliding-window-not-token-bucket.md`
  and `knowledge/decisions/window-jitter-margin.md`.
- Why single-flight? A conservative single key behaves best when NVIDIA
  sees strictly serialized, rate-limited traffic: no overlapping
  generations, no worker-exhaustion storms, and retries that can never
  overtake. Boring on purpose.
- This tool is **not** designed to circumvent NVIDIA's terms of service. It
  makes agents patient enough to live within one key's RPM budget.

### Quick smoke test

```sh
curl -s http://127.0.0.1:8000/v1/chat/completions \
  -H "content-type: application/json" \
  -H "authorization: Bearer local" \
  -d '{"model": "z-ai/glm-5.2", "messages": [{"role": "user", "content": "hi"}]}'
```

Full agent walkthroughs (OpenCode, Hermes Agent, MiMo Code, Prime Agent,
plus the OpenAI Python client): **[USAGE.md](USAGE.md)**.

## Development

```sh
cargo test              # unit + e2e (spawns the real binary against a mock NIM)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The e2e suite proves the contract directly against a tracked mock upstream:
FIFO order, single-flight (max simultaneous upstream requests == 1), the
complete-response barrier (buffered and streaming), 30-RPM rolling-window
enforcement, retry ordering (429/Retry-After/5xx), client-disconnect drain,
and graceful shutdown. See `tests/e2e.rs`.

Contributions welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Security
reports: [SECURITY.md](SECURITY.md) and [SUPPORT.md](SUPPORT.md).

## License

[MIT](LICENSE)
