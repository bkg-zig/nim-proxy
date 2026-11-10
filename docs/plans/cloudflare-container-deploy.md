# Cloudflare Container deploy for nim-proxy

Status: live on nim-gate.eysho.info (2026-11-10), parallel test mode
Integration branch: `main` (only branch present; `release/v0.6.6` from older
plans no longer exists). Task branch: `deploy/cloudflare-containers` in the
worktree at `/home/bkg/api/bkg-nim-cf`. Final PR targets `main`.

## Outcome

nim-proxy runs as exactly one Cloudflare Container behind a Worker on
`nim-gate.eysho.info`, seeded with the operator-supplied `config.json`, with
`/data` persisted on a private R2 bucket via FUSE. The existing BKG instance
at `nim.eysho.info` stays production; the Cloudflare instance is a parallel
test deployment and must not serve productive `/v1` traffic while the BKG
instance uses the same NIM keys.

## Decisions (owner, 2026-11-10)

- Scope: prepare **and** live-deploy.
- Persistence: R2 FUSE mount for `/data` (container disk itself is ephemeral).
- Instances: exactly one. The in-memory per-key sliding window makes a
  second instance with the same keys a rate-limit violation risk
  (invariant 3). Single Durable Object name, `instance` fixed in code.
- Mode: parallel test. BKG `nim.eysho.info` remains production, untouched.
  No DNS change to `nim.eysho.info`.
- Domain: `nim-gate.eysho.info` as Worker custom domain.
- Seed: operator-supplied `.helicon/attachments/config.json` (plaintext NIM
  keys). It is uploaded only as an R2 object, never committed, never logged.

## Constraints

- The scratch image cannot run FUSE (no shell, no fuse, no tigrisfs), so
  Cloudflare gets its own runtime image under `cloudflare/`. The existing
  `Dockerfile` and Compose deploy stay untouched.
- `config.json` saves are atomic rename + fsync(dir) + 0600, and history
  appends use `sync_all`. FUSE/R2 does not give full POSIX semantics; these
  paths must be proven against a real bucket locally before live deploy,
  not assumed.
- Container env secrets (R2 S3 credentials) come from Worker secrets passed
  through `ctx.container.start({ env })`. Nothing secret lands in
  `wrangler.jsonc`, Git, or logs.
- `TRUST_PROXY=true` (Cloudflare terminates TLS). The app port is only
  reachable through the Durable Object, never published directly.
- Inactivity timeout is capped at 6h by the platform. After a sleep or a
  restart the rate window resets; the documented 61s handoff rule for
  restarts still applies. Cold start on next request is accepted.

## Ponytail rung

Plain JS Worker with the Durable Object Container API (`ctx.container`),
no `@cloudflare/containers` dependency, no TypeScript build, no code
changes to the Rust binary. One new Dockerfile reusing the existing musl
build recipe plus alpine + tigrisfs for the mount.

## Proof (named before editing)

1. `docker build` of the Cloudflare image succeeds.
2. Local container run against the real R2 bucket (FUSE device): binary
   boots, `/health` answers, a history boot record appears as an R2
   object, `config.json` still parses after boot.
3. `wrangler deploy --dry-run` (or config validation) passes.
4. Live: `curl https://nim-gate.eysho.info/health` returns `ok`, dashboard
   shows the login page (seeded user, no first-run wizard), container
   logs show a clean boot, exactly one container instance runs.
5. No `/v1` completion is sent through the Cloudflare instance in
   parallel-test mode. SSE keepalive is therefore verified only as far
   as the dashboard/health path allows and is listed as open below.

## Findings during local validation (2026-11-10)

- Image builds (`nim-proxy-cf:local`, 96.8MB). Host disk was 100% full;
  `docker builder prune` freed 8.5GB of regenerable build cache.
- `wrangler r2 object` defaults to a local bucket. The first seed landed
  locally and proved nothing; redone with `--remote` and compared.
- FUSE cannot create the first history file: `publish_first_boot` uses
  `fs::hard_link`, tigrisfs answers `CreateLinkOp: not implemented`, and
  the store degrades to in-memory history (proxy still boots). Fix at
  deploy level, no Rust change: seed `history-v1.jsonl` before first
  start. After that, appends + `sync_all` persist to R2 (verified: seed
  boot + FUSE boot + samples, 952 bytes in the bucket while running).
- Config rename + directory fsync verified on the FUSE mount. The
  app-level Settings save still needs a logged-in check after deploy
  (the seeded superuser password is only known to the owner).
- Creating the R2 S3 credentials needed the account API token from
  `/srv/bkg/core/secrets/cloudflare_api_token`; the wrangler OAuth login
  has no token-management scope (403). Token `nim-proxy-data-rw` is
  scoped to this one bucket, Object Read & Write.
- tigrisfs logs the access key id at startup. That id alone is not the
  secret, but container logs should be treated as sensitive anyway.

## Open / risks

- FUSE write semantics for atomic config rename and per-sample history
  `sync_all` (latency, failure mode). Proven locally per proof 2; if the
  history write path poisons the store on FUSE, this deploy stops and the
  finding is recorded here instead of shipping around it.
- Running as root inside the Cloudflare VM (FUSE mount) weakens the
  non-root posture of the scratch image. The VM is single-tenant and the
  port is only reachable via the Durable Object; recorded, not hidden.
- SSE through Worker + Durable Object + `getTcpPort().fetch()` for long
  generations is unverified in parallel-test mode (would need a `/v1`
  call). Verify before any migration off BKG.
- The seeded config file in `.helicon/attachments/` contains plaintext
  NIM keys and sits untracked in the main checkout. Delete or move it
  after seeding; it must never be committed.

## Domain correction (2026-11-10)

First deploy used `gate.eysho.info` and failed on the custom domain only:
that name is an existing Cloudflare AI Gateway (CNAME
`gateway.ai.cloudflare.com`, live). It was left untouched. Owner chose
`nim-gate.eysho.info`; the Worker also answers on its workers.dev URL.

## Scope delta shown to owner

Original ask was "prepare Cloudflare container deploy". Owner expanded it
to live deploy + R2 FUSE + seeded config + `nim-gate.eysho.info` on
2026-11-10. The BKG production instance and its domain stay out of scope.

## Live result (2026-11-10)

- `wrangler deploy` succeeded after the domain correction above
  (Version 357e325d-7422-410d-b222-b01ceace1ef1).
- `https://nim-gate.eysho.info/health` and the workers.dev URL return
  `ok` (HTTP 200, ~0.12s warm). `/` returns the same 401 auth body as
  production, `/login` returns 200: the seeded store is active, no
  first-run wizard.
- Exactly one container instance, named `nim-proxy-singleton`.
- History persistence is live: the bucket object grew to 1936 bytes /
  4 boot records after the Cloudflare boot.
- No `/v1` request was sent through this instance (parallel-test rule).
  SSE keepalive and the logged-in Settings save remain owner checks.
- Local proof containers and the `/tmp` credential env file were removed.
  The R2 token `nim-proxy-data-rw` stays in the account for the running
  container; revoke it in the dashboard if this deployment is torn down.

## Demo-key test and base_url fix (2026-11-10)

The owner created a `test` client key in the Cloudflare dashboard and
asked for a live check. It first returned upstream 404s for every model.
Root cause, reproduced locally and directly against NVIDIA: the seeded
`upstream.base_url` is `https://integrate.api.nvidia.com/v1`, but the
binary appends the request path (`/v1/...`) to the base, producing
`/v1/v1/...`. The default base is the bare host. Fix applied only to the
R2 copy of the config (`base_url` without `/v1`, all 25 client keys and
the owner's new `test` key preserved), then the container was restarted
through a temporary token-guarded Worker hook that was removed and
re-deployed afterwards. The attachment seed file still contains the
broken value; fix it there before reusing it anywhere else.

After the fix: `/v1/models` returns 200 (84 models) and a chat
completion with the demo key returned HTTP 200 with usage. Two upstream
behaviors were confirmed directly against NVIDIA and are not proxy
faults: several catalog models return 404 `Function ... Not found for
account`, and the old `meta/llama-3.2-11b-vision-instruct` is end-of-life
(410). This was a single owner-requested test request; parallel-test mode
otherwise still applies (no productive `/v1` traffic on this instance).
