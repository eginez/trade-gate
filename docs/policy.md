# Policy

Policy is **all Pkl**. There is no Cedar here — no `cedar-policy`
crate, no `.cedar` output. Pkl schemas define what a policy *can*
be; Pkl configs are the data that fills them. `pkl eval` renders
them to JSON; the Rust proxy loads that JSON and evaluates
requests against it, **permits-only, deny-by-default**. Everything
not listed as a permit is forbidden automatically — there is **no
forbids list**.

## Files

| File | Role |
|---|---|
| `config/schemas/PolicySet.pkl` | Schema ONLY: what a policy can be. |
| `config/schemas/Allowlist.pkl` | Schema ONLY: what an allowlist can be. |
| `config/policies/policies.pkl` | Data ONLY: the 7 permits. |
| `config/policies/allowlist.pkl` | Data ONLY: symbols, cap, window. |
| `config/generated/policies.json` | `pkl eval` output, checked in. |
| `config/generated/allowlist.json` | `pkl eval` output, checked in. |

Schemas and configs are separate on purpose: a change to what a
policy *can* be goes in `config/schemas/`; a change to the actual
permits or values goes in `config/policies/`. Re-render with
`pkl eval` and commit the regenerated JSON in `config/generated/`
in the same change.

## The closed action union

`BrokerAction` in `PolicySet.pkl` is a closed union of exactly 7
actions. Anything else — a wildcard, `sell`, `delete_order` — is
a **Pkl compile error**, not a runtime denial:

| Action | Method | Path |
|---|---|---|
| `get_account_numbers` | GET | `/trader/v1/accounts/accountNumbers` |
| `get_account` | GET | `/trader/v1/accounts/{hash}` |
| `list_orders` | GET | `/trader/v1/accounts/{hash}/orders` |
| `get_order` | GET | `/trader/v1/accounts/{hash}/orders/{orderId}` |
| `get_quote` | GET | `/marketdata/v1/quotes` |
| `preview_order` | POST | `/trader/v1/accounts/{hash}/previewOrder` |
| `place_order` | POST | `/trader/v1/accounts/{hash}/orders` |

Paths are templates: literal segments must match exactly,
`{name}` captures exactly one segment. Segment count must match.
No `*`, no `**`, no regex. Paths must start with `/trader/v1/`
or `/marketdata/v1/` (enforced in Pkl by regex and again in Rust
at load time). `PolicySet.permits` is `isDistinctBy(action)` —
one permit per action, no duplicates.

The principal and the resource are not fields at all: the
principal is always the script, the resource is always the broker
API. They are hardcoded in the Rust evaluator.

## The body rule

Only `preview_order` and `place_order` carry a body rule
(`OrderBody` in the schema). It is a *rule*, not a concrete order:
it says which values are allowed.

- `session`: literal `"NORMAL"`
- `duration`: literal `"DAY"`
- `orderTypes`: `MARKET` and/or `LIMIT`
- `orderStrategyType`: literal `"SINGLE"`
- `instruction`: literal `"BUY"` — a SELL is unrepresentable
- `assetTypes`: `EQUITY` and/or `ETF`

The Rust evaluator (`proxy/src/policy.rs`) adds what Pkl cannot
express:

- Exactly one leg; unknown fields (top-level or in the leg) are
  rejected.
- `quantity` ≥ 1, whole shares only.
- Symbol must be in the allowlist (`SymbolNotAllowlisted`).
- Dollar cap: `quantity × live quote price ≤ cap`, where the cap
  is the permit's `maxDollars` if set, else
  `maxDollarsPerRun`. **No live price is a deny** — the script
  must quote first.
- `get_quote` allows only the `symbols` query parameter, with a
  value; anything else is denied.

## The allowlist

`Allowlist.pkl` — the named sets the permits refer to:

- `symbols`: non-empty, distinct. Checked against
  `orderLegCollection[0].instrument.symbol`.
- `maxDollarsPerRun`: integer > 0. Per-run cap.
- `rollingWindowDays`: integer > 0. A second buy of the same
  symbol for the same account inside this window is a `409`.

## Deny-by-default, in two layers

1. **Pkl schema** (closed action union, literal `"BUY"`, no
   wildcard / allow-all) — a bad policy is a Pkl compile error.
2. **Rust** (`proxy/src/policy.rs`, `proxy/src/config.rs`): no
   permit match, or any evaluation error, is a deny. Never fail
   open. Loading is strict (`deny_unknown_fields`), so a typo'd
   or extra field in the rendered JSON refuses to start the proxy
   instead of being silently ignored. An empty permits list, a
   duplicate action, an unknown action, or a bad method/path is a
   startup error.

## Deny reasons

The proxy always answers a denial with JSON
(`{"error": <code>, "message": ..., "status": ...}`), so the
script always knows why:

| Code | Status | Meaning |
|---|---|---|
| `no_route` | 403 | No permit matches this method + path. |
| `invalid_json` | 400 | Body is not valid JSON, or not a JSON object. |
| `body_mismatch` | 403 | A field fails the body rule, or an unknown field is present. |
| `symbol_not_allowlisted` | 403 | Symbol is not in the allowlist. |
| `over_cap` | 403 | `quantity × price` exceeds the cap, or no live price exists. |
| `duplicate_buy_in_window` | 409 | Same `(account, symbol)` already bought inside the window. (Raised by the gate, not the evaluator.) |
| `upstream_error` | 502 | Upstream did not answer. |

What the evaluator deliberately does **not** do (stays in Rust in
the gate): the rolling-window DB check, the live-price dollar cap
beyond the permit's own `maxDollars`, and token injection.

## Changing the policy

1. Edit `config/policies/policies.pkl` or
   `config/policies/allowlist.pkl`. (To change what a policy *can*
   be, edit `config/schemas/` instead.)
2. Re-render:
   ```sh
   pkl eval config/policies/policies.pkl -f json > config/generated/policies.json
   pkl eval config/policies/allowlist.pkl -f json > config/generated/allowlist.json
   # or: ./tools/check-generated.sh   (diffs rendered vs checked-in)
   ```
3. Commit the `.pkl` change **and** the regenerated JSON in the
   same change.
4. Verify: `cargo test`, `python3 test-script/probe.py`,
   `bazel test //...` — the Rust tests load the generated JSON
   with `include_str!`, so a stale or bad render fails the build
   instead of failing at 2am during a run.
5. Rebuild the proxy (the Docker proxy image renders Pkl at build
   time; see `docker/proxy/Dockerfile`).

`tools/check-generated.sh` re-renders and diffs. It **skips**
(passes) when `pkl` is not installed, so it never fails a build
for lack of the CLI; the Docker proxy build, where Pkl *is*
copied in, fails hard instead. Note: under `bazel test` the
script runs from runfiles, so the Bazel target runs a genrule
copy of it (see `BUILD.bazel`) rather than the source script —
snapshot pushes do not preserve the executable bit.

## Open decisions (examples, not decisions)

The values in `config/policies/` are **examples only**, labelled
in the files:

- Symbols `VOO`, `SCHD` — the real ticker list is undecided.
- `maxDollarsPerRun = 1000`, `place_order.maxDollars = 1000` —
  the real per-run cap is undecided.
- `rollingWindowDays = 28` — the window *definition* is not
  decided yet; the field is the shape of the decision, not the
  decision.

Adjust them in `config/policies/`, not in code.
