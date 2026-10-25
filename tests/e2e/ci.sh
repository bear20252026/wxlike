#!/usr/bin/env bash
# CI driver for the native server: covers happy path, error paths, timeouts,
# a real restart on the same SQLite file, and the fault/privilege suites.
#
#   tests/e2e/ci.sh                     # builds are done by the workflow
#   WXLIKE_SERVER_BIN=target/debug/wxlike-server tests/e2e/ci.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${WXLIKE_SERVER_BIN:-$ROOT/target/debug/wxlike-server}"
PORT="${WXLIKE_E2E_PORT:-18099}"
BASE="http://127.0.0.1:${PORT}"
TMP="$(mktemp -d)"
EVIDENCE="${WXLIKE_E2E_EVIDENCE:-$TMP/evidence}"
ADMIN_TOKEN="e2e-$(date +%s)-$RANDOM"
PYTHON="${PYTHON:-python3}"
mkdir -p "$EVIDENCE"
PID=""

if [ ! -x "$BIN" ]; then
  echo "server binary not found: $BIN" >&2
  exit 1
fi

stop_server() {
  if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
    kill -TERM "$PID"
    for _ in $(seq 1 100); do
      kill -0 "$PID" 2>/dev/null || break
      sleep 0.1
    done
    kill -0 "$PID" 2>/dev/null && kill -KILL "$PID" || true
    wait "$PID" 2>/dev/null || true
  fi
  PID=""
}

cleanup() {
  stop_server
  if [ -z "${WXLIKE_E2E_EVIDENCE:-}" ]; then rm -rf "$TMP"; fi
}
trap cleanup EXIT

start_server() {
  WXLIKE_DB="$TMP/wxlike.db" \
  WXLIKE_MEDIA_DIR="$TMP/media" \
  WXLIKE_HTTP="127.0.0.1:${PORT}" \
  WXLIKE_QUIC="127.0.0.1:19443" \
  WXLIKE_MQ=memory \
  WXLIKE_DEV_INSECURE=1 \
  WXLIKE_DEV_FAUCET=1 \
  WXLIKE_ADMIN_TOKEN="$ADMIN_TOKEN" \
  WXLIKE_LIVE_INGEST_SECRET="e2e-ingest-$RANDOM" \
    "$BIN" >"$TMP/server.log" 2>&1 &
  PID=$!
  for _ in $(seq 1 120); do
    if curl --noproxy '*' -fsS "$BASE/health" >/dev/null 2>&1; then return 0; fi
    if ! kill -0 "$PID" 2>/dev/null; then
      echo "[e2e] server exited during startup" >&2
      cat "$TMP/server.log" >&2
      return 1
    fi
    sleep 0.25
  done
  echo "[e2e] server did not become healthy" >&2
  cat "$TMP/server.log" >&2
  return 1
}

run_e2e() {
  "$PYTHON" "$ROOT/tests/e2e/run.py" \
    --base-url "$BASE" --admin-token "$ADMIN_TOKEN" \
    --suite "$1" --evidence-dir "$EVIDENCE"
}

echo "[e2e] starting native server on $BASE"
start_server
run_e2e all
run_e2e restart-seed

echo "[e2e] SIGTERM, restart on the same database"
stop_server
start_server
run_e2e restart-recovery

echo "[e2e] fault and privilege suites"
# restart-during-send is covered here by restart-seed/restart-recovery; the
# docker --restart-cmd variant is exercised by containers/server/smoke.sh.
for suite in privileges migration; do
  "$PYTHON" "$ROOT/tests/fault/run.py" \
    --base-url "$BASE" --admin-token "$ADMIN_TOKEN" \
    --suite "$suite" --evidence-dir "$EVIDENCE" \
    --server-bin "$BIN"
done

echo "[e2e] passed; evidence in $EVIDENCE"
