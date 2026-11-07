# test-script/ — policy exerciser, NOT the trading script
#
# `probe.py` hits the proxy with good and bad requests and asserts
# the proxy's answers (200 forwarded / 403 with a reason / 409
# duplicate / 400 invalid JSON). `mock_upstream.py` is a fake broker
# on localhost that the proxy forwards to. Neither file trades
# anything, and neither ever talks to Schwab.
#
# Run locally (no Docker):
#   ./run.sh
#   # or: cargo test && cargo build -p trade-gate-proxy && python3 probe.py
#
# Run in Docker (proxy + mock already up via compose):
#   PROXY_BASE=http://proxy:8080 python3 probe.py
