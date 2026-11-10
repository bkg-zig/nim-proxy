---
type: Runbook
title: Deploy on Cloudflare Containers
description: Single container behind a Worker, R2 FUSE /data, parallel-test rules.
tags: [cloudflare, deployment, containers, r2]
timestamp: 2026-11-10T00:00:00Z
---

# Deploy on Cloudflare Containers

nim-proxy also runs as one Cloudflare Container behind a Worker
(`cloudflare/` in the repo): parallel test deployment at
`nim-gate.eysho.info`, seeded from an existing store. The BKG Docker
deployment ([deploy-docker](deploy-docker.md)) stays production. Never
serve productive `/v1` traffic from two instances that share NIM keys —
each instance keeps its own in-memory per-key window, so a second active
instance can exceed the upstream limit (invariant 3). Exactly one Durable
Object name (`nim-proxy-singleton`) is routed, on purpose.

## Why the image differs

Container disk is ephemeral and the scratch image has no shell, so this
deploy uses its own image: same musl build, alpine runtime with
`tigrisfs`. At startup `cloudflare/entrypoint.sh` mounts the private R2
bucket `nim-proxy-data` at `/data`; credentials are Worker secrets passed
to the container at start, scoped Object Read & Write to that bucket
only. Treat the bucket as a secret: it holds `config.json`.

Verified against the real bucket (2026-11-10): config atomic rename +
directory fsync work on the mount, and history appends with `sync_all`
persist while the container runs.

**Gotcha — first history creation fails on FUSE.** Publishing the first
boot record uses `fs::hard_link`; tigrisfs answers `CreateLinkOp: not
implemented` and the history store degrades to in-memory (the proxy still
boots). Seed `history-v1.jsonl` before first start — an empty file is
enough, the open path then appends. Seed `config.json` the same way.
`wrangler r2 object` defaults to a *local* bucket; always pass `--remote`
or the seed silently lands nowhere.

**Gotcha — DNS wildcard.** A `*.eysho.info` A record to the BKG server
can shadow a new Worker custom domain for IPv4 clients until caches
expire. Check with `dig @1.1.1.1`, not the local resolver, and do not
delete records the Worker manages.

## Lifecycle

The container sleeps after at most 6h without requests (platform cap on
the inactivity timeout) and cold-starts from R2 state on the next one.
Rate windows reset on restart; the 61s drained-handoff rule from
[deploy-docker](deploy-docker.md) applies before another instance with
the same keys serves traffic.

## Deploy and rollback

```sh
wrangler secret put AWS_ACCESS_KEY_ID -c cloudflare/wrangler.jsonc
wrangler secret put AWS_SECRET_ACCESS_KEY -c cloudflare/wrangler.jsonc
wrangler deploy -c cloudflare/wrangler.jsonc
curl -sS https://nim-gate.eysho.info/health   # expect: ok
```

Rollback: `wrangler delete -c cloudflare/wrangler.jsonc`; delete the
bucket and revoke the `nim-proxy-data-rw` token only when the data
should go too. The live state and remaining owner checks (logged-in
Settings save, SSE through the Worker) are recorded in
[the deploy plan](../../docs/plans/cloudflare-container-deploy.md).
