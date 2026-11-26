#!/usr/bin/env bash
# G00-04 S4 full-schema census: build an empty SQLite database from the reviewed
# 110-table snapshot, then prove every planned source table has a writable
# PostgreSQL target shape. No row is copied here; failures name the migration
# that must be fixed before a real snapshot import is allowed.
set -euo pipefail

: "${DATABASE_URL:?DATABASE_URL is required}"
OUT="$(mktemp /tmp/wxlike-census-XXXXXX.json)"
trap 'rm -f "$OUT"' EXIT

PYTHONPATH=composition/data-import python3 composition/data-import/census.py \
  --snapshot crates/platform/tests/schema_snapshot.txt \
  --dsn "$DATABASE_URL" \
  --output "$OUT"

python3 - "$OUT" <<'PY'
import json
import sys

payload = json.load(open(sys.argv[1], encoding="utf-8"))
assert payload["ok"], payload["failures"]
assert payload["tables"] == 108, payload["tables"]
assert payload["verified"] == 108, payload["verified"]
assert payload["skipped"], "wallets/ledger must stay in M013"
assert len(payload["failures"]) == 0
print("G00-04 full-schema census: OK")
PY
