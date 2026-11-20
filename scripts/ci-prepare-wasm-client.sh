#!/usr/bin/env bash
# CI 前置：apps/desktop-web 的 npm 依赖含 `file:../../packages/wasm-client`，
# 而那是 wasm-pack 的产物、不入库（见 .gitignore）。任何要装 desktop-web
# 依赖的作业（web-v2 / desktop / desktop-release），在 `npm ci` 之前都必须
# 先跑这个脚本把产物生成出来，否则 npm 找不到 file: 目标直接失败。
#
# 幂等：产物已在（例如同一 job 里先跑过 wasm 作业）就跳过。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/packages/wasm-client/package.json"

if [[ -f "$OUT" ]]; then
  echo "packages/wasm-client 已就绪，跳过"
  exit 0
fi

# wasm-pack 版本与 wasm 作业保持同一钉法（v0.13.1）：
# pkg 的 package.json / .d.ts 形状随版本变，消费侧跟着漂。
curl -sSfL https://github.com/rustwasm/wasm-pack/releases/download/v0.13.1/wasm-pack-v0.13.1-x86_64-unknown-linux-musl.tar.gz \
  | tar -xz -C /tmp
sudo install /tmp/wasm-pack-v0.13.1-x86_64-unknown-linux-musl/wasm-pack /usr/local/bin/wasm-pack

rustup target add wasm32-unknown-unknown
bash "$ROOT/scripts/build-wasm.sh"
