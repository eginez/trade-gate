#!/usr/bin/env python3
"""Demo client for trade-gate — NOT a trading script.

A short, human-readable demo that exercises the proxy rules against
a mock upstream on localhost. It shows, in plain language, what the
proxy lets through and what it blocks, and why.

Run it from the repo root:
    python3 demo/demo.py

It starts the mock upstream and the proxy itself (fresh temp DB),
runs the demo, then tears both down. Requires the proxy built first:
    cargo build -p trade-gate-proxy

Or point it at an already-running proxy (e.g. in Docker):
    PROXY_BASE=http://proxy:8080 python3 demo/demo.py

No real broker, no real money, no credentials in this file.
The proxy injects the credential; this script never sees one.
"""
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
MOCK_PORT = 18199
PROXY_PORT = 18180
HASH = "abc123"

PROXY_BASE = os.environ.get("PROXY_BASE", f"http://127.0.0.1:{PROXY_PORT}")


def call(method, path, body=None, raw=None):
    """One request through the proxy. Returns (status, parsed body, headers)."""
    url = PROXY_BASE + path
    data = raw if raw is not None else (
        json.dumps(body).encode() if body is not None else None)
    req = urllib.request.Request(url, data=data, method=method)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            payload = resp.read()
            return resp.status, (json.loads(payload) if payload else None), resp.headers
    except urllib.error.HTTPError as e:
        payload = e.read()
        try:
            parsed = json.loads(payload) if payload else None
        except json.JSONDecodeError:
            parsed = payload
        return e.code, parsed, e.headers


def show(title, status, body, want):
    mark = "ok" if status == want else "UNEXPECTED"
    reason = ""
    if isinstance(body, dict) and body.get("error"):
        reason = f" — {body['error']}"
    print(f"[{mark}] {title}: got {status}, want {want}{reason}")


def buy(symbol="VOO", instruction="BUY", session="NORMAL", qty=1, **extra):
    body = {
        "session": session,
        "duration": "DAY",
        "orderType": "LIMIT",
        "orderStrategyType": "SINGLE",
        "orderLegCollection": [{
            "instruction": instruction,
            "quantity": qty,
            "instrument": {"symbol": symbol, "assetType": "ETF"},
        }],
    }
    body.update(extra)
    return body


def wait_health(url, name, timeout=30):
    for _ in range(int(timeout * 2)):
        try:
            with urllib.request.urlopen(url, timeout=2) as resp:
                if resp.status == 200:
                    return True
        except Exception:
            time.sleep(0.5)
    print(f"[!] {name} did not become healthy at {url}")
    return False


def demo():
    print("trade-gate demo — what the proxy lets through, and what it blocks\n")

    print("1. Allowed reads — the proxy forwards these to the upstream")
    s, _, _ = call("GET", "/trader/v1/accounts/accountNumbers")
    show("GET account numbers", s, None, 200)
    s, _, _ = call("GET", "/marketdata/v1/quotes?symbols=VOO")
    show("GET quote for VOO (price $500 in the mock)", s, None, 200)

    print("\n2. Allowed buy — one leg, BUY, allowlisted symbol, under the cap")
    s, _, _ = call("POST", f"/trader/v1/accounts/{HASH}/previewOrder", buy())
    show("POST preview order, valid BUY", s, None, 200)
    s, _, headers = call("POST", f"/trader/v1/accounts/{HASH}/orders", buy())
    show("POST place order, valid BUY", s, None, 201)
    location = headers.get("Location") if headers else None
    print(f"    order created at: {location}")
    if location:
        # Confirming the order is what records the buy in the
        # rolling-window DB — only then does the duplicate check bite.
        s, _, _ = call("GET", location)
        show("GET that order to confirm it", s, None, 200)

    print("\n3. Denied — the proxy answers itself, nothing reaches the upstream")
    s, b, _ = call("POST", f"/trader/v1/accounts/{HASH}/orders",
                   buy(instruction="SELL"))
    show("POST place order, SELL (BUY-only)", s, b, 403)
    s, b, _ = call("POST", f"/trader/v1/accounts/{HASH}/orders",
                   buy(symbol="GME"))
    show("POST place order, symbol not on the allowlist", s, b, 403)
    s, b, _ = call("POST", f"/trader/v1/accounts/{HASH}/orders",
                   buy(session="EXTENDED"))
    show("POST place order, wrong session", s, b, 403)
    s, b, _ = call("POST", f"/trader/v1/accounts/{HASH}/orders",
                   buy(qty=3))
    show("POST place order, 3 x $500 over the $1000 cap", s, b, 403)
    s, b, _ = call("DELETE", f"/trader/v1/accounts/{HASH}/orders/order-1")
    show("DELETE an order (no such route)", s, b, 403)

    print("\n4. Duplicate — same symbol bought twice in the window")
    s, b, _ = call("POST", f"/trader/v1/accounts/{HASH}/orders", buy())
    show("POST place order, same symbol again", s, b, 409)

    print("\nDone. Every request above got an answer from the proxy —")
    print("nothing was swallowed, and no denied request reached the upstream.")


def main():
    if os.environ.get("PROXY_BASE"):
        demo()
        return 0
    # Local mode: start mock + proxy, run, tear down.
    proxy_bin = os.environ.get(
        "PROXY_BIN", os.path.join(REPO, "target", "debug", "trade-gate-proxy"))
    if not os.path.exists(proxy_bin):
        print(f"proxy binary not found at {proxy_bin}")
        print("run `cargo build -p trade-gate-proxy` first")
        return 2
    tmp = tempfile.mkdtemp(prefix="trade-gate-demo-")
    env = dict(os.environ)
    env.update({
        "PROXY_LISTEN": f"127.0.0.1:{PROXY_PORT}",
        "UPSTREAM_HOST": f"127.0.0.1:{MOCK_PORT}",
        "UPSTREAM_TLS": "false",
        "POLICIES_PATH": os.path.join(REPO, "config", "generated", "policies.json"),
        "ALLOWLIST_PATH": os.path.join(REPO, "config", "generated", "allowlist.json"),
        "DB_PATH": os.path.join(tmp, "buys.db"),
        "CREDENTIAL_STORE": "stub",
    })
    mock = subprocess.Popen(
        [sys.executable, os.path.join(REPO, "test-script", "mock_upstream.py"),
         str(MOCK_PORT)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    proxy = subprocess.Popen(
        [proxy_bin], env=env,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        if not wait_health(f"http://127.0.0.1:{MOCK_PORT}/healthz", "mock upstream"):
            return 2
        if not wait_health(f"http://127.0.0.1:{PROXY_PORT}/healthz", "proxy"):
            return 2
        demo()
        return 0
    finally:
        proxy.terminate()
        mock.terminate()
        for proc in (proxy, mock):
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)


if __name__ == "__main__":
    sys.exit(main())
