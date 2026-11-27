#!/usr/bin/env bash
# C-04 真实浏览器验收：本地 ws fixture + 只跑 ws-fixture feature 下的 v3 重连用例。
#
# 普通 `wasm-pack test` 不依赖外部进程；本脚本给 CI/本地手动验收用。
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

probe() {
  python3 - <<'PY'
import socket
s = socket.socket()
s.settimeout(0.1)
try:
    s.connect(("127.0.0.1", 18765))
except OSError:
    raise SystemExit(1)
finally:
    s.close()
PY
}

if probe; then
  echo "FAIL: 127.0.0.1:18765 已被占用；先停掉旧的 ws fixture" >&2
  exit 1
fi

python3 ci/jobs/contract/ws_fixture.py &
FIXTURE_PID=$!
trap 'kill "$FIXTURE_PID" 2>/dev/null || true' EXIT

# 等 fixture 监听；最多 2 秒。
for _ in $(seq 1 20); do
  if probe; then break; fi
  sleep 0.1
done

wasm-pack test --headless --chrome crates/client-web --features ws-fixture
