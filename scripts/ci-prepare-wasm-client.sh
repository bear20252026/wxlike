#!/usr/bin/env bash
# CI 前置：apps/desktop-web 的 npm 依赖含 `file:../../packages/wasm-client`，
# 而那是 wasm-pack 的产物、不入库（见 .gitignore）。任何要装 desktop-web
# 依赖的作业（web-v2 / desktop / desktop-package / desktop-release），在
# `npm ci` 之前都必须先跑这个脚本把产物生成出来，否则 npm 找不到 file:
# 目标直接失败。
#
# 幂等：产物已在（例如同一 job 里先跑过 wasm 作业）就跳过。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/packages/wasm-client/package.json"
VERSION="v0.13.1"
# 与 wasm 作业同一钉法（v0.13.1）：pkg 的 package.json / .d.ts 形状随版本变，
# 消费侧跟着漂。
BIN_DIR="${WASM_PACK_BIN_DIR:-$HOME/.local/bin}"

if [[ -f "$OUT" ]]; then
  echo "packages/wasm-client 已就绪，跳过"
  exit 0
fi

# 从官方预编译资产安装。资产清单见 wasm-pack v0.13.1 release；
# 注意没有 aarch64-apple-darwin，arm64 macOS 走 Rosetta 跑 x86_64 版。
install_from_asset() {
  local asset="$1"
  local tmp
  tmp="$(mktemp -d)"
  curl -sSfL "https://github.com/rustwasm/wasm-pack/releases/download/${VERSION}/${asset}" \
    | tar -xz -C "$tmp"
  mkdir -p "$BIN_DIR"
  local bin
  bin="$(find "$tmp" -maxdepth 2 -type f -name 'wasm-pack*' ! -name '*.tar.gz' | head -n 1)"
  if [[ -z "$bin" ]]; then
    echo "wasm-pack 资产里找不到可执行文件：$asset" >&2
    exit 1
  fi
  # 保留原文件名：Windows 资产里是 wasm-pack.exe。
  cp "$bin" "$BIN_DIR/"
  chmod +x "$BIN_DIR/$(basename "$bin")" || true
  rm -rf "$tmp"
}

install_wasm_pack() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$os" in
    Linux)
      case "$arch" in
        x86_64) install_from_asset "wasm-pack-${VERSION}-x86_64-unknown-linux-musl.tar.gz" ;;
        aarch64) install_from_asset "wasm-pack-${VERSION}-aarch64-unknown-linux-musl.tar.gz" ;;
        *) echo "不支持的 Linux 架构：$arch" >&2; return 1 ;;
      esac
      ;;
    Darwin)
      # v0.13.1 只发布了 x86_64-apple-darwin；arm64 runner 由 Rosetta 转译。
      install_from_asset "wasm-pack-${VERSION}-x86_64-apple-darwin.tar.gz"
      ;;
    MINGW*|MSYS*|CYGWIN*)
      install_from_asset "wasm-pack-${VERSION}-x86_64-pc-windows-msvc.tar.gz"
      ;;
    *)
      echo "不支持的平台：$os" >&2
      return 1
      ;;
  esac
}

if ! command -v wasm-pack >/dev/null 2>&1; then
  install_wasm_pack
  export PATH="$BIN_DIR:$PATH"
fi
command -v wasm-pack >/dev/null 2>&1 || {
  echo "wasm-pack 不可用（安装失败）" >&2
  exit 1
}

rustup target add wasm32-unknown-unknown
bash "$ROOT/scripts/build-wasm.sh"
