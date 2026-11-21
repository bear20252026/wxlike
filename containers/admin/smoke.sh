#!/usr/bin/env bash
# Adminx container smoke: bring up a real platform (server image) to produce the
# read-only platform DB, then start adminx against it and prove:
#   * /health is public and green (and declares writes_platform_db:false);
#   * reads need a credential (401 without, 200 with the static token);
#   * writes are refused without a credential (401) and with the static token
#     (403) — the static token cannot write, by design (docs/adminx.md §5).
#
#   ./containers/admin/smoke.sh wxlike-admin:test [wxlike-server:test]
#
# The platform token is deliberately the static token here: the write check
# stops at adminx's own writer() gate (403), before any upstream call. Proving
# the Admin-audience session path needs a real Admin session (PR #453); that is
# covered by crates/admin/tests/g02_09_live_evidence.rs, not by this smoke.
#
# Evidence is written to $WXLIKE_SMOKE_EVIDENCE when set (CI uploads it);
# otherwise a temporary directory is used and removed.
set -euo pipefail

ADMIN_IMAGE="${1:?usage: smoke.sh <admin-image> [server-image]}"
SERVER_IMAGE="${2:-wxlike-server:ci}"
NAME="wxlike-admin-smoke-$$"
SERVER_NAME="wxlike-admin-smoke-server-$$"
NET="wxlike-admin-smoke-net-$$"
VOL="wxlike-admin-smoke-data-$$"
PORT="${WXLIKE_ADMIN_SMOKE_PORT:-18090}"
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
  docker rm -f "$NAME" "$SERVER_NAME" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
  docker volume rm "$VOL" >/dev/null 2>&1 || true
  if [ "$KEEP_EVIDENCE" = 0 ]; then rm -rf "$EVIDENCE"; fi
}
trap cleanup EXIT

command -v docker >/dev/null || { echo "ERROR docker is not installed" >&2; exit 1; }
command -v curl >/dev/null || { echo "ERROR curl is not installed" >&2; exit 1; }

say() { printf '[smoke] %s\n' "$*"; }
fail() { printf 'ERROR %s\n' "$*" >&2; exit 1; }
expect() {
  local got="$1" want="$2" label="$3"
  if [ "$got" != "$want" ]; then
    fail "$label: got HTTP $got, want $want"
  fi
  say "$label: HTTP $got"
}
code() { curl --noproxy '*' -s -o /dev/null -w '%{http_code}' "$@"; }

docker network create "$NET" >/dev/null
docker volume create "$VOL" >/dev/null

# --- 平台：真的起一个 server，产出 adminx 要只读打开的那份 SQLite -----------
docker run -d --name "$SERVER_NAME" --network "$NET" \
  --read-only --tmpfs /tmp:rw,noexec,nosuid,size=32m \
  --user 10001:10001 \
  -e WXLIKE_DEV_INSECURE=1 \
  -e WXLIKE_MQ=memory \
  -e WXLIKE_DB=/data/wxlike.db \
  -e WXLIKE_MEDIA_DIR=/data/media \
  -e WXLIKE_HTTP=0.0.0.0:8099 \
  -e WXLIKE_QUIC=0.0.0.0:9443 \
  -v "$VOL:/data" \
  "$SERVER_IMAGE" >/dev/null

for _ in $(seq 1 60); do
  if docker exec "$SERVER_NAME" curl --noproxy '*' -fsS http://127.0.0.1:8099/health >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
docker exec "$SERVER_NAME" curl --noproxy '*' -fsS http://127.0.0.1:8099/health >/dev/null 2>&1 \
  || { docker logs "$SERVER_NAME" >&2 || true; fail "platform server did not become healthy"; }
say "platform server healthy"

# --- adminx：与 server 共享同一个 /data（不同文件），只读打开平台库 -----------
docker run -d --name "$NAME" --network "$NET" \
  --read-only --tmpfs /tmp:rw,noexec,nosuid,size=32m \
  --user 10001:10001 \
  -e WXLIKE_ADMIN_TOKEN="$ADMIN_TOKEN" \
  -e WXLIKE_ADMIN_PLATFORM_TOKEN="$ADMIN_TOKEN" \
  -e WXLIKE_DB=/data/wxlike.db \
  -e WXLIKE_ADMIN_DB=/data/adminx.db \
  -e WXLIKE_PLATFORM_URL=http://server:8099 \
  -e WXLIKE_ADMIN_BIND=0.0.0.0:8090 \
  -p "127.0.0.1:${PORT}:8090" \
  -v "$VOL:/data" \
  "$ADMIN_IMAGE" >/dev/null

for _ in $(seq 1 60); do
  if curl --noproxy '*' -fsS "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1; then break; fi
  sleep 1
done
curl --noproxy '*' -fsS "http://127.0.0.1:${PORT}/health" >/dev/null 2>&1 \
  || { docker logs "$NAME" >&2 || true; fail "adminx did not become healthy"; }
say "adminx healthy"

# /health 是公开面，且必须自报"不写平台库"。
health_body="$(curl --noproxy '*' -fsS "http://127.0.0.1:${PORT}/health")"
printf '%s\n' "$health_body" > "$EVIDENCE/adminx-health.json"
printf '%s' "$health_body" | grep -q '"writes_platform_db":false' \
  || fail "/health 未自报 writes_platform_db:false"
say "/health 公开且 writes_platform_db:false"

# adminx 自己的账号库真的落到了 /data（这是它唯一被允许写的文件）。
docker exec "$NAME" /bin/sh -c 'test -s /data/adminx.db' \
  || fail "adminx 账号库未写入 /data/adminx.db"
say "adminx.db 已写入 /data"

# 读面：无令牌 401，静态令牌 200。
expect "$(code "http://127.0.0.1:${PORT}/api/stats")" 401 "读面无令牌被拒"
expect "$(code -H "Authorization: Bearer $ADMIN_TOKEN" "http://127.0.0.1:${PORT}/api/stats")" 200 "读面带静态令牌可用"

# 写面：无令牌 401；静态令牌 403（它不对应任何人，写不出可追责留痕）。
expect "$(code -X POST "http://127.0.0.1:${PORT}/api/op/report.triage")" 401 "写面无令牌被拒"
expect "$(code -X POST -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H 'Content-Type: application/json' -d '{}' \
  "http://127.0.0.1:${PORT}/api/op/report.triage")" 403 "写面静态令牌被拒"

say "passed; evidence in $EVIDENCE"
