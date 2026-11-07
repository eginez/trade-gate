# trade-gate

A policy-gated reverse proxy for scheduled purchases.

The proxy sits between an untrusted script and a broker API. The script
talks plain HTTP to the proxy; the proxy inspects every request against a
permits-only policy, injects the real credential, and re-encrypts the
request to the broker with the broker's real certificate. No MITM, no
fake certificates, no credential ever inside the script's container.

## What it is

- **Proxy, all Rust.** A reverse proxy (Pingora, `ProxyHttp`) that
  fail-closes: every request the script initiates gets a result —
  success passed through, or an explicit `403` (gate / not allowlisted,
  with a JSON reason), `409` (duplicate buy in the rolling window),
  `400` (invalid JSON), or `502`/`504` (upstream timed out / never
  answered). Nothing is silently dropped, no empty `200`, no hang.
  - GETs stream through Pingora normally.
  - POSTs are store-and-forwarded: Pingora 0.9 consumes a body read
    in `request_filter` and never replays it upstream (retry
    buffering is enabled only after `request_filter`, and there is
    no public API to inject a body back). So the gate buffers the
    small (< 64 KiB) POST body, evaluates it against the Pkl policy,
    and forwards it itself with a hyper client. Verified
    end-to-end by `test-script/probe.py`.
- **Policy, all Pkl.** Pkl is the policy language. There is no Cedar
  here — no `cedar-policy` crate, no `.cedar` output. Pkl schemas in
  `config/schemas/` define what a policy *can* be; Pkl configs in
  `config/policies/` are the data that fills them. `pkl eval` renders
  them to JSON in `config/generated/`; the Rust proxy loads that JSON
  and evaluates requests against it, permits-only, deny-by-default.
  Everything not listed as a permit is forbidden automatically —
  there is no forbids list.
- **Sandbox, Docker.** The script runs in a container on an
  internal-only Docker network and can only reach `proxy:8080`. The
  proxy is the only container with internet. See `docker/`.

## What is NOT built yet

- The trading script. That comes last. This repo has only the proxy,
  the config parsing, and a *test* script that exercises the network
  policies against a mock upstream.
- Real broker OAuth. The credential store is a stub interface
  (`proxy/src/credentials.rs`): it returns a clearly-marked stub token
  and warns loudly. No real App Key / Secret / tokens anywhere in
  this repo.
- Real token refresh. The refresh-token flow is stubbed; the
  7-day refresh expiry is not yet handled.

## Layout

```
proxy/                  Rust proxy (Pingora)
  src/main.rs           server bootstrap, env config
  src/config.rs         serde structs mirroring the Pkl schemas
  src/policy.rs         Pkl policy evaluator, permits-only
  src/gate.rs           Pingora ProxyHttp gate (fail-closed)
  src/db.rs             SQLite rolling-window buy history
  src/credentials.rs    credential-store stub interface
config/
  schemas/              Pkl schemas ONLY (PolicySet.pkl, Allowlist.pkl)
  policies/             Pkl configs ONLY (policies.pkl, allowlist.pkl)
  generated/            pkl eval JSON output, checked in
docker/
  docker-compose.yml    proxy + test-script, two networks
  proxy/Dockerfile      multi-stage: Pkl eval at build time only
  test-script/Dockerfile
test-script/            policy exerciser (NOT the trading script)
  probe.py              hits the proxy, asserts 200 / 403 / 409 / 400
  mock_upstream.py      fake broker API on localhost, no internet
```

Example values in the configs (symbols `VOO`, `SCHD`, `maxDollarsPerRun`
1000, rolling window 28 days) are **examples only**, clearly labelled
in the files. They are not decisions — the ticker list, the per-run cap,
and the window definition are still open and are adjusted by editing
`config/policies/`, not code.

## Build

Bazel is canonical (see `MODULE.bazel`):

```sh
bazel build //...
bazel test //...
```

Cargo also works as a fallback (same sources, same tests):

```sh
cargo test
cargo build --release -p trade-gate-proxy
```

> Note: Bazel was not available on the machine this skeleton was
> first built on, so the Bazel build is **not verified** here — the
> Cargo build and tests are. `bazel build //...` is the first thing
> to verify on a machine with Bazel installed.

## Run the policy tests

```sh
# Unit tests (Rust, no network, no Docker)
cargo test

# End-to-end policy probe against a mock upstream (no real broker, no Docker)
python3 test-script/probe.py
# or, with Docker (not verified on the build machine — no Docker there):
#   docker compose -f docker/docker-compose.yml up --build \
#     --abort-on-container-exit --exit-code-from test-script
```

The probe starts a mock broker on `127.0.0.1:18099`, starts the proxy
on `127.0.0.1:18080` pointing at the mock, and asserts:

- allowed `GET` quote / account / orders → forwarded, `200`
- allowed `POST previewOrder` with a valid BUY body → forwarded
- allowed `POST orders` with a valid BUY body → `201`, order id in
  `Location`, then `GET /orders/{id}` confirms it
- second buy of the same symbol in the window → `409`
- `POST orders` with SELL, a non-allowlisted symbol, a wrong session,
  or an extra field → `403`
- `DELETE` / `PUT` orders → `403`
- invalid JSON → `400`
- unknown path → `403`

## Configuration

The upstream host is a **mandatory startup argument** — there is no
default and no broker host is hardcoded in the binary:

```sh
trade-gate-proxy --upstream-host <host:port>
# e.g. --upstream-host 127.0.0.1:18099   (mock, in tests)
```

Starting without it exits with code 2 and prints usage.

Environment (proxy):

| Variable | Default | Meaning |
|---|---|---|
| `PROXY_LISTEN` | `127.0.0.1:8080` | address the proxy listens on |
| `UPSTREAM_TLS` | `true` | TLS to upstream (`false` for the mock) |
| `POLICIES_PATH` | `config/generated/policies.json` | rendered Pkl permits |
| `ALLOWLIST_PATH` | `config/generated/allowlist.json` | rendered Pkl allowlist |
| `DB_PATH` | `buys.db` | SQLite rolling-window DB (proxy only) |
| `CREDENTIAL_STORE` | `stub` | credential backend; only `stub` exists for now |
