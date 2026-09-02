# nim-proxy — Usage Guide

**A minimal, headless, OpenAI-compatible gateway for the [NVIDIA NIM API](https://build.nvidia.com).**
One binary. One command. No Docker, no database, no dashboard, no config file.

This guide covers installing the binary, starting it, **where the model name
comes from**, and pointing an AI agent — OpenCode, Hermes Agent, MiMo Code
(Mimocode), or Prime Agent — at it.

---

## Table of contents

1. [Install](#1-install)
2. [Get a NVIDIA key and pick a model](#2-get-a-nvidia-key-and-pick-a-model)
3. [Start the proxy](#3-start-the-proxy)
4. [The model name: who specifies it](#4-the-model-name-who-specifies-it)
5. [CLI reference](#5-cli-reference)
6. [What the proxy guarantees](#6-what-the-proxy-guarantees)
7. [Agent setup guides](#7-agent-setup-guides)
   - [OpenCode](#opencode)
   - [Hermes Agent](#hermes-agent)
   - [MiMo Code (Mimocode)](#mimo-code-mimocode)
   - [Prime Agent](#prime-agent)
   - [Any other OpenAI-compatible consumer](#any-other-openai-compatible-consumer)
8. [Operations: metrics, health, shutdown](#8-operations-metrics-health-shutdown)
9. [Security notes](#9-security-notes)
10. [Troubleshooting](#10-troubleshooting)

---

## 1. Install

**Option A — prebuilt binary (recommended).** Download the static binary
from the [releases page](https://github.com/katesean/nim-proxy/releases)
and put it anywhere on your `PATH`:

```sh
curl -LO https://github.com/katesean/nim-proxy/releases/latest/download/nim-proxy-0.7.1-linux-amd64.tar.gz
tar xzf nim-proxy-0.7.1-linux-amd64.tar.gz
install -m755 nim-proxy-amd64 /usr/local/bin/nim-proxy
nim-proxy --version    # → nim-proxy 0.7.1
```

The binary is fully static (musl): no libc, no runtime dependencies, no
installer. It runs on any Linux amd64 host — a bare server, a VM, or an
*existing* container. An arm64 build (`linux-arm64.tar.gz`) is attached to
the same release.

**Option B — build from source.**

```sh
git clone https://github.com/katesean/nim-proxy.git
cd nim-proxy
cargo build --release
# → target/release/nim-proxy
```

**Sanity check** — the tool is headless by design; nothing here needs a
browser, TTY, or `DISPLAY`:

```sh
nim-proxy --help
nim-proxy --version
```

Invalid invocations (missing key, `--rpm 0`, bad port, unknown flag) exit
with code 2 and a clear error **before** any server starts.

---

## 2. Get a NVIDIA key and pick a model

1. Sign up at [build.nvidia.com](https://build.nvidia.com) (free tier: ~40
   requests/minute per key). Your key looks like `nvapi-…`.
2. Pick the model(s) you want to serve. NVIDIA NIM model ids are namespaced
   and must be used verbatim, e.g.:

   | Model id | What it is |
   |---|---|
   | `z-ai/glm-5.2` | Z.ai GLM-5.2, long-context agentic coding model |
   | `meta/llama-3.3-70b-instruct` | Meta Llama 3.3 70B |
   | `deepseek-ai/deepseek-r1` | DeepSeek R1 reasoning model |
   | `qwen/qwen2.5-coder-32b-instruct` | Qwen 2.5 Coder 32B |
   | `nvidia/llama-3.1-nemotron-70b-instruct` | NVIDIA Nemotron 70B |

   The exact list your key can serve comes from the catalog itself — once
   the proxy is running (next section):

   ```sh
   curl -s http://127.0.0.1:8000/v1/models | python3 -m json.tool | grep '"id"'
   ```

   (`/v1/models` is proxied from NVIDIA and cached for 10 minutes, so
   catalog polls cost zero rate budget.)

---

## 3. Start the proxy

The canonical invocation — this is the entire deployment:

```sh
nim-proxy --nim-api-key "nvapi-..." --rpm 30 --host 127.0.0.1 --port 8000
```

It prints the one thing your agents need — the endpoint — to stdout:

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

Notes:

- `--rpm 30` is deliberately conservative vs. NVIDIA's ~40 RPM free tier;
  the limiter is an exact 60-second rolling window (plus a 1 s jitter
  margin), so NVIDIA never sees a burst beyond your budget.
- All logs go to **stderr**; stdout carries only the banner. The NVIDIA key
  is never printed anywhere — not to stdout, stderr, logs, metrics, error
  bodies, or panics.
- Run it under systemd, tmux, `nohup`, or your process supervisor of
  choice. SIGINT/SIGTERM drain in-flight work (up to 30 s) and exit cleanly.
- If your agent runs in **another container or network namespace** on the
  same host, start with `--host 0.0.0.0` (the banner warns: only on trusted
  networks — anyone who can reach the port can spend your quota) and point
  the agent at `http://<host-ip>:8000/v1`.
- The same applies **inside Docker**: `-p 8000:8000` publishes the port
  through the container's network interface, so a loopback bind is
  unreachable from outside the container — run the image with
  `--host 0.0.0.0`:

  ```sh
  docker run -p 8000:8000 ghcr.io/katesean/nim-proxy:0.7.1 \
    --nim-api-key "nvapi-..." --rpm 30 --host 0.0.0.0
  ```

---

## 4. The model name: who specifies it

**Short answer: by default the consumer (the AI agent) specifies the model
name, and the proxy passes it through verbatim. The proxy can optionally
provide a default (`--model`) or pin one model for everyone
(`--force-model`). Both are now fully implemented and tested.**

The OpenAI API convention is that every chat-completions request carries a
model id in its body:

```json
{ "model": "z-ai/glm-5.2", "messages": [ ... ] }
```

nim-proxy supports all three postures, chosen at startup:

| Mode | How | Behavior |
|---|---|---|
| **Pass-through** *(default)* | no flags | Each request's `model` field reaches NVIDIA verbatim. Every agent picks its own model. Recommended. |
| **Proxy default** | `--model z-ai/glm-5.2` | Requests that **omit** `model` (or send an empty one) get the default injected. An explicit client model always wins. |
| **Pinned** | `--force-model z-ai/glm-5.2` | **Every** request is rewritten to this model id, regardless of what agents ask for. For pointing a whole fleet at one model. |

```sh
# Agent picks the model (recommended default):
nim-proxy --nim-api-key "nvapi-..." --rpm 30

# Agent may omit the model; proxy fills in a default:
nim-proxy --nim-api-key "nvapi-..." --rpm 30 --model z-ai/glm-5.2

# Proxy decides for everyone, no matter what agents ask:
nim-proxy --nim-api-key "nvapi-..." --rpm 30 --force-model z-ai/glm-5.2
```

Rules and details:

- `--model` and `--force-model` are mutually exclusive; giving both exits 2
  before the server starts.
- The startup banner always states the active policy on its `Model:` line,
  so you can see at a glance who picks the model.
- The rewrite happens before anything else in the request lifecycle —
  metrics labels, usage accounting, streaming retries — so the whole
  pipeline sees the effective model consistently.
- Non-JSON and non-object bodies are never touched.
- The model id must match NVIDIA's catalog exactly (NIM does not fuzzy-match
  model names). Check with `curl http://127.0.0.1:8000/v1/models`.

**Which side should you choose?**

- Set the model in the **agent's config** when the agent supports model
  selection (all four agents in this guide do). This is the standard,
  per-agent choice — e.g. a coding agent on GLM-5.2 while a chat tool uses
  a smaller model through the same proxy.
- Use the **proxy-side default** (`--model`) when a consumer can't be
  configured with a model id and sends requests without one.
- Use the **pin** (`--force-model`) when you want one model for every
  consumer no matter what their configs say — e.g. locking a shared box to
  the one model your team standardized on.

---

## 5. CLI reference

```
nim-proxy --nim-api-key <KEY> [--rpm <RPM>] [--host <HOST>] [--port <PORT>]
          [--base-url <URL>] [--model <MODEL>] [--force-model <MODEL>]
```

| Flag | Default | Meaning |
|---|---|---|
| `--nim-api-key <KEY>` | *(required)* | NVIDIA NIM API key (`nvapi-…`). Applied upstream only; never logged anywhere. |
| `--rpm <RPM>` | `30` | Rolling-window requests-per-minute budget (per 60 s sliding window). Must be 1–10000. |
| `--host <HOST>` | `127.0.0.1` | Listen address. `0.0.0.0`/`::` binds all interfaces — the banner warns, use only on trusted networks. |
| `--port <PORT>` | `8000` | Listen port, 1–65535. |
| `--base-url <URL>` | `https://integrate.api.nvidia.com` | Upstream base URL (self-hosted NIM works too). |
| `--model <MODEL>` | *(none)* | Default model id injected when a request omits `model`. |
| `--force-model <MODEL>` | *(none)* | Model id every request is rewritten to. Mutually exclusive with `--model`. |
| `-h`, `--help` | | Print usage, exit 0. |
| `-V`, `--version` | | Print the version, exit 0. |

Flags accept `--flag value` or `--flag=value`. Duplicated flags are errors.
Everything is validated before the listener binds — bad invocations never
start a half-configured server.

**Endpoints served:**

| Path | Purpose |
|---|---|
| `/v1/*` | OpenAI-compatible API, proxied verbatim to NIM (chat completions, completions, embeddings, models, …) with the guarantees below. |
| `/health` | Liveness probe (`200 ok`). |
| `/metrics` | Prometheus exposition (`nimproxy_*` series). |

The local endpoint accepts **any bearer token** (or none): the real NVIDIA
credential only ever lives inside the proxy's upstream calls.

---

## 6. What the proxy guarantees

These properties are enforced by design and proven by the e2e test suite
(`cargo test`), which drives the real binary against a mock upstream:

- **Strict FIFO** — requests reach NVIDIA in exact arrival order; a later
  request never overtakes an earlier one.
- **Single-flight** — at most **one** NVIDIA request exists at any instant.
  If 100 requests arrive, 1 executes and 99 **wait** — they are never
  rejected with 503 for queuing.
- **Complete-response barrier** — the next request starts only after the
  previous one has *fully finished* upstream: complete buffered body, or
  complete SSE stream (not just the first chunk). Retries (429 with
  Retry-After, 500/502/503/504, network errors, interrupted streams) are
  part of the same request's lifecycle and can never be bypassed by a
  queued request.
- **Rolling-window RPM** — an exact sliding 60-second window (not a
  fixed-minute counter) with a 1-second delivery-jitter margin. Even with an
  instant upstream, NVIDIA never sees more than `--rpm` requests in any
  rolling minute.
- **Client-disconnect safety** — if an agent hangs up mid-stream, the
  abandoned upstream generation is drained to its real termination before
  the next request may start. The barrier always holds.
- **Streaming passthrough** — SSE streams flow through untouched; waiting
  clients receive `: heartbeat` comment lines so they don't time out while
  queued. `stream_options.include_usage` is injected on streamed requests
  (with automatic fallback) so usage accounting is exact.
- **Model policy** — pass-through by default; `--model` fills only missing
  values; `--force-model` rewrites every request (see section 4).

---

## 7. Agent setup guides

Every guide below assumes the proxy is running on the same host as the agent
(`http://127.0.0.1:8000/v1`). Adjust the URL if the proxy runs elsewhere.

Ready-made config files for all four agents live in [`examples/`](examples/)
in the repository and are attached to the release.

### OpenCode

[OpenCode](https://opencode.ai) (terminal coding agent) uses a JSON config
at `~/.config/opencode/opencode.json` (global) or `./opencode.json`
(project). Custom OpenAI-compatible providers are declared under
`provider.<id>` with the `@ai-sdk/openai-compatible` adapter; **the model
name is the key under `models`, and you select it as
`"<provider-id>/<model-id>"`** in the top-level `model` field:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "nim": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "NVIDIA NIM (via nim-proxy)",
      "options": {
        "baseURL": "http://127.0.0.1:8000/v1",
        "apiKey": "local",
        "timeout": false
      },
      "models": {
        "z-ai/glm-5.2": {
          "name": "GLM-5.2 (NIM)",
          "limit": { "context": 131072, "output": 32768 }
        }
      }
    }
  },
  "model": "nim/z-ai/glm-5.2",
  "small_model": "nim/z-ai/glm-5.2"
}
```

- `apiKey` can be any non-empty string (`"local"` is fine) — the proxy
  applies the real NVIDIA credential upstream.
- **Set `timeout: false`** — important. nim-proxy keeps the connection open
  with SSE heartbeats while it waits for rate-limit headroom; a client-side
  timeout would abort mid-wait and defeat the proxy's purpose.
- To list more models, copy the `models` entry per NIM id and reference
  `nim/<model-id>`.
- **Model name setup**: done — `"model": "nim/z-ai/glm-5.2"` selects it.
  Verify the id your account serves with
  `curl -s localhost:8000/v1/models | grep -i glm`.

Full annotated config: [`examples/opencode.json`](examples/opencode.json).
Environment-variable alternative for quick runs:

```sh
OPENAI_BASE_URL=http://127.0.0.1:8000/v1 OPENAI_API_KEY=local opencode
```

### Hermes Agent

[Hermes Agent](https://github.com/NousResearch/hermes-agent) (Nous Research)
works with any OpenAI-compatible endpoint. Configuration lives in
`~/.hermes/config.yaml`, credentials in `~/.hermes/.env`. **The model name
goes in the `model.default` field; a custom OpenAI-compatible endpoint is
`provider: "custom"` plus `base_url`:**

`~/.hermes/config.yaml`:

```yaml
model:
  default: "z-ai/glm-5.2"      # model id sent to the API (NVIDIA catalog id)
  provider: "custom"            # any OpenAI-compatible endpoint
  base_url: "http://127.0.0.1:8000/v1"
  api_key: "local"              # any non-empty value; the proxy holds the real key
  streaming: true               # nim-proxy streams and heartbeats while queued
```

`~/.hermes/.env` (alternative to `api_key` above):

```sh
OPENAI_API_KEY=local
```

- Switch models later with `hermes model` (setup wizard — pick "Custom
  endpoint"), `/model` inside a session, or the `--model` flag for a single
  run: `hermes --model z-ai/glm-5.2`.
- Hermes also has a native `nvidia` provider (direct to build.nvidia.com,
  `NVIDIA_API_KEY`) — using it would bypass the proxy's queueing and rate
  management. **Keep `provider: "custom"` pointed at nim-proxy** to get the
  FIFO/single-flight/RPM guarantees.
- Hermes auto-detects the context window from the provider's `/v1/models`;
  nim-proxy serves that catalog (cached), so no manual `context_length` is
  needed. If you do set one, GLM-5.2 on NIM's hosted endpoint is safest at
  128k.
- Named custom providers are also supported if you run several endpoints:
  under a top-level `providers:` block, each entry takes `base_url` and
  `api_key`/`key_env`.

### MiMo Code (Mimocode)

[MiMo Code](https://github.com/XiaomiMiMo/MiMo-Code) (Xiaomi; install with
`curl -fsSL https://mimo.xiaomi.com/install | bash` or
`npm install -g @mimo-ai/cli`, run as `mimo`) uses JSON/JSONC config at
`~/.config/mimocode/mimocode.jsonc` (global) or `.mimocode/mimocode.jsonc`
(project). Custom OpenAI-compatible providers go under
`provider.<id>` — **the model name is the key inside `models`, selected in
the top-level `model` as `"<provider-id>/<model-id>"`:**

`~/.config/mimocode/mimocode.jsonc`:

```jsonc
{
  "$schema": "https://mimo.xiaomi.com/mimocode/config.json",
  "model": "custom/z-ai/glm-5.2",
  "provider": {
    "custom": {
      "name": "NVIDIA NIM (via nim-proxy)",
      "npm": "@ai-sdk/openai-compatible",
      "only_configured_models": true,
      "models": {
        "z-ai/glm-5.2": {
          "name": "GLM-5.2 (NIM)"
        }
      },
      "options": {
        "baseURL": "http://127.0.0.1:8000/v1",
        "apiKey": "local"
      }
    }
  }
}
```

- Use exactly `baseURL` and `apiKey` in `options`. `apiKey` is any
  non-empty string — the proxy replaces it upstream.
- Model ids containing `/` are fine: only the **first** slash separates the
  provider id from the model id, so `custom/z-ai/glm-5.2` means provider
  `custom`, model `z-ai/glm-5.2`. The key under `models` must be the exact
  NVIDIA catalog id.
- Rename the provider block (`custom` → e.g. `nim`) if you already have a
  custom provider configured; keep the top-level `model` in sync
  (`"model": "nim/z-ai/glm-5.2"`).
- Verify with `mimo models` or the TUI model picker.
- During first launch you can also choose **Custom Provider** in the TUI and
  fill base URL / API key / model ID interactively — it writes the same
  config.

### Prime Agent

[Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent) (Prime
Intellect; install with
`curl -fsSL https://app.primeintellect.ai/prime-agent/install.sh | sh`)
adds custom providers via `~/.prime/agent/models.json`. **The model name is
the `id` of each entry in the `models` array** (that exact string is sent to
the API):

`~/.prime/agent/models.json`:

```json
{
  "providers": {
    "nim-proxy": {
      "baseUrl": "http://127.0.0.1:8000/v1",
      "api": "openai-completions",
      "apiKey": "local",
      "models": [
        {
          "id": "z-ai/glm-5.2",
          "name": "GLM-5.2 (NIM via nim-proxy)",
          "contextWindow": 131072,
          "maxTokens": 32768
        }
      ]
    }
  }
}
```

- `api: "openai-completions"` is the OpenAI Chat Completions wire format —
  exactly what nim-proxy speaks.
- `apiKey` can be a literal (`"local"`), an environment variable name, or a
  `"!command"` shell command; any non-empty value works against the proxy.
- Select the model per run with `prime-agent --model z-ai/glm-5.2`, or
  interactively with `/model` inside a session (the picker lists the ids you
  declared). The file reloads whenever you open `/model` — no restart needed.
- Set `compat.supportsReasoningEffort: false` at the model level if your NIM
  endpoint rejects `reasoning_effort`; set
  `compat.supportsDeveloperRole: false` if it rejects the `developer` role.
- `contextWindow`/`maxTokens` are optional (defaults 128k/16k); align them
  with the model's real NIM limits.

### Any other OpenAI-compatible consumer

Anything that lets you set a base URL + API key + model id works. The three
values, always:

| Setting | Value |
|---|---|
| Base URL | `http://127.0.0.1:8000/v1` |
| API key | any non-empty string (e.g. `local`) |
| Model | a NVIDIA catalog id, e.g. `z-ai/glm-5.2` |

Python (openai SDK):

```python
from openai import OpenAI

client = OpenAI(base_url="http://127.0.0.1:8000/v1", api_key="local")

resp = client.chat.completions.create(
    model="z-ai/glm-5.2",                 # <- the model name, set by the consumer
    messages=[{"role": "user", "content": "hello"}],
)
print(resp.choices[0].message.content)
```

curl:

```sh
curl -s http://127.0.0.1:8000/v1/chat/completions \
  -H "content-type: application/json" \
  -H "authorization: Bearer local" \
  -d '{"model": "z-ai/glm-5.2", "messages": [{"role": "user", "content": "hi"}]}'
```

Env-var style (works with many tools):

```sh
export OPENAI_BASE_URL=http://127.0.0.1:8000/v1
export OPENAI_API_KEY=local
```

---

## 8. Operations: metrics, health, shutdown

- **`/metrics`** (Prometheus): queue depth and wait times, single-flight
  slot occupancy (`nimproxy_singleflight_active` 0/1), gate depth, TTFT,
  tokens/s, upstream seconds, usage/token accounting — all labeled by model
  and path.
- **`/health`**: `200 ok` while serving. The binary also self-probes:
  `nim-proxy --health` connects to its own `/health` and exits 0/1 (used by
  the optional scratch container image).
- **SIGINT / SIGTERM**: stops accepting new work, drains in-flight requests
  (up to 30 seconds), exits cleanly. Requests still *queued* (not yet
  started upstream) are abandoned — the queue is in-memory by design and
  this is documented behavior, not data loss.
- A request waits in the FIFO queue for up to 15 minutes before answering
  504; under sustained load at 30 RPM a 100-deep queue drains in ~4 minutes.
  (Separate flood protection: beyond 512 concurrent *in-flight* local
  requests, new ones get 503 to bound memory — that cap is about local
  memory under a connection flood, not about queueing discipline.)

---

## 9. Security notes

- Binds to **127.0.0.1 by default**. The local endpoint accepts any bearer
  token (or none) — the real NVIDIA credential never leaves the proxy's
  upstream calls.
- `--host 0.0.0.0` exposes the endpoint to the network: anyone who can
  reach it can spend your NVIDIA quota. The banner warns; use it only on
  trusted networks (e.g. a host-internal bridge between containers) and put
  TLS in front if it must be remote.
- The NVIDIA key is never written to stdout, stderr, logs, metrics, error
  bodies, or panic messages.

---

## 10. Troubleshooting

**A 404 / "model not found" from NVIDIA.** The model id in the request
doesn't match the catalog exactly. List what your key actually serves:
`curl -s http://127.0.0.1:8000/v1/models`. With `--force-model`, the pinned
id must be one of those. With pass-through/default, fix the agent's model
id (see each agent's section above for where it lives).

**Agent times out while the proxy queues.** Disable the agent's client-side
request timeout (OpenCode: `"timeout": false`; other tools: raise or disable
their HTTP timeout). The proxy heartbeats waiting streams precisely so
clients don't need to time out; a short client timeout aborts the wait and
defeats the queueing.

**Requests seem slow under load.** That is the design working: one upstream
request at a time, strict FIFO, 30 RPM. 60 requests arrive → ~2 minutes to
drain. Raise `--rpm` only within what your NVIDIA tier actually allows.

**`--model` and `--force-model` together?** They're mutually exclusive —
pick one; the tool exits 2 with an explanation.

**Port already in use.** `nim-proxy cannot start: cannot listen on
<host>:<port>…` — pass a different `--port` or stop the other listener. The
error appears before any server starts.

**Running in Docker and nothing can connect.** The default `--host 127.0.0.1`
binds the container's own loopback, which a published port (`-p 8000:8000`)
cannot reach (Docker routes through the container's eth0). Start the
container with `--host 0.0.0.0` — and treat the resulting exposure
seriously: put TLS or a network policy in front if the port is reachable
beyond your own host.

**Where do logs go?** stderr. Redirect them: `nim-proxy … 2>>proxy.log`.
stdout has exactly one payload: the startup banner with the endpoint URL.
