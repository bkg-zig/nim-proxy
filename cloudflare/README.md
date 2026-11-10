# nim-proxy on Cloudflare Containers

A single Cloudflare Container running nim-proxy behind a Worker at
`nim-gate.eysho.info`. This is a parallel test deployment; the BKG instance at
`nim.eysho.info` remains production. Do not send productive `/v1` traffic
to both instances while they share NIM keys — each instance keeps its own
in-memory per-key rate window.

## Layout

- `src/index.js` — Worker + one named Durable Object (`nim-proxy-singleton`)
  that starts the container, waits for `/health`, and forwards requests.
  One name only, on purpose: a second instance would split the rate window.
- `Dockerfile` — musl build (same recipe as the root Dockerfile) on an
  alpine runtime with `tigrisfs`, because the scratch image cannot mount
  FUSE.
- `entrypoint.sh` — mounts the R2 bucket at `/data`, then execs the binary.
- `wrangler.jsonc` — `durable_object` scheduling policy, custom domain,
  non-secret vars only.

## Persistence

Container disk is ephemeral. `/data` is a FUSE mount of the private R2
bucket `nim-proxy-data`, so `config.json` (credentials, treat the bucket
as a secret) and `history-v1.jsonl` survive sleeps and restarts.
Snapshots are not used. After any restart the in-memory rate window is
empty; keep the usual 61s handoff discipline before another instance with
the same keys serves traffic.

The container sleeps after at most 6h without requests (platform cap on
the inactivity timeout). The next request cold-starts it from R2 state.

First-boot caveat: creating a brand-new `history-v1.jsonl` uses a hard
link to publish the first boot record, which FUSE does not support. Seed
the bucket with an initial history file before first start (an empty file
is enough; the app then appends):

```sh
wrangler r2 object put nim-proxy-data/history-v1.jsonl --remote --file=<seed>
wrangler r2 object put nim-proxy-data/config.json --remote --file=<seed>
```

Note `wrangler r2 object` defaults to a local bucket; `--remote` is
required. Config saves (atomic rename + directory fsync) and history
appends were both verified against the real bucket on 2026-11-10.

## Secrets

R2 S3 credentials (Object Read & Write, scoped to `nim-proxy-data` only)
are Worker secrets, passed to the container at start:

```sh
wrangler secret put AWS_ACCESS_KEY_ID -c cloudflare/wrangler.jsonc
wrangler secret put AWS_SECRET_ACCESS_KEY -c cloudflare/wrangler.jsonc
```

Never put them in `wrangler.jsonc`, `.env`, Git, or logs.

## Deploy

Run from the repository root:

```sh
docker build -f cloudflare/Dockerfile -t nim-proxy-cf:local .   # local proof
wrangler deploy -c cloudflare/wrangler.jsonc
wrangler containers list
curl -sS https://nim-gate.eysho.info/health
```

First requests can take a while (image rollout + FUSE mount + boot).
The dashboard shows the normal login; the seeded store has a superuser,
so there is no first-run wizard on this instance.

## Rollback

```sh
wrangler delete -c cloudflare/wrangler.jsonc   # Worker + container app
wrangler r2 bucket delete nim-proxy-data       # only if data should go too
```

The BKG deployment (root `Dockerfile`, `docker-compose.yml`) is untouched
by all of this.
