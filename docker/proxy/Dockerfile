# syntax=docker/dockerfile:1
# trade-gate proxy image — multi-stage, Alpine where possible.
#
# Pkl is COPIED into the image (not mounted) and evaluated at BUILD
# time only:
#   stage 1 (pkl-builder): download the Pkl CLI, render the Pkl
#     schemas + configs to JSON, and FAIL THE BUILD if the rendered
#     output differs from the checked-in config/generated/ files.
#     Debian slim, NOT Alpine: the pkl binary is glibc-linked and
#     will not run on musl. The JRE base is gone — the pkl CLI is a
#     native binary and never needed Java.
#   stage 2 (rust-builder): build the Rust proxy as a static musl
#     binary (rusqlite is `bundled`, so SQLite is compiled in and
#     nothing is linked against the host libc). Alpine's gcc targets
#     musl natively; cmake is for the libz-ng-sys build script.
#   stage 3 (runtime): Alpine + ca-certificates only — the static
#     binary + the rendered JSON. No shell beyond busybox, no
#     package manager use at runtime, no toolchain. The proxy
#     healthcheck's `wget` is busybox wget, already in the image.
#
# Build context is the repo root (see docker-compose.yml).

ARG PKL_VERSION=0.27.0
# NOTE: verify PKL_VERSION against https://github.com/apple/pkl/releases
# before building — the download URL pattern is
#   https://github.com/apple/pkl/releases/download/<version>/pkl-linux-amd64

FROM debian:bookworm-slim AS pkl-builder
ARG PKL_VERSION
RUN apt-get update && apt-get install -y --no-install-recommends curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL -o /usr/local/bin/pkl \
      "https://github.com/apple/pkl/releases/download/${PKL_VERSION}/pkl-linux-amd64" \
    && chmod +x /usr/local/bin/pkl
WORKDIR /build
COPY config/ ./config/
RUN pkl eval ./config/policies/policies.pkl -f json > /build/policies.json \
 && pkl eval ./config/policies/allowlist.pkl -f json > /build/allowlist.json \
 && diff -u ./config/generated/policies.json /build/policies.json \
 && diff -u ./config/generated/allowlist.json /build/allowlist.json

FROM rust:1.99-alpine AS rust-builder
ARG TARGETARCH
RUN apk add --no-cache musl-dev cmake
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY proxy/ ./proxy/
RUN case "$TARGETARCH" in \
      amd64) TRIPLE=x86_64-unknown-linux-musl ;; \
      arm64) TRIPLE=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported arch: $TARGETARCH" >&2; exit 1 ;; \
    esac && \
    rustup target add "$TRIPLE" && \
    cargo build --release --target "$TRIPLE" -p trade-gate-proxy && \
    cp "target/$TRIPLE/release/trade-gate-proxy" /tmp/trade-gate-proxy

FROM alpine:3.22 AS runtime
RUN apk add --no-cache ca-certificates
COPY --from=rust-builder /tmp/trade-gate-proxy /usr/local/bin/trade-gate-proxy
COPY --from=pkl-builder /build/policies.json /etc/trade-gate/policies.json
COPY --from=pkl-builder /build/allowlist.json /etc/trade-gate/allowlist.json
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/trade-gate-proxy"]
