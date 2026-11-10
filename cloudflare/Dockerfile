# Cloudflare Container image for nim-proxy.
#
# Unlike the production scratch image (Dockerfile at the repo root), this
# runtime needs a shell and FUSE: the container disk is ephemeral, so /data
# (config.json + history) is mounted from a private R2 bucket at startup by
# cloudflare/entrypoint.sh. The Rust build stage is the same musl recipe as
# the root Dockerfile. Build context is the repo root (see wrangler.jsonc).
FROM rust:1-alpine@sha256:3c38f3f82c2f3d73da3b38e18d279393a04cb43ddded0e35088a8c3324d40900 AS build
RUN apk add --no-cache musl-dev gcc
ENV RUSTFLAGS="-C target-feature=+crt-static"
ARG TARGETARCH
RUN case "${TARGETARCH:-amd64}" in \
      amd64) t=x86_64-unknown-linux-musl ;; \
      arm64) t=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported TARGETARCH=$TARGETARCH" >&2; exit 1 ;; \
    esac; \
    rustup target add "$t"; \
    echo "$t" > /tmp/target
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release --target "$(cat /tmp/target)" && rm -rf src
COPY src ./src
RUN touch src/main.rs && cargo build --release --target "$(cat /tmp/target)" \
    && cp "target/$(cat /tmp/target)/release/nim-proxy" /app/nim-proxy

FROM alpine:3.20
RUN apk add --no-cache ca-certificates fuse curl
ARG TIGRISFS_VERSION=1.2.1
RUN ARCH=$(uname -m) && \
    case "$ARCH" in x86_64) ARCH=amd64 ;; aarch64) ARCH=arm64 ;; esac && \
    curl -fL "https://github.com/tigrisdata/tigrisfs/releases/download/v${TIGRISFS_VERSION}/tigrisfs_${TIGRISFS_VERSION}_linux_${ARCH}.tar.gz" -o /tmp/tigrisfs.tar.gz && \
    tar -xzf /tmp/tigrisfs.tar.gz -C /usr/local/bin/ && \
    rm /tmp/tigrisfs.tar.gz && \
    chmod +x /usr/local/bin/tigrisfs
COPY --from=build /app/nim-proxy /nim-proxy
COPY cloudflare/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh && mkdir -p /data
ENV DATA_DIR=/data \
    HOST=0.0.0.0 \
    PORT=8000
EXPOSE 8000
ENTRYPOINT ["/entrypoint.sh"]
