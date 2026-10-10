# Plan — Model Racing Implementation

**Status:** Repair integration and final verification, 2026-10-10 (Asia/Shanghai).

## Current repair contract (supersedes the draft below)

The owner requested repair after the handoff audit reproduced eight compile
errors and found placeholder upstream functions, disconnected racing, ordered
winner selection, detached loser tasks, and unvalidated volatile configuration.
Integration branch: `feature/model-racing`. Work item:
`codex/model-racing-repair`, isolated in `.worktrees/model-racing-repair`.

- **Outcome:** deliver the existing opt-in full-parallel, smart fallback and
  streaming TTFT strategies through `POST /v1/chat/completions`, with durable
  admin-managed configuration and truthful race observations.
- **Proof:** preserve the compiler failure as the build regression; commit
  behavioral tests before implementation and observe red then green using
  `cargo test --test racing` and `cargo test --test racing_config`. Then run
  `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`,
  generated OpenAPI and route-contract tests, documentation/link checks and
  `git diff --check`, following the test strategy. All HTTP tests use synthetic
  stores and a local mock; no production key or NIM request is needed.
- **Constraint:** disabled requests keep their existing route and wire behavior;
  each participant uses the existing dispatcher and model governor; cancellation
  cannot refund an already-sent RPM slot. Preserve absolute deadline and stream
  disconnect ownership. Keep main, the running service, the sibling localization
  branch, stash and untracked reports unchanged.
- **Ponytail Rung:** 2/5: reuse settings::commit, reqwest, FuturesUnordered,
  dispatcher/governor and the existing test harness, without new dependencies.
- **Act:** compile-only repairs first, behavioral red checks next, implementation
  and independent review before final commit. Root owns integration and proof;
  delegates own control-plane, reusable foundation and independent E2E tests.

Scope delta shown to the owner before implementation: complete the disconnected
feature through the existing chat endpoint and its existing racing management
API. No new `/v1/race` route, per-request race override, Dashboard controls,
quality-based model selection, live benchmarking, deployment or localization
merge is part of this repair. Configuration is disabled by default, limited to
five unique models, and serialized as an optional store field (absent when off).
For streaming, all enabled strategies select the first valid content, reasoning
or tool-call delta; comments, empty deltas and terminal errors cannot win.
Smart fallback cancels the timed-out primary before starting fallback contenders.

### Fresh proof record

- Audit at `41d77d5`: `cargo test --locked --offline` failed compilation with
  eight errors and four warnings; `cargo fmt --check` failed. No Rust test ran.
  Locale, i18n, source syntax/assets and agent-guide guards passed. Historical
  reports of speed gains are not performance acceptance evidence.
- Regression commits `bd76fa0` and `df21970` preserve the original behavioral
  failures. Continuation initially passed 221 library tests, 15 racing HTTP
  tests, 6 racing configuration tests and 119 existing E2E tests (one existing
  release-only test ignored).
- Independent review identified SSE framing and post-selection observation
  defects. The expanded HTTP suite reproduced seven failing tests (16 passed):
  CR-only framing, no-space `event:error`, successful classification after a
  winner's error, the pre-token byte bound counting post-token bytes, and three
  checks preserving the historical `ttft_winner` cancellation label. Root will
  repair these before the final complete verification. This stays within the
  original truthful-observation and streaming contracts.
- Independent source review: foundation/client/measurement repair approved by
  the foundation reviewer (8 performance and 3 client tests passed). The same
  independent reviewer approved the config validation, serialized publication,
  authorization ordering and typed API contract; final data-plane review is
  pending the SSE fixes.

## Authority and Scope

Implementation of model racing/tournament functionality to allow concurrent requests to multiple models with first-response-wins semantics. This addresses the use case mentioned in Rambler's model tournament where comparative model performance evaluation is needed.

## Background

The zh-CN handoff plan noted: "Model-racing disposition: defer implementation. The inherited research reports additional per-key request consumption and a model-routing contract change; it supplies no measured benefit for this deployment."

This branch will explore the design and implementation of model racing as a separate, opt-in feature.

## Use Cases

1. **Model Tournament**: Send the same prompt to multiple models simultaneously, return the first successful response
2. **Performance Benchmarking**: Compare response times and quality across models
3. **Fallback Strategy**: Primary model with automatic fallback to alternatives on timeout
4. **Quality Comparison**: A/B testing for model selection decisions

## Design Considerations

### Key Questions to Answer

1. **Request Routing**: How to fan out one client request to multiple upstream models?
   - New `/v1/race` endpoint vs extending existing `/v1/chat/completions`?
   - Client-specified model list vs server-configured tournaments?

2. **Response Strategy**: Which response to return?
   - First successful response (speed)
   - Best quality (requires evaluation criteria)
   - All responses (for comparison)

3. **Resource Management**: How to handle multiple concurrent requests?
   - Impact on rate limits (multiplies key consumption)
   - Governor behavior with racing requests
   - Cancellation of slower responses after winner

4. **Metrics and Observability**: What to track?
   - Per-model latency in race
   - Win/loss statistics
   - Resource consumption
   - Quality metrics if applicable

5. **Streaming Support**: How to handle streaming responses?
   - First token wins?
   - Full response comparison?
   - Mixed streaming/buffered?

### Architecture Options

#### Option A: Separate Race Endpoint
```
POST /v1/race
{
  "models": ["model-a", "model-b", "model-c"],
  "strategy": "first-wins",
  "messages": [...]
}
```

**Pros:**
- Clean separation of concerns
- Explicit opt-in
- Can have race-specific configuration

**Cons:**
- Not OpenAI-compatible
- Clients need special support

#### Option B: Header-Based Racing
```
POST /v1/chat/completions
X-Nim-Proxy-Race: model-a,model-b,model-c
X-Nim-Proxy-Race-Strategy: first-wins
{
  "model": "model-a",  // primary/fallback
  "messages": [...]
}
```

**Pros:**
- Backward compatible
- Minimal client changes
- Works with existing tools

**Cons:**
- Mixing concerns in existing endpoint
- Header parsing complexity

#### Option C: Internal Tournament Mode
Server-side configuration for automatic racing without client awareness.

**Pros:**
- Zero client changes
- Centralized policy

**Cons:**
- Hidden behavior
- Resource consumption not visible to client

### Cost and Trade-offs

**Resource Consumption:**
- Racing multiplies upstream requests (3 models = 3x requests)
- Rate limit consumption increases proportionally
- More key slots occupied during race

**Benefits:**
- Reduced perceived latency (first wins)
- Fallback resilience
- Performance data for model selection
- Quality comparison data

## Proposed Initial Design

### Phase 1: Foundation (this branch)

1. **New Race Endpoint**
   - `POST /v1/race` with OpenAI-compatible body + race config
   - Support buffered responses initially (streaming in Phase 2)
   - Strategy: `first-wins` (return first successful response)

2. **Request Fan-out**
   - Spawn parallel upstream requests
   - Use existing dispatcher/governor for each
   - Cancel remaining requests when first succeeds

3. **Metrics**
   - `nimproxy_race_requests_total{strategy}`
   - `nimproxy_race_model_wins_total{model}`
   - `nimproxy_race_latency_seconds{model,position}` (winner, 2nd, 3rd, etc.)
   - `nimproxy_race_cancelled_total{model,reason}`

4. **Configuration**
   - Opt-in feature flag
   - Maximum models per race (default 5)
   - Timeout for race (use existing deadline mechanism)

### Phase 2: Advanced Features (future)

- Streaming support
- Quality-based selection (requires response evaluation)
- Server-side tournament configuration
- Statistical winner selection (e.g., best 2 of 3)

## Open Questions

1. **Request Body**: Should all models see identical request bodies, or can we vary sampling params?
2. **Error Handling**: If all models fail, which error to return?
3. **Observability**: How detailed should race metrics be? Store all responses?
4. **Cost Control**: Should there be per-user race limits?
5. **Upstream Compatibility**: Any NIM-specific considerations?

## Success Criteria

- [ ] Race endpoint accepts request and fans out to N models
- [ ] First successful response returned to client
- [ ] Slower responses cancelled properly
- [ ] Metrics track wins/losses/latencies
- [ ] Resource consumption (keys, slots) properly managed
- [ ] E2E tests cover racing scenarios
- [ ] Documentation explains feature and trade-offs

## Deferred / Out of Scope

- Streaming race responses
- Quality-based selection
- Automatic tournament mode
- Response caching/deduplication
- Cost optimization strategies

## Related Decisions

- [explicit-request-deadline](../../knowledge/decisions/explicit-request-deadline.md) - Can reuse deadline mechanism
- [governor](../../knowledge/architecture/governor.md) - Need to understand interaction with model limits
- [key-pool](../../knowledge/architecture/key-pool.md) - Race multiplies key consumption

## Implementation Tasks

- [ ] Design API contract (request/response schemas)
- [ ] Implement race handler with fan-out logic
- [ ] Add cancellation for slower responses
- [ ] Implement metrics collection
- [ ] Add E2E tests
- [ ] Document feature in README and API docs
- [ ] Consider OpenAPI schema updates
