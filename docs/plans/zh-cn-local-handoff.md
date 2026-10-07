# Plan — local zh-CN handoff closure

**Status:** in progress, 2026-10-07 (Asia/Shanghai).

## Authority and scope

The owner asked Codex to continue the interrupted localization handoff, first
preserving the existing uncommitted work and then completing the missing local
checks. This is a local customization of the v0.6.6 checkout at `330efc4`, not
the upstream v0.6.7 locale-pack delivery described in the
[stabilization design](v0.6.6-v0.6.7-stabilization-design.md).

The historical integration branch is `release/v0.6.6`; this shallow checkout
contains only the merged release on `main`. The owner's handoff specifically
requires staying in the existing checkout rather than making another worktree.
Use local branch `codex/zh-cn-handoff` from `330efc4`; leave the `main` ref
unchanged. No push, upstream PR, tag, release, or locale-tag migration is
authorized by this closure. An upstream contribution remains an owner decision
and would first require resolving the current integration branch and the
upstream `zh-Hans` naming requirement.

Do not change or restart the existing `nim-proxy` container or access its data
directory. Dedicated test containers use synthetic configuration and mock data.
Model racing remains a separate research item and is not implemented here.

## Outcome / Proof / Constraint / Ponytail Rung

- **Outcome:** preserve the inherited zh-CN catalog and language switch in local
  commits, then provide fresh evidence for the previously skipped checks.
- **Proof:** use the commands routed by the
  [test strategy](../../knowledge/testing/test-strategy.md): locale/i18n and
  pseudolocale guards, JavaScript source/self-test modes, agent-guide link
  validation, `git diff --check`, `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test`, served browser
  `--all-states`, `--all-states --pseudolocale`, `--visual-matrix`, and
  `--startup-probe locale-precedence`. Record exact executed commands/results
  and any unavailable checks below. Layout requires an explicit human review;
  generated screenshots and agent inspection cannot supply that approval.
- **Constraint:** preserve existing wire shapes, ids, contextual sinks, auth,
  pacing, history, deployment, and local `zh-CN` preferences. Historical release
  evidence is provenance, not fresh proof of this customization.
- **Ponytail Rung:** 2/4 — reuse the existing validators, Rust tests, CDP render
  gate, Docker runtime, and native browser. Add only a missing reusable proof
  directly needed by the inherited behavior.

## Scope deltas surfaced before implementation

- The inherited changes have no local active plan or durable decision explaining
  `zh-CN` versus upstream `zh-Hans`. Record the local scope and reconcile current
  README/presentation-memory claims while retaining historical release records.
- The new sidebar action has no dedicated committed behavior regression. Review
  and browser execution will establish the precise missing proof and any actual
  defect before changing its implementation.
- Fresh inspection found `locale_v1.py --all` checks only English/public content
  and the other production filenames. Extend the existing guard to inspect every
  production catalog, first proving a malformed translation escapes the old
  check. Explicit validation of `zh-CN.json` already passes independently.

## Act → Verify record

- 2026-10-07: read the handoff, governing design and presentation plans, semantic
  index, locale/sink/formatting/render/testing concepts and relevant chronology.
  Verified the inherited file inventory, shallow base, and running container's
  healthy status. Created `codex/zh-cn-handoff` in the existing directory under
  the owner's explicit directory constraint.
- Independent read-only review and isolated Rust/browser environment preparation
  are in progress. Previous-session counts are not accepted as fresh results.
- Fresh checkpoint proof: explicit `locale_v1.py src/web/locales/zh-CN.json`
  passed; i18n reports 452 referenced ids and 121 passing self-test cases;
  locale self-test reports 19 passing cases; generated pseudolocale has 452
  messages. Syntax, assets, source/provenance startup self-tests, agent-guide
  validation (12 self-test fixtures), and diff whitespace checks passed.
- Independent agent `review_localization` reviewed the inherited code diff,
  full paired catalogs, API/sink/CSS behavior and documentation. It found no
  confirmed runtime/security/translation blocker to a preservation checkpoint,
  but confirmed the locale-validator gap and missing browser proof. The
  checkpoint is preservation only, not completed acceptance.
- Browser review also found the existing `--all-states --pseudolocale` command
  does not propagate its locale into the 72-row interaction matrix. That matrix
  therefore remains English despite the earlier pseudolocale page run. Do not
  claim complete translated-state coverage. The gate currently rejects
  `--locale zh-CN`; a focused served-catalog/switch proof is needed.

## Remaining work

- [x] Independently review and commit the inherited localization (`a16f044`).
- [x] Reconcile the local decision and current documentation.
- [ ] Add/observe missing regression proof where needed, with red → green evidence.
- [ ] Complete and record missing Rust and browser checks.
- [ ] Retain screenshots for the owner's visual inspection.
- [ ] Report commits, fresh checks, limitations, and the upstream PR decision.

## Validator repair proof

`locale_validator_fix` authored the narrow guard repair; the parent agent
independently reviewed its diff and reran the checks. The added controls run the
actual `main(["--all"], temporary_root)` with synthetic files. Before repair,
six corrupted repositories all exited 0 without a diagnostic. After repair,
missing-id, placeholder, stale-hash, and three metadata-mismatch cases exit 1
with exactly `completeness`, `placeholders`, `stale`, or `catalog-schema`.
`--selftest` now passes 26 cases and `--all` reports two checked catalog sets.
The i18n 452-id/121-self-test, pseudolocale 452-message, formatter 125-case,
72-row/284-item interaction self-test and local-link guards also passed.

## Pending upstream bug report (not submitted)

No GitHub write is authorized in this local closure. The existing bug form's
fields are retained here for the discovered pre-existing matrix gap:

- **What happened?** `--all-states --pseudolocale` selects a generated locale
  for the initial page run but does not pass it into the full interaction
  matrix. Earlier documentation overstates translated Settings coverage.
- **Steps to reproduce:** follow `localeArg` into the call to
  `runInteractionMatrix`, then its call to `runMatrixContext`; `visualLocale`
  remains its default `null`. Run the command and inspect matrix catalog
  requests to confirm English. The code path was independently inspected;
  runtime evidence is not yet claimed for this report.
- **Auth mode:** Not sure (the form's legacy options do not describe the
  synthetic configured-store session used by the test harness).
- **How are you running it?** Docker / docker compose (isolated test container).
- **Version or commit:** inherited from v0.6.6 base `330efc4`.
- **Client / harness:** `node scripts/render_check.js --all-states --pseudolocale`.
- **Relevant logs:** no credentials or production logs involved; static path
  evidence above, runtime matrix check pending.
- **Before submitting:** duplicate-issue search pending; no API keys,
  passwords, or session cookies are included.

Model-racing disposition: defer implementation. The inherited research reports
additional per-key request consumption and a model-routing contract change;
it supplies no measured benefit for this deployment. Those tradeoffs warrant a
separate design and workload evidence before changing the proxy data plane.
Its estimates and external implementation claims were not revalidated here.
