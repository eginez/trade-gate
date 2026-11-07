#!/bin/sh
# test-script/run.sh — build the proxy, run the unit tests, then the
# end-to-end policy probe against the mock upstream. No real broker, no Docker.
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
cargo test
cargo build -p trade-gate-proxy
python3 test-script/probe.py
