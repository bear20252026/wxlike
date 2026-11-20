#!/usr/bin/env bash
# G01-03：把 crates/client-web 打成可发布的 JS/wasm 包。
#
# 产物落在 packages/wasm-client（wasm-pack 生成，不进 git），供
# apps/desktop-web 以 `file:` 依赖消费；G10 的 adapters 侧接线见 issue #106。
# wasm-pack 会一并产出 package.json / *.js / *.d.ts / *.wasm。
#
# 用法：
#   bash scripts/build-wasm.sh          # release（CI 用）
#   bash scripts/build-wasm.sh --dev    # dev profile（本地调试）
#
# 为什么单独一个脚本：wasm-pack 的产物名/目录/自检是契约的一部分，
# 消费侧（npm `file:` 依赖）按这个布局解析；散在各处手敲容易漂。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$ROOT/packages/wasm-client"
OUT_NAME="wxlike-core"
PROFILE="release"
if [[ "${1:-}" == "--dev" ]]; then
  PROFILE="dev"
fi

if ! command -v wasm-pack >/dev/null 2>&1; then
  echo "缺 wasm-pack：https://rustwasm.github.io/wasm-pack/installer/" >&2
  echo "（CI 的 wasm 作业已固定版本安装，本地开发才需要自己装）" >&2
  exit 2
fi

# wasm-pack 自己会 cargo build --target wasm32-unknown-unknown，缺 target 时报错
# 很隐晦；这里先拦一道给明确指引。
if ! rustup target list --installed 2>/dev/null | grep -qx 'wasm32-unknown-unknown'; then
  echo "缺 wasm32 target：rustup target add wasm32-unknown-unknown" >&2
  exit 2
fi

wasm-pack build "$ROOT/crates/client-web" \
  --target web \
  --out-dir "$OUT_DIR" \
  --out-name "$OUT_NAME" \
  --"$PROFILE"

# 产物自检：少任何一件都说明打包链断了，别让消费侧拿到半个包。
shopt -s nullglob
wasms=("$OUT_DIR"/*.wasm)
js=("$OUT_DIR"/*.js)
dts=("$OUT_DIR"/*.d.ts)
if [[ ! -f "$OUT_DIR/package.json" || ${#wasms[@]} -lt 1 || ${#js[@]} -lt 1 || ${#dts[@]} -lt 1 ]]; then
  # 用 ${OUT_DIR} 显式划界：macOS 自带 bash 3.2 会把紧跟其后的全角括号
  # 首字节当成变量名的一部分，`$OUT_DIR（` 会报 unbound variable。
  echo "产物不完整：${OUT_DIR}（需要 package.json + .wasm + .js + .d.ts）" >&2
  exit 1
fi

echo "已产出 ${OUT_DIR}（wasm-pack 生成，勿手改；--${PROFILE}）"
