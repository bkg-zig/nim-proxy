---
type: Component
title: Opt-in model racing
description: Administrator-selected chat models share normal admission and retries; owned futures select and cancel contenders.
tags: [racing, routing, cancellation, streaming]
timestamp: 2026-10-10T00:00:00Z
---

# Opt-in model racing

An optional config-v1 `racing` policy is validated by `config::validate`,
persisted through `settings::commit`, and captured with every request's
immutable `Config` snapshot. Absence disables it. All racing APIs require an
admin/superuser; enabling it conflicts with strict passthrough. This is an
intentional routing change: chat POSTs replace the requested model with up to
five explicitly configured models. See the [operator guide](../../docs/features/model-racing.md).

`proxy::RaceRequest` owns candidate I/O. `racing::select` owns only selection:
`FuturesUnordered` takes the first successful completion, and dropping the
collection cancels losers instead of detaching tasks. Smart selection times
out and drops the primary before creating fallback candidates. Each candidate
uses the [governor](governor.md), [dispatcher](dispatcher.md), and
[key pool](key-pool.md); retries and usage-injection fallback consume new RPM
slots. Already-sent slots cannot be returned after a race is cancelled.

Buffered selection waits for a successful full body. Streamed selection waits
for a meaningful complete SSE event while withholding candidate bytes; only
the winning prefix and stream are released to the client. Comments and empty
deltas are not TTFT. Prefix memory is bounded. The single streaming task owns
the client in-flight guard, winning permit and observation finalizer, and
races disconnect and the [absolute deadline](../decisions/explicit-request-deadline.md).

Statistics record definitive candidate failures and completed winners, never
count cancelled work as failed models or buffered latency as TTFT. The bounded
SSE classifier remains with the selected winner through relay, so later error
events and incomplete EOF cannot become successful samples. Raw relay bytes
remain unchanged, including a post-selection provider error.
Missing measurements are excluded from rankings; non-finite TPS cannot poison
the moving average. Model statistics are bounded and process-local, and winner
sampling is not a controlled performance comparison.

`tests/racing.rs`, `tests/racing_config.rs`, performance/client unit tests,
route inventory and generated OpenAPI provide regression evidence. The
[repair plan](../../docs/plans/model-racing-implementation.md) records fresh
test results and scope; no live NIM latency percentage is established.
