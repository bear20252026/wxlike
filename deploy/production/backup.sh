#!/usr/bin/env bash
# Consistent SQLite backup + media archive + manifest.
#
#   deploy/production/backup.sh --db /srv/wxlike/wxlike.db \
#     --media /srv/wxlike/media --out /var/backups/wxlike
#
# SQLite is copied with `.backup`, not cp: a live WAL database copied byte-wise
# can be torn. The copy is then integrity-checked before it is accepted.
set -euo pipefail

DB=""
MEDIA=""
OUT=""
LABEL="manual"
while [ $# -gt 0 ]; do
  case "$1" in
    --db) DB="$2"; shift 2 ;;
    --media) MEDIA="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --label) LABEL="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$DB" ] && [ -n "$OUT" ] || { echo "usage: backup.sh --db FILE --out DIR [--media DIR] [--label NAME]" >&2; exit 2; }
[ -f "$DB" ] || { echo "database not found: $DB" >&2; exit 1; }
command -v sqlite3 >/dev/null || { echo "sqlite3 is required" >&2; exit 1; }

# Git Bash on Windows runs a native sqlite3 that does not understand /tmp paths.
sqlpath() { if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi; }

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
DEST="$OUT/${LABEL}-${STAMP}"
mkdir -p "$DEST"
STARTED="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

sqlite3 "$(sqlpath "$DB")" ".backup '$(sqlpath "$DEST/wxlike.db")'"
INTEGRITY="$(sqlite3 "$(sqlpath "$DEST/wxlike.db")" 'PRAGMA integrity_check;')"
[ "$INTEGRITY" = "ok" ] || { echo "backup failed integrity_check: $INTEGRITY" >&2; exit 1; }

SCHEMA_VERSION="$(sqlite3 "$(sqlpath "$DEST/wxlike.db")" 'PRAGMA user_version;')"
USERS="$(sqlite3 "$(sqlpath "$DEST/wxlike.db")" "SELECT count(*) FROM users;" 2>/dev/null || echo 0)"
MESSAGES="$(sqlite3 "$(sqlpath "$DEST/wxlike.db")" "SELECT count(*) FROM messages;" 2>/dev/null || echo 0)"

if [ -n "$MEDIA" ] && [ -d "$MEDIA" ]; then
  tar -czf "$DEST/media.tar.gz" -C "$MEDIA" .
  MEDIA_SHA="$(sha256sum "$DEST/media.tar.gz" | cut -d' ' -f1)"
  MEDIA_BYTES="$(wc -c < "$DEST/media.tar.gz" | tr -d ' ')"
else
  MEDIA_SHA=""
  MEDIA_BYTES=0
fi

DB_SHA="$(sha256sum "$DEST/wxlike.db" | cut -d' ' -f1)"
DB_BYTES="$(wc -c < "$DEST/wxlike.db" | tr -d ' ')"
FINISHED="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

python3 - "$(sqlpath "$DEST/manifest.json")" <<PY
import json, sys
payload = {
    "schema_version": 1,
    "label": "${LABEL}",
    "started_at": "${STARTED}",
    "finished_at": "${FINISHED}",
    "database": {"path": "wxlike.db", "sha256": "${DB_SHA}", "bytes": int("${DB_BYTES}"), "schema_version": int("${SCHEMA_VERSION}" or 0)},
    "media": {"path": "media.tar.gz" if "${MEDIA_SHA}" else None, "sha256": "${MEDIA_SHA}" or None, "bytes": int("${MEDIA_BYTES}")},
    "counts": {"users": int("${USERS}" or 0), "messages": int("${MESSAGES}" or 0)},
    "integrity_check": "${INTEGRITY}",
}
with open(sys.argv[1], "w", encoding="utf-8") as fh:
    json.dump(payload, fh, indent=2)
    fh.write("\n")
PY

echo "backup ok: $DEST"
echo "  db=${DB_BYTES}B sha256=${DB_SHA} users=${USERS} messages=${MESSAGES} schema=${SCHEMA_VERSION}"
[ -n "$MEDIA_SHA" ] && echo "  media=${MEDIA_BYTES}B sha256=${MEDIA_SHA}"
echo "  RPO is the backup schedule, not this script; see deploy/runbooks/backup-restore.md"
