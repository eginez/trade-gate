# Architecture

`trade-gate` is a policy-gated reverse proxy for scheduled purchases.
An untrusted script talks plain HTTP to the proxy; the proxy inspects
every request against a permits-only policy, injects the real
credential, and re-encrypts the request to the broker with the
broker's real certificate.

## Trust model

Three parties, three trust levels. Do not blur them.

| Party | Trust | Sees |
|---|---|---|
| Script (untrusted) | None. Bugs or bad edits are treated as compromise. | Only the proxy at `proxy:8080`. No token, no App Key/Secret, no DB, no internet. |
| Proxy (trusted) | Holds real credentials, the policy, the allowlist, the buy-history DB. | Everything it needs to gate and forward. |
| Host / Docker supervisor (trusted) | Runs the containers, owns the credential store. | The refresh token store, the DB volume. |

Consequences:

- The script can never spend anything the policy does not permit,
  even if it is fully compromised: the gate checks every request
  before forwarding, and the credential is injected by the proxy,
  never handed to the script.
- The proxy is only a **fail-safe**. Every call the script initiates
  gets a result: success passed through, or an explicit `403`
  (gate, with a JSON reason), `409` (duplicate buy in the rolling
  window), `400` (invalid JSON), or `502`/`504` (upstream timed
  out). Nothing is silently dropped, no empty `200`, no hang.
- The proxy is **not** a daemon: it runs for the duration of one
  run and exits when its parent (Compose / the scheduler) stops it.
  Nothing is left running between runs.

## Components

```
test-script / demo ──(internal net)──> proxy ──(external net)──> broker API
   (untrusted)            (Pingora)                (TLS, real cert)
                              │
                    SQLite buys.db (proxy-only volume)
                    Pkl-rendered JSON (config/generated/)
                    credential store (stub for now)
```

- **Proxy** (`proxy/`, Rust, Pingora `ProxyHttp`). One binary,
  `trade-gate-proxy`. Started with a mandatory
  `--upstream-host <host:port>` argument — no default, nothing
  hardcoded. Listens on `PROXY_LISTEN` (default `127.0.0.1:8080`).
  See `proxy/src/main.rs` for startup and `proxy/src/gate.rs`
  for the request path.
- **Policy** (`config/`, Pkl rendered to JSON). The permits and the
  allowlist. Loaded at startup; a bad policy refuses to start the
  proxy instead of starting permissive. See `docs/policy.md`.
- **Buy-history DB** (`proxy/src/db.rs`, SQLite, rusqlite). Two
  tables: `buys` (confirmed buys) and `attempts` (previews, denials,
  timeouts — never blocks a later buy). Only the proxy mounts it.
- **Credential store** (`proxy/src/credentials.rs`). Trait plus a
  stub that returns a clearly-marked fake token and warns on every
  use. Real broker OAuth and the `docker-credential-pass` backend
  are **not implemented yet** — see `docs/development.md`.
- **Test script** (`test-script/probe.py`, Python). A policy
  exerciser, **not** the trading script. Hits the proxy against a
  mock upstream and asserts the 200 / 403 / 409 / 400 behaviour.
  The trading script is built last, on the owner's word only.
- **Mock upstream** (`test-script/mock_upstream.py`). Fake broker
  API on localhost. No real broker is ever contacted in tests.

## Request flow

The gate (`proxy/src/gate.rs`, `request_filter`) runs, in order,
fail-closed at every step:

1. `GET /healthz` — answered locally, never forwarded.
2. **Route exists** (method + path template) — else `403 no_route`.
   Only 7 actions exist; there is no route for DELETE, PUT, SELL,
   or transactions.
3. **Body parses and matches the permit's body rule** — else
   `400 invalid_json` / `403 body_mismatch`. Unknown fields are
   rejected; the body rule pins session `NORMAL`, duration `DAY`,
   order type `MARKET`/`LIMIT`, strategy `SINGLE`, instruction
   literal `BUY`, asset type `EQUITY`/`ETF`, exactly one leg,
   quantity ≥ 1.
4. **Symbol allowlisted, dollar cap** (`quantity × last quote
   price ≤ cap`) — else `403 symbol_not_allowlisted` /
   `over_cap`. No live price (the script did not quote first) is
   a deny, not a pass. The price comes from a cache populated by
   earlier `get_quote` responses, so the script must quote a
   symbol before it can buy it.
5. **Rolling-window duplicate** (`place_order` only) — if the same
   `(account_hash, symbol)` was already bought inside the window,
   `409 duplicate_buy_in_window`, never forwarded.
6. **Forward**, with the real credential injected. Any
   `Authorization` header the script sent is replaced, never
   forwarded.

GETs stream through Pingora normally. POSTs are **store-and-
forwarded**: Pingora 0.9 consumes a body read in `request_filter`
and never replays it upstream (retry buffering is enabled only
after `request_filter`, and there is no public API to inject a body
back). So the gate buffers the small (< 64 KiB) POST body,
evaluates it, and forwards it itself with a hyper client
(`forward_post` in `gate.rs`). This is a deliberate design
constraint — do not "simplify" it back to streaming.

### When a buy becomes a buy

A `201` from upstream does **not** write a buy row. It marks the
order *pending* (order id from the `Location` header). Only a
confirmed `GET /orders/{id}` afterwards writes the buy row. That
is what makes the retry rule safe:

- POST timeout / no `Location` → **do not retry blindly**. Redo
  the duplicate check (`GET /orders?status=WORKING`), then confirm
  (`GET /orders/{id}`). A retry that would double-buy is caught
  by the 409 check, and a buy that did land is confirmed instead
  of repeated.
- Previews, 400s, and timeouts write an *attempt* row, never a
  buy. An attempt never blocks a later real buy.

## The script's call sequence

All calls go via the proxy, in this order:

1. `GET /trader/v1/accounts/accountNumbers` → account hash
2. `GET /trader/v1/accounts/{hash}` (pre-flight)
3. `GET /trader/v1/accounts/{hash}/orders?status=WORKING`
   (duplicate check)
4. `GET /marketdata/v1/quotes?symbols=...` (price → whole shares)
5. `POST /trader/v1/accounts/{hash}/previewOrder`
6. `POST /trader/v1/accounts/{hash}/orders` (the only POST that places)
7. `GET /trader/v1/accounts/{hash}/orders/{id}` (confirm)

The script never calls OAuth, DELETE/PUT, SELL, or transactions.

## Sandbox (Docker)

`docker/docker-compose.yml`: three services, two networks.

- `test-script` (and `demo`, and the future trading script) live
  only on the `internal` network (`internal: true` — no internet
  egress, ever) and can only reach `proxy:8080`. Hardened:
  `read_only`, `cap_drop: [ALL]`, `no-new-privileges`, user
  `1000:1000`, `mem_limit: 256m`, `cpus: 0.5`, `pids_limit: 64`.
  No ports published.
- `proxy` is on `internal` + `external` — the only container
  with internet. It mounts the DB volume (`proxy-data`).
- `mock` stands in for the broker API on the external leg in
  tests. In production the proxy's external leg is the broker API.
- A `refresh` profile exists for the weekly token keep-alive, but
  the refresh flow is still stubbed (see below).

**Known limitation:** Docker containers cannot start on the build
VM (no bridge module, no `CAP_NET_ADMIN` — `setns`/`netns`
operations are denied at the host level). The Compose file as
written cannot run there; the probe and demo run locally against
the real proxy binary + mock instead. Running Compose needs a
machine with a real Docker daemon.

## What is NOT built yet

- **The trading script.** Comes last, on the owner's word only.
- **Real broker OAuth.** Authorization-code + refresh-token flow,
  7-day refresh expiry, `client_secret_basic`. The stub warns
  loudly so it can never be mistaken for real credentials.
- **Token refresh / keep-alive.** The refresh-token flow is
  stubbed; the 7-day refresh expiry is not yet handled. The
  intended backend is `docker-credential-pass` (GPG `pass` store).
- **Real tickers / cap / window.** The values in
  `config/policies/` (symbols `VOO`/`SCHD`, cap $1000, window 28
  days) are **examples only**, labelled in the files. They are not
  decisions.
