# Development

## Builds

**Bazel is canonical.** `bazel build //...` is the build to keep
green; `cargo test` is the verified fallback.

```sh
bazel build //...          # canonical build (5 targets)
bazel test //...           # canonical tests (2 targets)
cargo test                 # Rust unit tests, 17 tests
cargo build --release -p trade-gate-proxy
python3 test-script/probe.py   # end-to-end policy probe vs mock, 18 checks
python3 demo/demo.py           # human-readable demo vs mock (local only)
./tools/check-generated.sh     # re-render Pkl, diff vs checked-in JSON
```

Verified 2026-11-07: `bazel build //...` and `bazel test //...`
(2/2: `//:check_generated`, `//proxy:trade_gate_proxy_test`)
pass on the Linux build VM; `cargo test` 17/17; `probe.py`
18/18. The stale "Bazel not verified" note that used to live in
the README and `MODULE.bazel` was wrong after that run.

### Toolchain pins

- **Bazel 9.3.2** (via bazelisk). `MODULE.bazel` pins
  `rules_rust 0.70.0`, `rules_shell 0.6.1`, `platforms 1.0.0`,
  Rust **1.99.0**, edition 2021. `MODULE.bazel.lock` is committed
  — it pins the resolved module versions.
- **Rust 1.99.0** in `MODULE.bazel` *and* in
  `docker/proxy/Dockerfile` (`rust:1.99-bookworm` builder). Keep
  them in step: `lock_api 0.5.0` (via `parking_lot 0.13`) uses
  `ptr_as_ref_unchecked`, only stabilized in early 2026
  (~1.98/1.99), so rustc 1.85 fails on it with E0658.
- **Cargo** is the fallback on machines without Bazel. Same
  sources, same tests. `Cargo.lock` is committed — Bazel's
  `crate_universe` reads it.

### The Bazel fixes (why these lines exist)

Each of these was a real build failure on Bazel 9.3.2; do not
"clean them up" without re-running `bazel build //...`:

- `rules_rust 0.61.0 → 0.70.0`: Bazel 9 removed
  `linking_context.linkstamps`, which 0.61 calls in
  `rustc.bzl` `collect_deps` — analysis fails. 0.63 moved
  linkstamps to `linker_inputs`; 0.70 is the current release
  tested against Bazel 9.
- `rules_shell 0.6.1` + `platforms 1.0.0`: Bazel 9 removed the
  shell rules from `@bazel_tools`, so `sh_test` must load from
  `@rules_shell//shell:sh_test.bzl`.
- `proc_macro_deps` in `proxy/BUILD.bazel`: rules_rust 0.70
  enforces the split strictly — `async-trait` (a proc-macro)
  in `deps` fails analysis.
- `crate.from_cargo` manifests `["//:Cargo.toml"]` only: per
  rules_rust, only the workspace `Cargo.toml` is required.
- `compile_data = ["//:generated_config"]` on `rust_test`: the
  test modules load `config/generated/*.json` with `include_str!`,
  which rustc resolves at **compile time**. The files were only
  under `data` (runtime runfiles, invisible to rustc), so test
  compilation failed. `data` stays for the runtime
  `POLICIES_PATH`/`ALLOWLIST_PATH` defaults.
- Hermetic cmake + ninja (`rules_foreign_cc` 0.17.0, binary mode):
  `libz-ng-sys` builds zlib-ng's C code through cmake, and cmake
  needs a build program. Both now come from Bazel-managed
  prebuilt toolchains — no system cmake, make, or ninja is
  required on any machine. `MODULE.bazel` registers
  `tools.cmake(mode = "binary", version = "3.31.12")` and
  `tools.ninja(mode = "binary", version = "1.13.2")`, and a
  `crate.annotation` for `libz-ng-sys` hands the toolchains to its
  build script (`CMAKE`, `CMAKE_GENERATOR=Ninja`,
  `CMAKE_MAKE_PROGRAM`). One subtlety: the `cmake` crate never
  reads `CMAKE_MAKE_PROGRAM` from the environment, and cmake only
  accepts it as a `-D` cache entry — so a small patch in
  `third_party/patches/` makes the crate's build script forward it
  as a define (and it points at the real ninja binary, not the
  `$(NINJA)` wrapper, which would need `REAL_NINJA` in the action
  env). This replaced the old macOS `.bazelrc`
  workaround, which pointed Bazel at Homebrew's cmake
  (`CMAKE=/opt/homebrew/bin/cmake`) and ran build scripts outside
  the sandbox — that file is deleted. The only host tools left
  are the C compiler/linker from the system C++ toolchain.
- `check_generated` genrule: snapshot pushes do not preserve
  file modes, so `tools/check-generated.sh` arrives non-executable.
  The `genrule` copies it and `chmod +x` the copy; `sh_test`
  runs the generated file. (Known risk: the script computes
  `ROOT` from `$0`, which points into bazel-out runfiles under a
  test run — the checked-in test passing does not prove the path
  logic is robust; if `check_generated` starts failing under
  Bazel, look there first.)

### Pkl

Pkl runs at **authoring/build time only** — there is no official
Rust embedding. `pkl eval` renders JSON to `config/generated/`
(checked in). The hand-generated JSON in the repo is in exact
`pkl eval -f json` shape and marked as such; the Pkl CLI is not
installed on the build VM, so `tools/check-generated.sh` exits 0
by skipping there. See `docs/policy.md` for the change workflow.

## CI

`.github/workflows/ci.yml` runs `bazel build //...` and
`bazel test //...` on `ubuntu-latest` and `macos-latest`, on
**pushes to `main` only** (no `pull_request` trigger). Setup is
`bazel-contrib/setup-bazel@0.19.0` with three caches:
`bazelisk-cache` (the Bazel binary, keyed on `.bazelversion`),
`disk-cache` (build outputs, per OS + workflow), and
`repository-cache` (module/crate downloads, shared across
workflows). `.bazelversion` pins Bazel **9.3.2** — the version the
build was verified against. No cmake is installed in CI: it comes
from the hermetic prebuilt toolchains (see above).
First run on a cold cache downloads the toolchain and crates
(~20 min); later runs reuse the caches.

## Running the tests

```sh
# Unit tests (Rust, no network, no Docker)
cargo test

# End-to-end probe against a mock upstream (no real broker, no Docker)
python3 test-script/probe.py
```

The probe starts a mock broker on `127.0.0.1:18099`, starts the
proxy on `127.0.0.1:18080` pointing at the mock, and asserts the
behaviour in the README: allowed GETs forwarded, valid BUY
preview/place forwarded (`201` + `Location`, then `GET` confirms
it), second buy in the window `409`, SELL / non-allowlisted
symbol / wrong session / extra field `403`, DELETE / PUT `403`,
invalid JSON `400`, unknown path `403`.

With Docker (needs a real Docker daemon — see below):

```sh
docker compose -f docker/docker-compose.yml up --build \
  --abort-on-container-exit --exit-code-from test-script
docker compose -f docker/docker-compose.yml down -v
```

The demo is local-only and never pushed:

```sh
python3 demo/demo.py        # prints the 11-check walkthrough
```

## Docker

`docker/docker-compose.yml` runs proxy + mock + test-script
(fresh DB each run). The proxy image (`docker/proxy/Dockerfile`)
is multi-stage: Pkl eval at build time only, Rust builder, then a
minimal runtime. The test-script image is Python + the probe and
mock.

**Known limitation:** Docker containers cannot start on the Linux
build VM — the host kernel has no bridge module and the process
caps lack `CAP_NET_ADMIN` (`docker run` fails with `setns` /
`operation not permitted` in every network mode, including
`--network none`). `docker pull` also fails (Go client TLS
handshake timeout through the egress proxy). The Compose file as
written cannot run there. Options: record the local run (the
demo GIF in `demo/` was made that way), or run Compose on a
machine with a real Docker daemon.

## Conventions

- Schemas and configs are separate: a change to what a policy
  *can* be goes in `config/schemas/`; a change to the actual
  permits or values goes in `config/policies/`. Re-render with
  `pkl eval` and commit the regenerated JSON in
  `config/generated/` in the same change.
- Example values in the configs (symbols, per-run cap, window
  days) are **examples only**, labelled in the files. They are
  not decisions.
- No secrets anywhere in this repo: no App Key/Secret, no tokens,
  no account numbers, no emails, no net-worth figures. The
  credential store is a stub that warns loudly.
- Fail closed at startup: a bad policy/allowlist, a missing
  `--upstream-host`, or an unknown credential store refuses to
  start instead of starting permissive.
- Pingora 0.9 quirk, documented in `gate.rs` and the README: a
  body read in `request_filter` is consumed and never replayed
  upstream. GETs stream through Pingora; POSTs are
  store-and-forwarded in `request_filter` with a hyper client. Do
  not "simplify" this back.
- Verify before claiming: `cargo test`, the probe, and the demo
  must all pass after a change. A screenshot (or a probe run)
  wins over a code reading.
- Broker-neutral naming: no broker name in file names, class
  names, or comments. The upstream host is a mandatory
  `--upstream-host <host:port>` startup argument — no default,
  nothing hardcoded.
- Do not build the trading script without the owner's explicit
  word. Do not push `demo/` — it is local-only (see
  `.gitignore`). Do not push anything without the owner's word;
  snapshot pushes go through the GitHub snapshot process, and
  the snapshot tool cannot delete files, so renames need
  separate `delete_file` calls for the old paths.
- Do not commit `target/`, `*.db`, `bazel-*` symlinks, or
  anything under `demo/`.

## Open items

- Real broker OAuth + `docker-credential-pass` backend
  (`proxy/src/credentials.rs` is a stub).
- Refresh-token rotation / weekly keep-alive (7-day refresh
  expiry is not handled; the `refresh` Compose profile just
  starts the proxy for now).
- Real tickers, per-run cap, and rolling-window definition
  (see `docs/policy.md` — examples only today).
- The trading script, built last, on the owner's word only.
- The hermetic cmake/ninja setup is verified on the Linux build
  VM only so far — a clean `bazel build //...` on the owner's Mac
  (where the old failure was) is still unconfirmed.
