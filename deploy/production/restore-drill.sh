#!/usr/bin/env bash
# Restore a backup into a scratch directory and verify it; optionally boot the
# real server against it and wait for /health. Records measured RPO/RTO.
#
#   deploy/production/restore-drill.sh --backup /var/backups/wxlike/manual-... \
#     --out /tmp/restore-drill --server-bin target/release/wxlike-server
set -euo pipefail

BACKUP=""
OUT=""
SERVER_BIN=""
PORT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --backup) BACKUP="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --server-bin) SERVER_BIN="$2"; shift 2 ;;
    --port) PORT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$BACKUP" ] && [ -n "$OUT" ] || { echo "usage: restore-drill.sh --backup DIR --out DIR [--server-bin FILE]" >&2; exit 2; }
[ -f "$BACKUP/manifest.json" ] || { echo "manifest missing in $BACKUP" >&2; exit 1; }
command -v sqlite3 >/dev/null || { echo "sqlite3 is required" >&2; exit 1; }
sqlpath() { if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi; }

START_EPOCH="$(python3 -c 'import time;print(time.time())')"
DEST="$OUT/restored"
rm -rf "$DEST"
mkdir -p "$DEST"
cp "$BACKUP/wxlike.db" "$DEST/wxlike.db"
[ -f "$BACKUP/media.tar.gz" ] && tar -xzf "$BACKUP/media.tar.gz" -C "$DEST" || true

INTEGRITY="$(sqlite3 "$(sqlpath "$DEST/wxlike.db")" 'PRAGMA integrity_check;')"
[ "$INTEGRITY" = "ok" ] || { echo "restored database failed integrity_check: $INTEGRITY" >&2; exit 1; }

MANIFEST="$(sqlpath "$BACKUP/manifest.json")"
EXPECTED_DB_SHA="$(python3 -c "import json;print(json.load(open('$MANIFEST'))['database']['sha256'])")"
ACTUAL_DB_SHA="$(sha256sum "$DEST/wxlike.db" | cut -d' ' -f1)"
[ "$EXPECTED_DB_SHA" = "$ACTUAL_DB_SHA" ] || { echo "restored database hash mismatch" >&2; exit 1; }

USERS="$(sqlite3 "$(sqlpath "$DEST/wxlike.db")" "SELECT count(*) FROM users;" 2>/dev/null || echo 0)"
MESSAGES="$(sqlite3 "$(sqlpath "$DEST/wxlike.db")" "SELECT count(*) FROM messages;" 2>/dev/null || echo 0)"
EXPECTED_USERS="$(python3 -c "import json;print(json.load(open('$MANIFEST'))['counts']['users'])")"
EXPECTED_MESSAGES="$(python3 -c "import json;print(json.load(open('$MANIFEST'))['counts']['messages'])")"
[ "$USERS" = "$EXPECTED_USERS" ] || { echo "users $USERS != manifest $EXPECTED_USERS" >&2; exit 1; }
[ "$MESSAGES" = "$EXPECTED_MESSAGES" ] || { echo "messages $MESSAGES != manifest $EXPECTED_MESSAGES" >&2; exit 1; }

SERVER_STARTED=false
if [ -n "$SERVER_BIN" ]; then
  [ -x "$SERVER_BIN" ] || { echo "server binary not executable: $SERVER_BIN" >&2; exit 1; }
  if [ -z "$PORT" ]; then PORT="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"; fi
  QUIC_PORT="$(python3 -c 'import socket;s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
  WXLIKE_DB="$(sqlpath "$DEST/wxlike.db")" WXLIKE_MEDIA_DIR="$(sqlpath "$DEST")" WXLIKE_HTTP="127.0.0.1:$PORT" \
  WXLIKE_QUIC="127.0.0.1:$QUIC_PORT" WXLIKE_MQ=memory WXLIKE_DEV_INSECURE=1 \
    "$SERVER_BIN" >"$DEST/server.log" 2>&1 &
  PID=$!
  healthy=false
  for _ in $(seq 1 120); do
    if curl --noproxy '*' -fsS "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then healthy=true; break; fi
    kill -0 "$PID" 2>/dev/null || break
    sleep 0.25
  done
  kill -TERM "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
  [ "$healthy" = true ] || { echo "restored server never became healthy" >&2; cat "$DEST/server.log" >&2; exit 1; }
  SERVER_STARTED=true
fi

END_EPOCH="$(python3 -c 'import time;print(time.time())')"
python3 - "$MANIFEST" "$END_EPOCH" "$START_EPOCH" "$SERVER_STARTED" "$(sqlpath "$OUT/drill.json")" <<'PY'
import json, sys
from datetime import datetime, timezone
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
finished = datetime.strptime(manifest["finished_at"], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
restore_start = float(sys.argv[3])
payload = {
    "schema_version": 1,
    "backup": sys.argv[1],
    "restored_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "rpo_seconds": round(restore_start - finished.timestamp(), 1),
    "rto_ms": int((float(sys.argv[2]) - restore_start) * 1000),
    "server_started": sys.argv[4] == "true",
    "integrity_check": "ok",
    "counts": manifest["counts"],
}
with open(sys.argv[5], "w", encoding="utf-8") as fh:
    json.dump(payload, fh, indent=2)
    fh.write("\n")
PY

echo "restore drill ok: $OUT/drill.json"
cat "$OUT/drill.json"
