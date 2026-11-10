#!/bin/sh
# Mount the private R2 bucket at /data, then run nim-proxy on it.
# Credentials arrive as container env from Worker secrets; never log them.
set -eu

if [ -z "${AWS_ACCESS_KEY_ID:-}" ] || [ -z "${AWS_SECRET_ACCESS_KEY:-}" ] \
  || [ -z "${R2_BUCKET_NAME:-}" ] || [ -z "${R2_ACCOUNT_ID:-}" ]; then
  echo "nim-proxy: R2 mount configuration is incomplete" >&2
  exit 1
fi

mkdir -p /data

/usr/local/bin/tigrisfs \
  --endpoint "https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com" \
  --region auto -f "$R2_BUCKET_NAME" /data &
fuse_pid=$!

attempt=0
until mountpoint -q /data; do
  if ! kill -0 "$fuse_pid" 2>/dev/null; then
    echo "nim-proxy: FUSE process exited before mounting" >&2
    exit 1
  fi
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 30 ]; then
    echo "nim-proxy: timed out mounting R2 bucket" >&2
    exit 1
  fi
  sleep 1
done

echo "nim-proxy: R2 bucket mounted at /data"
exec /nim-proxy
