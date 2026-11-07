#!/usr/bin/env python3
"""trade-gate policy probe — NOT the trading script.

Exercises the proxy's network policies against a mock upstream
(test-script/mock_upstream.py), never against a real broker.

Local mode (default): starts the mock on 127.0.0.1:18099 and the
proxy binary on 127.0.0.1:18080 (fresh temp SQLite DB), runs the
cases, tears both down. Requires the proxy to be built first:
  cargo build -p trade-gate-proxy

Docker mode: if PROXY_BASE is set in the environment, the proxy and
mock are assumed to be already up (docker compose) and nothing is
started here — the cases just run against PROXY_BASE.

Cases (each asserts status, and the error code for denials):
  allowed GET quote / account / orders ......... 200, forwarded
  allowed POST previewOrder, valid BUY ........ 200, forwarded
  allowed POST orders, valid BUY .............. 201, Location header
  GET /orders/{id} confirms the buy .......... 200
  second buy of the same symbol in window ..... 409 duplicate_buy_in_window
  POST orders with SELL ....................... 403 body_mismatch
  POST orders, non-allowlisted symbol ......... 403 symbol_not_allowlisted
  POST orders, wrong session .................. 403 body_mismatch
  POST orders, extra field .................... 403 body_mismatch
  POST orders, qty x price over cap ........... 403 over_cap
  POST orders, invalid JSON ................... 400 invalid_json
  DELETE / PUT orders ......................... 403 no_route
  unknown path ................................ 403 no_route
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
MOCK_PORT = 18099
PROXY_PORT = 18080
HASH = "abc123"

PROXY_BASE = os.environ.get("PROXY_BASE", f"http://127.0.0.1:{PROXY_PORT}")

PASS = 0
FAIL = 0


def check(name, got_status, want_status, body=None, want_error=None):
    global PASS, FAIL
    ok = got_status == want_status
    if want_error is not None:
        ok = ok and isinstance(body, dict) and body.get("error") == want_error
    if ok:
        PASS += 1
        print(f"PASS  {name}  ({got_status})")
    else:
        FAIL += 1
        print(f"FAIL  {name}: got {got_status} {body!r}, want {want_status} error={want_error!r}")


def req(method, path, body=None, raw=None):
    url = PROXY_BASE + path
    data = raw if raw is not None else (
        json.dumps(body).encode() if body is not None else None)
    r = urllib.request.Request(url, data=data, method=method)
    if data is not None:
        r.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(r, timeout=10) as resp:
            payload = resp.read()
            return resp.status, (json.loads(payload) if payload else None), resp.headers
    except urllib.error.HTTPError as e:
        payload = e.read()
        try:
            parsed = json.loads(payload) if payload else None
        except json.JSONDecodeError:
            parsed = payload
        return e.code, parsed, e.headers


def buy_body(symbol="VOO", instruction="BUY", session="NORMAL", qty=1, **extra):
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


def wait_health(url, proc_name, timeout=30):
    for _ in range(int(timeout * 2)):
        try:
            with urllib.request.urlopen(url, timeout=2) as resp:
                if resp.status == 200:
                    return True
        except Exception:
            time.sleep(0.5)
    print(f"FAIL  {proc_name} did not become healthy at {url}")
    return False


def run_cases():
    global PASS, FAIL
    # Health — answered locally by the proxy, never forwarded.
    s, b, _ = req("GET", "/healthz")
    check("healthz", s, 200, b)

    # Allowed reads.
    s, b, _ = req("GET", "/trader/v1/accounts/accountNumbers")
    check("GET accountNumbers", s, 200, b)
    s, b, _ = req("GET", f"/trader/v1/accounts/{HASH}")
    check("GET account", s, 200, b)
    s, b, _ = req("GET", f"/trader/v1/accounts/{HASH}/orders")
    check("GET list orders", s, 200, b)
    s, b, _ = req("GET", "/marketdata/v1/quotes?symbols=VOO")
    check("GET quote VOO (caches $500 price)", s, 200, b)

    # Allowed preview, valid BUY.
    s, b, _ = req("POST", f"/trader/v1/accounts/{HASH}/previewOrder", buy_body())
    check("POST previewOrder valid BUY", s, 200, b)

    # Denials — before the real buy, so they cannot pollute the DB.
    s, b, _ = req("POST", f"/trader/v1/accounts/{HASH}/orders", buy_body(instruction="SELL"))
    check("POST orders SELL", s, 403, b, "body_mismatch")
    s, b, _ = req("POST", f"/trader/v1/accounts/{HASH}/orders", buy_body(symbol="GME"))
    check("POST orders non-allowlisted symbol", s, 403, b, "symbol_not_allowlisted")
    s, b, _ = req("POST", f"/trader/v1/accounts/{HASH}/orders", buy_body(session="EXTENDED"))
    check("POST orders wrong session", s, 403, b, "body_mismatch")
    s, b, _ = req("POST", f"/trader/v1/accounts/{HASH}/orders", buy_body(surprise="x"))
    check("POST orders extra field", s, 403, b, "body_mismatch")
    s, b, _ = req("POST", f"/trader/v1/accounts/{HASH}/orders", buy_body(qty=3))
    check("POST orders over cap (3 x $500 > $1000)", s, 403, b, "over_cap")
    s, b, _ = req("POST", f"/trader/v1/accounts/{HASH}/orders", raw=b"{not json")
    check("POST orders invalid JSON", s, 400, b, "invalid_json")
    s, b, _ = req("DELETE", f"/trader/v1/accounts/{HASH}/orders/order-1")
    check("DELETE orders", s, 403, b, "no_route")
    s, b, _ = req("PUT", f"/trader/v1/accounts/{HASH}/orders/order-1", buy_body())
    check("PUT orders", s, 403, b, "no_route")
    s, b, _ = req("GET", "/trader/v1/accounts/whatever/transactions")
    check("GET unknown path", s, 403, b, "no_route")

    # The real (mock) buy, then confirmation, then duplicate.
    s, b, h = req("POST", f"/trader/v1/accounts/{HASH}/orders", buy_body())
    check("POST orders valid BUY", s, 201, b)
    location = h.get("Location") if h else None
    print(f"      Location: {location}")
    if location:
        oid = location.rsplit("/", 1)[-1]
        s, b, _ = req("GET", f"/trader/v1/accounts/{HASH}/orders/{oid}")
        check("GET order confirms buy", s, 200, b)
    else:
        FAIL += 1
        print("FAIL  no Location header on 201 — cannot confirm buy")
    s, b, _ = req("POST", f"/trader/v1/accounts/{HASH}/orders", buy_body())
    check("POST orders duplicate in window", s, 409, b, "duplicate_buy_in_window")

    print(f"\n{PASS} passed, {FAIL} failed")
    return 1 if FAIL else 0


def main():
    if PROXY_BASE.startswith("http://127.0.0.1") and os.environ.get("PROXY_BASE") is None:
        # Local mode: start mock + proxy, run, tear down.
        proxy_bin = os.environ.get(
            "PROXY_BIN", os.path.join(REPO, "target", "debug", "trade-gate-proxy"))
        if not os.path.exists(proxy_bin):
            print(f"proxy binary not found at {proxy_bin} — run `cargo build -p trade-gate-proxy` first")
            return 2
        tmp = tempfile.mkdtemp(prefix="trade-gate-probe-")
        env = dict(os.environ)
        env.update({
            "PROXY_LISTEN": f"127.0.0.1:{PROXY_PORT}",
            "UPSTREAM_TLS": "false",
            "POLICIES_PATH": os.path.join(REPO, "config", "generated", "policies.json"),
            "ALLOWLIST_PATH": os.path.join(REPO, "config", "generated", "allowlist.json"),
            "DB_PATH": os.path.join(tmp, "buys.db"),
            "CREDENTIAL_STORE": "stub",
        })
        mock = subprocess.Popen(
            [sys.executable, os.path.join(HERE, "mock_upstream.py"), str(MOCK_PORT)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        proxy = subprocess.Popen(
            [proxy_bin, "--upstream-host", f"127.0.0.1:{MOCK_PORT}"], env=env,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            if not wait_health(f"http://127.0.0.1:{MOCK_PORT}/healthz", "mock"):
                return 2
            if not wait_health(f"http://127.0.0.1:{PROXY_PORT}/healthz", "proxy"):
                return 2
            return run_cases()
        finally:
            proxy.terminate()
            mock.terminate()
            for proc in (proxy, mock):
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=10)
    # Docker mode: PROXY_BASE was set — proxy + mock are already up.
    return run_cases()


if __name__ == "__main__":
    sys.exit(main())
