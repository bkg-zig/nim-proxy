# Plan — Model Racing Implementation

**Status:** Same-model request racing implementation in progress,
2026-10-12 (Asia/Shanghai). The earlier multi-model repair remains historical
implementation evidence, not acceptance of the corrected requirement.

## Execution contract — 2026-10-12

The owner authorized implementing this corrected plan. The work item is
`codex/same-model-hedging-fix`, based on the integration branch
`feature/model-racing` through the unmerged repair and contract dependencies
`c169089` and `d338974`. Main and the primary checkout remain untouched.

- **Outcome:** preserve the client-selected model and request semantics in
  every contender, choose the first effective response, and cancel all losers.
- **Proof:** per the [test strategy](../../knowledge/testing/test-strategy.md),
  commit reviewed failing HTTP/config regressions, then run `cargo test --test
  racing --test racing_config`, the complete Rust suite, Clippy with warnings
  denied, `cargo fmt --check`, generated OpenAPI/route/field-order guards,
  agent-guide selftest/current links and `git diff --check`. Run the strict
  enforcing mock load harness with racing enabled. All keys and traffic are
  synthetic; no live NIM or production service mutation is required.
- **Constraint:** each send, including retries, uses the shared model governor
  and FIFO RPM dispatcher; sent reservations are never refunded. Disabled
  routing, authentication, deadlines, stream ownership and machine identifiers
  retain their contracts. No new dependencies, UI, deployment or quality ranking.
- **Ponytail Rung:** 2/5 — reuse owned futures, serde, the existing settings
  transaction, pool/dispatcher, bounded SSE gate and HTTP mock harness.
- **Act:** implement and verify small red-to-green slices; obtain independent
  review before each substantive commit and publish a reviewable work-item PR
  against `feature/model-racing` after local verification.

Scope decisions shown to the owner before implementation:

1. New explicit policy: `{"type":"same_model","attempts":2,
   "hedge_delay_ms":0}`. Attempts must be 1–5; delay must be 0–300000 ms.
   Zero starts all attempts; a positive delay starts the first immediately and
   each subsequent copy after the interval from the preceding launch. If all
   launched contenders fail, launch the next immediately. Pending originals
   remain eligible. A winner prevents any unlaunched copies from starting.
2. First attempt retains conversation affinity; additional copies prefer
   different currently available keys. Diversity is best effort: one-key or
   saturated pools may reuse a ready key without blocking FIFO for diversity.
   Key diversity does not imply independent NIM workers or measured speedup.
3. Buffered success requires HTTP success, valid completion JSON with no error,
   nonempty choices, assistant messages carrying content/reasoning/tool/refusal
   output, and nonempty terminal finish reasons. Streaming keeps the existing
   bounded first-meaningful-content/reasoning/tool-event predicate. Once winner
   bytes are emitted, another generation is never spliced in.
4. This intentionally retires the old multi-model policy types. Old policies
   remain parseable for an actionable validation error, but cannot be enabled
   through the API or loaded at startup. Operators explicitly replace or remove
   the optional `racing` field; no store is silently migrated or rewritten.
   Existing route names, envelopes, config version and metric names stay fixed;
   the new strategy label is `same_model`, with cancellation `winner_found`.
   The compatibility decision will be reflected in OpenAPI and release notes.

Verification environment: Windows host has no Cargo. Use the existing isolated
Rust verification container with a dedicated copy of this worktree and cached
build dependencies. Container test processes use synthetic temporary stores.

### Implementation proof log

- Fresh baseline `cargo test --locked --offline --test racing_config --
  --test-threads=4`: 6 passed. Key-diversity red check `cargo test --locked
  --offline --lib racing_ -- --test-threads=4`: 7 ran, 5 failed and 2 passed.
  Actual failures prove affinity reused `key0`, ignored key identity after
  rebuild, and did not record granted group keys. Root independently reviewed
  the delegate's tests/stubs before the red commit; no selection semantics
  were implemented in those stubs.
- Configuration red check `cargo test --locked --offline --test racing_config
  -- --test-threads=4`: 8 ran, 6 failed and 2 passed. The old build accepted
  legacy/mixed policies, started with a legacy store, and did not recognize
  `same_model`. Root independently reviewed the delegated HTTP assertions and
  retained exact save-failure boundary checks before the regression commit.

## Current scope — same-model request racing

The owner clarified the intended behavior on 2026-10-12:
“同一个模型发起多个请求，采用最先有效响应的结果”. Preserve the
client-selected model and race request attempts for that model. Cross-model
replacement, model tournaments and quality ranking do not satisfy this goal.

Integration branch remains `feature/model-racing`. The audit work item is
`codex/same-model-hedging-contract`, in an isolated managed worktree. It
continues from the unmerged repair dependency `c169089`, a descendant of the
integration branch. Both `codex/model-racing-repair` and `codex/model-hedging`
pointed at that same commit during this audit; the latter name introduces no
behavioral change. This documentation item does not integrate that dependency.

- **Outcome:** record the corrected requirement, the implementation mismatch,
  reusable mechanisms and pending acceptance criteria before behavioral work.
- **Proof:** follow the [test strategy](../../knowledge/testing/test-strategy.md).
  For this documentation edit, run `python -X utf8 scripts/check_agent_guide.py
  --selftest`, `python -X utf8 scripts/check_agent_guide.py`, explicitly apply
  its `local_link_problems` helper to this plan, run `git diff --check`, and
  obtain independent review. Rerun the existing racing suites only to establish
  the present implementation; they cannot prove the corrected requirement.
- **Constraint:** preserve historical evidence and current executable behavior.
  Every future attempt must keep the requested model and request semantics,
  obey per-key RPM and the shared per-model governor, and release losers without
  refunding already-sent requests. Existing API, store and metric contracts
  require an explicit compatibility decision before change.
- **Ponytail Rung:** 2/5: reuse the dispatcher, governor, owned futures, bounded
  SSE gate, deadlines, settings transaction and local mock harness.
- **Act:** publish this scope delta locally, validate the documentation and
  record independent review. Behavioral implementation remains future work.

### Findings from the current implementation

- `RaceRequest::attempt_inner` in [proxy.rs](../../src/proxy.rs) overwrites
  `request["model"]` with each configured candidate. Enabled racing therefore
  does not preserve the client-selected model.
- [racing_config.rs](../../src/racing_config.rs) requires unique model IDs;
  [configuration tests](../../tests/racing_config.rs) explicitly reject repeated
  IDs and a primary repeated as fallback. Repeating one model in the current
  configuration is not a workaround.
- `Smart` in [racing.rs](../../src/racing.rs) cancels the primary before starting
  fallbacks. If delayed hedging is chosen, the original pending attempt must
  remain eligible while another copy starts. The launch policy is not yet chosen.
- Conversation affinity currently gives identical model/messages the same
  preferred key. Removing uniqueness alone would not guarantee separate keys.
  Lane diversity needs an explicit policy compatible with FIFO admission and
  [affinity](../../knowledge/decisions/sticky-affinity-with-spillover.md).
- The streamed gate already waits for meaningful content, reasoning or tool
  data. Buffered candidates currently require HTTP success and a fully read
  body, without validating a successful completion JSON payload. Define
  “effective response” explicitly for both transports before implementation.

### Acceptance work still required

- [ ] Define attempt count, immediate versus delayed launch, and whether spare
  attempts require distinct available keys. Do not assume separate keys mean
  independent NIM model workers or guaranteed lower latency.
- [ ] Keep the original model, messages, tools and sampling parameters in every
  attempt, subject only to already-defined proxy transformations. Use attempt
  identity rather than model identity to distinguish participants.
- [ ] Specify the buffered success predicate and streamed first-effective-event
  predicate, including reasoning/tool deltas and malformed/error responses.
  After winner bytes are emitted, do not splice in a different generation.
- [ ] Commit red regressions proving identical model/payload across attempts,
  correct winner selection, loser cancellation, shared governor/RPM admission,
  deadline/disconnect cleanup and disabled-path preservation; then make them
  green with the minimum change. If delayed launch is selected, prove the
  original can still win after the extra attempt starts.
- [ ] Decide compatibility for the existing racing API, durable policies,
  OpenAPI and metrics; document any intentional breaking change with regression
  evidence and a release note. Do not silently reinterpret a multi-model store.
- [ ] Independently review and run the relevant tests and strict mock load proof
  from the test strategy. Live NIM speedup remains unmeasured.

### Audit evidence and delivery state

Fresh execution on 2026-10-12 against `c169089` in the existing isolated Rust
verification container: `cargo test --locked --offline --test racing --test
racing_config -- --test-threads=4` passed 23 racing tests and 6 configuration
tests, with no failures or ignored tests in those suites. The duplicate-model
rejection remains a passing assertion under the old contract.

The primary checkout still points at `feature/model-racing` / `41d77d5`;
the repaired code lives in `.worktrees/model-racing-repair` at `c169089`.
Fresh `cargo check --locked --offline` in the primary checkout failed with
8 compile errors and 4 warnings. The compiler reports type mismatches, missing
HTTP client API support, unsupported `Instant` serialization/deserialization
and a non-`Debug` stream. This confirms the primary checkout is not the repaired
implementation validated by the racing suites above.
The repair worktree was clean at audit time. The fork PR list returned no PRs.
No deployment or production request was performed by this audit. The full Rust
suite, Clippy, release CI and strict load test were not rerun for this
documentation-only correction; earlier records below retain their original scope.

Documentation proof passed: all 12 guide selftest fixtures demonstrated their
named failure, the guide/current-link check passed, this plan's local links
passed the existing helper, and `git diff --check` passed. An independent agent
reviewed all five changed Markdown files and approved the requirement/current-
behavior distinction and the explicitly pending implementation decisions.

## Historical repair contract — 2026-10-10

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
  checks preserving the historical `ttft_winner` cancellation label. Commit
  `945f256` preserves those regression cases. All seven failures are repaired.
  This stays within the
  original truthful-observation and streaming contracts.
- Independent source review: foundation/client/measurement repair approved by
  the foundation reviewer (8 performance and 3 client tests passed). The same
  independent reviewer approved the config validation, serialized publication,
  authorization ordering and typed API contract. Final data-plane re-review
  approved the SSE fixes and found no remaining blockers.
- Final exact working-tree proof in the isolated Rust container:
  `cargo test --locked --offline -- --test-threads=4` passed **378 tests**:
  221 library, 119 existing E2E, 9 OpenAPI, 23 racing, 6 racing-config.
  The existing `release_restart_and_idle_history` release-only test remains
  ignored (one skipped). No test failed.
- `cargo clippy --locked --offline --all-targets -- -D warnings` and
  `cargo fmt --check` passed. Agent-guide selftest passed all 12 negative
  controls; current links, both locale catalogs, 452 i18n references and
  `git diff --check` passed. Windows locale checks ran with Python's `-X utf8`
  because the host's default GBK decoder cannot read the UTF-8 catalogs.
- The resulting 21-operation OpenAPI file matches code generation. Operator
  documentation and architecture memory explain opt-in model replacement,
  increased RPM use, cancellation, sample limitations and configuration.
  Historical draft speedup/HTTP protocol claims are explicitly withdrawn as
  acceptance evidence.

### Completed scope and delivery boundaries

- [x] Compile and connect all three strategies to chat POSTs.
- [x] Preserve default routing, limiter/governor, cancellation and deadlines.
- [x] Persist/admin-gate/validate configuration and reject strict conflicts.
- [x] Count actual TTFT samples; reject erroneous/incomplete stream successes.
- [x] Independently review and pass the complete local proof set above.
- [x] Update generated API contract, usage docs, knowledge and release notes.

Delivery remains on `codex/model-racing-repair` for review against
`feature/model-racing`. No production deployment, NIM performance benchmark,
localization merge, remote push or PR publication was performed. Release CI
coverage/MSRV/security jobs were not run locally; they remain publication
gates. The running service, main branch, sibling localization work, stash and
original untracked reports are outside this repair.

## Archived initial design (superseded by the repair contract above)

The following draft preserves design provenance. Its proposed endpoint,
deferred streaming scope, and unchecked draft tasks are not current work.

### Authority and Scope

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
