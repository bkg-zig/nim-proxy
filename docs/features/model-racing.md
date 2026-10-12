# Model racing

**Development status (2026-10-12):** this page describes the current multi-model
implementation. The owner's clarified requirement is to race several attempts
of the same client-selected model. That behavior is not implemented; see the
[active plan](../plans/model-racing-implementation.md) for the correction and
pending decisions.

Racing is disabled by default. When enabled by an administrator, each
`POST /v1/chat/completions` request uses the configured candidate models in
place of the client's `model` field. Other endpoints keep their normal routing.
Choose models that support the same request features and acceptable answer
quality. Racing chooses by response time; it does not compare answer quality.

## Configure

Use an authenticated admin or superuser session for all four operations:

| Operation | Purpose |
|---|---|
| `GET /api/racing/config` | Read `{enabled, strategy}` |
| `POST /api/racing/config` | Validate, persist and publish a policy |
| `GET /api/racing/stats` | Read observed model statistics |
| `GET /api/racing/ranked` | Read models with enough measured TTFT samples |

For example, send this JSON to `POST /api/racing/config`:

```json
{
  "enabled": true,
  "strategy": {
    "type": "smart",
    "primary_model": "publisher/primary-model",
    "fallback_models": ["publisher/fallback-model"],
    "primary_timeout_ms": 1500
  }
}
```

The example identifiers are placeholders; use models available at your
configured upstream. To disable, send `{"enabled":false}`. A successful save
returns `{"ok":true}` and survives restart. Invalid policies and unauthorized
writes do not change either the active policy or `config.json`.

Policies allow one to five unique model IDs in total. IDs contain 1–128 ASCII
letters, digits, `.`, `_`, `-` or `/`. A smart timeout is 1–300000 ms. Racing and
`strict_passthrough` are mutually exclusive; both racing and general settings
writes enforce this. No Dashboard controls or per-request override headers are
provided by this implementation. See the generated [OpenAPI](../../openapi.json)
for the exact request and response schemas.

## Selection and cancellation

| Strategy | Behavior |
|---|---|
| `full_parallel` with `models` | Start all candidates; choose the first successful response |
| `smart` | Try the primary; on failure or timeout, cancel it and race fallbacks |
| `ttft_racing` with `models` | Start all candidates; streamed requests choose the first meaningful delta |

Buffered requests race until the full successful HTTP response body has been
read. All streaming strategies select on the first complete SSE event carrying
nonempty content, reasoning content, or tool/function-call data. Comments,
role-only/empty deltas and errors before the first meaningful event cannot win. The winner's buffered
prefix and subsequent stream bytes are relayed unchanged. Pre-token prefixes
are bounded to 64 KiB through the winning event; later bytes in the same read
do not count against that prefix bound. LF, CRLF and CR framing are accepted.
A candidate ending before any meaningful delta fails.
If every candidate fails, buffered requests receive HTTP 502 `race_failed`;
streams receive the same code in a terminal SSE error after HTTP 200.

Every attempt goes through the existing model governor and per-key FIFO RPM
dispatcher, including retries and usage-injection fallback. A sent request
uses its RPM slot even if cancelled. Racing therefore increases request
consumption, and capacity limits can serialize candidate admission. Losing
HTTP futures and permits are dropped when a winner is selected. Smart mode
starts its timeout before primary admission, so a busy queue may trigger its
fallback. Each request uses one immutable configuration snapshot.

`X-Nim-Proxy-Deadline-Ms` bounds admission, selection and the complete winning
response. Streaming disconnection cancels selection or relay immediately.
Buffered callers should use the deadline header when they need a wall-clock
bound; immediate buffered disconnect detection remains a transport limitation.
The winner continues to use the existing buffered request timeout or stream
idle timeout.

## Observations and limits

Wins and meaningful-delta TTFT are exposed via
`nimproxy_race_model_wins_total` and `nimproxy_race_ttft_seconds`.
`nimproxy_race_requests_total` uses strategy labels `full_parallel`, `smart`,
and `ttft`. `nimproxy_race_cancelled_total` counts pending contenders dropped
at winner selection or primary timeout; it is not a count of upstream billing
refunds. Winner cancellation preserves `winner_found` for parallel/smart and
`ttft_winner` for TTFT; primary timeout uses `primary_timeout`.
Smart hit/fallback counters explain policy decisions.

Performance statistics are bounded to 256 model identities and reset on
restart. Completed winners and definitively failed candidates contribute;
cancelled work is excluded rather than treated as a failed model response.
An already-selected stream still relays later upstream errors verbatim, but
errors and incomplete EOF do not produce successful observations. A successful
stream sample requires complete framing plus `[DONE]` or a terminal finish
event. Buffered completions do not produce
TTFT samples. Rankings require three successful measured TTFT samples and the
fastest-model recommendation requires five. These observations are biased
toward race winners and do not measure every candidate's full latency or
provide a controlled model benchmark.

No measured production speedup is claimed. Local mock regressions establish
selection, cancellation, permissions and rate-budget behavior; they do not
establish a percentage latency improvement on NVIDIA's hosted service.
