# AGENTS.md

How to work in this repo. Read this before changing anything; the
decisions below were made deliberately and are not to be re-litigated
by re-deriving them from scratch.

## What this is

`trade-gate` is a policy-gated reverse proxy for scheduled purchases.
An untrusted script talks plain HTTP to the proxy; the proxy inspects
every request against a permits-only policy, injects the real
credential, and re-encrypts the request to the broker with the
broker's real certificate. No MITM, no fake certificates, no
credential ever inside the script's container.

The trading script itself is NOT built yet, and is built last, on the
owner's word only. This repo has the proxy, the config parsing, a
policy probe, and a demo.

## Decisions that survive — do not reverse them without asking

- **Reverse proxy, not MITM.** The script knows it talks to the proxy.
- **Proxy is only a fail-safe.** Every call the script initiates gets
  a result: success passed through, or an explicit `403` (gate, with a
  JSON reason), `409` (duplicate buy in the rolling window), `400`
  (invalid JSON), or `502`/`504` (upstream timed out). Nothing is
  silently dropped, no empty `200`, no hang.
- **Policy is all Pkl.** No Cedar — no `cedar-policy` crate, no
  `.cedar` output. Pkl schemas in `config/schemas/` define what a
  policy *can* be; Pkl configs in `config/policies/` are the data.
  `pkl eval` renders JSON to `config/generated/`; Rust loads that JSON
  and evaluates permits-only, deny-by-default. There is no forbids
  list — everything not listed as a permit is forbidden automatically.
- **Broker-neutral naming.** No broker name in file names, class
  names, or comments. The upstream host is a mandatory
  `--upstream-host <host:port>` startup argument — no default, nothing
  hardcoded. The same goes for the demo: it never names a broker.
- **Rust first.** Proxy and (when built) client in Rust. Python is
  acceptable for test/demo scripts only.
- **Docker is the sandbox.** Script container on an internal-only
  network, can only reach `proxy:8080`. Proxy is the only container
  with internet. See `docker/docker-compose.yml`.
- **Rolling-window DB.** Only a `201` from upstream *plus* a confirmed
  `GET /orders/{id}` writes a buy row. Previews, 400s, and timeouts
  write an *attempt* row, not a buy. A second buy of the same
  `(account, symbol)` in the window is a `409` and is never forwarded.
- **Bazel is canonical, Cargo is the verified fallback.** `bazel
  build //...` is the build to keep green; `cargo test` is what was
  verified on machines without Bazel.

## Commands

```sh
cargo test                          # Rust unit tests (17 tests)
python3 test-script/probe.py        # end-to-end policy probe vs mock (18 checks)
python3 demo/demo.py                # human-readable demo vs mock
./tools/check-generated.sh          # re-render Pkl, diff vs checked-in JSON (skips if pkl absent)
bazel build //...                   # canonical build
bazel test //...                    # canonical tests
```

The probe/demo start a mock upstream and the proxy themselves on
localhost with a fresh temp DB. They pass `--upstream-host` explicitly.

## Layout

```
proxy/                  Rust proxy (Pingora, ProxyHttp)
  src/main.rs           startup: parses --upstream-host, loads policy/allowlist, serves
  src/gate.rs           the fail-closed gate (method/path/body, rolling window)
  src/policy.rs         Pkl policy evaluator, permits-only
  src/config.rs         serde structs mirroring the Pkl schemas
  src/db.rs             SQLite rolling-window buy history
  src/credentials.rs    credential-store stub interface (no real secrets)
config/
  schemas/              Pkl schemas ONLY (PolicySet.pkl, Allowlist.pkl)
  policies/             Pkl configs ONLY (policies.pkl, allowlist.pkl)
  generated/            pkl eval JSON output, checked in
docker/                 Compose + Dockerfiles (proxy, test-script, demo)
test-script/            policy exerciser (NOT the trading script)
demo/                   human-readable demo — LOCAL ONLY, never pushed (see below)
docs/                   architecture, policy, development notes
tools/check-generated.sh
```

## Conventions

- Schemas and configs are separate: a change to what a policy *can*
  be goes in `config/schemas/`; a change to the actual permits or
  values goes in `config/policies/`. Re-render with `pkl eval` and
  commit the regenerated JSON in `config/generated/` in the same change.
- Example values in the configs (symbols, per-run cap, window days)
  are **examples only**, labelled in the files. They are not decisions.
- No secrets anywhere in this repo: no App Key/Secret, no tokens, no
  account numbers, no emails, no net-worth figures. The credential
  store is a stub that warns loudly.
- Fail closed at startup: a bad policy/allowlist, a missing
  `--upstream-host`, or an unknown credential store refuses to start
  instead of starting permissive.
- Pingora 0.9 quirk, documented in `gate.rs` and the README: a body
  read in `request_filter` is consumed and never replayed upstream.
  GETs stream through Pingora; POSTs are store-and-forwarded in
  `request_filter` with a hyper client. Do not "simplify" this back.
- Verify before claiming: `cargo test`, the probe, and the demo must
  all pass after a change. A screenshot (or a probe run) wins over a
  code reading.

## Do not

- Do not build the trading script without the owner's explicit word.
- Do not push `demo/` — it is local-only (see `.gitignore`). The demo
  recording (`.cast`, `.gif`) lives here for the owner, not the repo.
- Do not push anything without the owner's word. Snapshot pushes go
  through the GitHub snapshot process; the snapshot tool cannot delete
  files, so renames need separate `delete_file` calls for the old paths.
- Do not add a broker name, a default upstream host, a forbids list,
  or a Cedar dependency. Do not reintroduce MITM.
- Do not commit `target/`, `*.db`, or anything under `demo/`.
