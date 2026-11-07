#!/bin/sh
# tools/check-generated.sh — re-render the Pkl configs and compare
# with the checked-in generated JSON. Skips (passes) when `pkl` is
# not installed, so it never fails a build for lack of the CLI;
# the Docker proxy build, where Pkl IS copied in, fails hard instead.
set -eu
if ! command -v pkl >/dev/null 2>&1; then
  echo "check-generated: pkl not installed, skipping"
  exit 0
fi
ROOT="$(dirname "$0")/.."
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
pkl eval "$ROOT/config/policies/schwab-policies.pkl" -f json > "$tmp/policies.json"
pkl eval "$ROOT/config/policies/allowlist.pkl" -f json > "$tmp/allowlist.json"
diff -u "$ROOT/config/generated/policies.json" "$tmp/policies.json"
diff -u "$ROOT/config/generated/allowlist.json" "$tmp/allowlist.json"
echo "check-generated: generated JSON matches the Pkl sources"
