#!/usr/bin/env bash
# Full container smoke: start read-only, exercise the real HTTP domain, restart
# the same volume and prove auth, messages, friendship and wallet survived.
#
#   ./containers/server/smoke.sh wxlike-server:test
#
# Evidence is written to $WXLIKE_SMOKE_EVIDENCE when set (CI uploads it);
# otherwise a temporary directory is used and removed.
set -euo pipefail

IMAGE="${1:?usage: smoke.sh <image>}"
NAME="wxlike-smoke-$$"
PORT="${WXLIKE_SMOKE_PORT:-18099}"
VOL="wxlike-smoke-data-$$"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PYTHON="${PYTHON:-python3}"
ADMIN_TOKEN="${WXLIKE_ADMIN_TOKEN:-smoke-$RANDOM-$RANDOM}"

if [ -n "${WXLIKE_SMOKE_EVIDENCE:-}" ]; then
  EVIDENCE="$WXLIKE_SMOKE_EVIDENCE"
  KEEP_EVIDENCE=1
else
  EVIDENCE="$(mktemp -d)"
  KEEP_EVIDENCE=0
fi
mkdir -p "$EVIDENCE"

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  docker volume rm "$VOL" >/dev/null 2>&1 || true
  if [ "$KEEP_EVIDENCE" = 0 ]; then rm -rf "$EVIDENCE"; fi
}
trap cleanup EXIT

docker volume create "$VOL" >/dev/null
start() {
  docker run -d --name "$NAME" \
    --read-only --tmpfs /tmp:rw,noexec,nosuid,size=32m \
    --user 10001:10001 \
    -e WXLIKE_DEV_INSECURE=1 \
    -e WXLIKE_MQ=memory \
    -e WXLIKE_DB=/data/wxlike.db \
    -e WXLIKE_MEDIA_DIR=/data/media \
    -e WXLIKE_HTTP=0.0.0.0:8099 \
    -e WXLIKE_QUIC=0.0.0.0:9443 \
    -e WXLIKE_ADMIN_TOKEN="$ADMIN_TOKEN" \
    -e WXLIKE_DEV_FAUCET=1 \
    -p "127.0.0.1:${PORT}:8099" \
    -v "$VOL:/data" \
    "$IMAGE" >/dev/null
  for _ in $(seq 1 60); do
    if curl --noproxy '*' -fsS "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  docker logs "$NAME" >&2 || true
  return 1
}

run_suite() {
  "$PYTHON" "$ROOT/tests/e2e/run.py" \
    --base-url "http://127.0.0.1:${PORT}" \
    --admin-token "$ADMIN_TOKEN" \
    --suite "$1" \
    --evidence-dir "$EVIDENCE"
}

start
echo "[smoke] server healthy"
run_suite smoke
run_suite restart-seed

echo "[smoke] restarting container on the same volume"
docker stop --time 30 "$NAME" >/dev/null
docker rm "$NAME" >/dev/null
start
curl --noproxy '*' -fsS "http://127.0.0.1:${PORT}/health" >/dev/null
run_suite restart-recovery
echo "[smoke] passed; evidence in $EVIDENCE"
