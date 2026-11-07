# syntax=docker/dockerfile:1
# trade-gate proxy image — multi-stage.
#
# Pkl is COPIED into the image (not mounted) and evaluated at BUILD
# time only:
#   stage 1 (pkl-builder): download the Pkl CLI, render the Pkl
#     schemas + configs to JSON, and FAIL THE BUILD if the rendered
#     output differs from the checked-in config/generated/ files.
#   stage 2 (rust-builder): build the Rust proxy.
#   stage 3 (runtime): only the proxy binary + the rendered JSON.
#     No Java, no Pkl CLI, no Rust toolchain in the runtime image.
#
# Build context is the repo root (see docker-compose.yml).

ARG PKL_VERSION=0.27.0
# NOTE: verify PKL_VERSION against https://github.com/apple/pkl/releases
# before building — the download URL pattern is
#   https://github.com/apple/pkl/releases/download/<version>/pkl-linux-amd64

FROM eclipse-temurin:21-jre AS pkl-builder
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

FROM rust:1.99-bookworm AS rust-builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY proxy/ ./proxy/
RUN cargo build --release -p trade-gate-proxy

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/*
COPY --from=rust-builder /src/target/release/trade-gate-proxy /usr/local/bin/trade-gate-proxy
COPY --from=pkl-builder /build/policies.json /etc/trade-gate/policies.json
COPY --from=pkl-builder /build/allowlist.json /etc/trade-gate/allowlist.json
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/trade-gate-proxy"]
