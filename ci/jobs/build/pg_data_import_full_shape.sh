#!/usr/bin/env bash
# G00-04 S4 full-shape import: run the real resumable importer over the empty
# 110-table snapshot. Zero-row COPY still exercises every target's DELETE,
# COPY/INSERT, read-back and sequence-reset path, including the compatibility
# views and PostgreSQL-only foreign keys.
set -euo pipefail

: "${DATABASE_URL:?DATABASE_URL is required}"
SQLITE_FILE="$(mktemp /tmp/wxlike-shape-XXXXXX.db)"
WORK_DIR="$(mktemp -d /tmp/wxlike-shape-work-XXXXXX)"
trap 'rm -f "$SQLITE_FILE"; rm -rf "$WORK_DIR"' EXIT

PYTHONPATH=composition/data-import python3 - "$SQLITE_FILE" <<'PY'
import sys
from pathlib import Path

from wximport.snapshot import build_snapshot_database

tables = build_snapshot_database(
    Path("crates/platform/tests/schema_snapshot.txt"), Path(sys.argv[1])
)
print(f"shape source: {len(tables)} tables")
PY

psql -X "$DATABASE_URL" -v ON_ERROR_STOP=1 -q \
  -c "DELETE FROM import_run WHERE run_id = 'ci-shape'"

python3 composition/data-import/import_all.py \
  --sqlite "$SQLITE_FILE" \
  --dsn "$DATABASE_URL" \
  --work-dir "$WORK_DIR" \
  --run-id ci-shape \
  --max-window-seconds 300

psql -X "$DATABASE_URL" -v ON_ERROR_STOP=1 -q <<'SQL'
DO $$
BEGIN
    IF (SELECT count(*) FROM import_table WHERE run_id = 'ci-shape') <> 107 THEN
        RAISE EXCEPTION 'full-shape: expected 107 planned tables';
    END IF;
    IF EXISTS (
        SELECT 1 FROM import_table WHERE run_id = 'ci-shape' AND status <> 'verified'
    ) THEN
        RAISE EXCEPTION 'full-shape: not every table verified';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM import_run WHERE run_id = 'ci-shape' AND status = 'verified'
    ) THEN
        RAISE EXCEPTION 'full-shape: run not verified';
    END IF;
END $$;
SQL
