#!/usr/bin/env python3
"""Mock broker API for trade-gate tests. NOT Schwab, NOT a trading script.

A tiny fake of the Schwab Trader API shapes the proxy forwards to:
quotes, accountNumbers, account, orders, previewOrder, placeOrder.
It runs on localhost only, holds no real money, and requires an
Authorization header — the proxy is supposed to inject one, so a
request that arrives here without it is a proxy bug (401).

Usage: python3 mock_upstream.py [port]   (default 18099)
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

ORDERS = {}
NEXT_ID = [1]
HASH = "abc123"


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # keep test output clean
        pass

    def _send(self, status, obj=None, headers=None):
        body = b"" if obj is None else json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if body:
            self.wfile.write(body)

    def _require_auth(self):
        # The proxy injects Authorization; direct calls without it fail.
        if not self.headers.get("Authorization", "").startswith("Bearer "):
            self._send(401, {"error": "missing_auth"})
            return False
        return True

    def do_GET(self):
        if self.path == "/healthz":
            return self._send(200, {"status": "ok"})
        if not self._require_auth():
            return
        parsed = urlparse(self.path)
        path = parsed.path
        if path == "/trader/v1/accounts/accountNumbers":
            return self._send(200, [{"accountNumber": "123456789", "hashValue": HASH}])
        if path == f"/trader/v1/accounts/{HASH}":
            return self._send(200, {"securitiesAccount": {
                "accountNumber": "123456789",
                "currentBalances": {"cashBalance": 10000.0}}})
        if path == f"/trader/v1/accounts/{HASH}/orders":
            return self._send(200, list(ORDERS.values()))
        if path.startswith(f"/trader/v1/accounts/{HASH}/orders/"):
            oid = path.rsplit("/", 1)[-1]
            if oid in ORDERS:
                return self._send(200, ORDERS[oid])
            return self._send(404, {"error": "order_not_found"})
        if path == "/marketdata/v1/quotes":
            qs = parse_qs(parsed.query)
            out = {}
            for sym in qs.get("symbols", ["VOO"])[0].split(","):
                out[sym] = {"assetType": "ETF", "symbol": sym, "lastPrice": 500.0}
            return self._send(200, out)
        return self._send(404, {"error": "not_found"})

    def do_POST(self):
        if not self._require_auth():
            return
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        try:
            body = json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            return self._send(400, {"error": "invalid_json"})
        path = urlparse(self.path).path
        if path == f"/trader/v1/accounts/{HASH}/previewOrder":
            return self._send(200, {"orderStrategy": body,
                                      "orderValidationResult": {"isValid": True}})
        if path == f"/trader/v1/accounts/{HASH}/orders":
            oid = f"order-{NEXT_ID[0]}"
            NEXT_ID[0] += 1
            order = dict(body)
            order["orderId"] = oid
            order["status"] = "WORKING"
            ORDERS[oid] = order
            return self._send(201, None, {"Location": f"/trader/v1/accounts/{HASH}/orders/{oid}"})
        return self._send(404, {"error": "not_found"})


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 18099
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
