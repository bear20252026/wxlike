#!/usr/bin/env bash
# 在 macOS（含 Mac mini / Apple Silicon）上原生部署 wxlike 服务端。
#
# 做四件事：编译 → 装二进制 → 渲染 launchd 配置 → 启动并自检。
# 全程不需要 sudo：二进制放 ~/.local/bin，数据放 ~/Library/Application Support。
#
#   ./deploy/macos/install.sh              # 安装/升级
#   ./deploy/macos/install.sh --uninstall  # 停止并移除服务（保留数据）
#
# 需要：Xcode Command Line Tools（`xcode-select --install`）与 Rust 工具链。
set -euo pipefail

LABEL="com.wxlike.server"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN_DIR="$HOME/.local/bin"
BIN="$BIN_DIR/wxlike-server"
DATA="$HOME/Library/Application Support/wxlike"
LOGS="$HOME/Library/Logs/wxlike"
PLIST_SRC="$REPO_ROOT/deploy/macos/$LABEL.plist"
PLIST_DST="$HOME/Library/LaunchAgents/$LABEL.plist"
HTTP_BIND="${WXLIKE_HTTP:-0.0.0.0:8099}"
QUIC_BIND="${WXLIKE_QUIC:-0.0.0.0:9443}"

say() { printf '\033[1;36m[wxlike]\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m[wxlike]\033[0m %s\n' "$*" >&2; exit 1; }

if [[ "${1:-}" == "--uninstall" ]]; then
  say "停止并移除 launchd 服务（数据目录保留：$DATA）"
  launchctl bootout "gui/$UID/$LABEL" 2>/dev/null || launchctl unload "$PLIST_DST" 2>/dev/null || true
  rm -f "$PLIST_DST"
  say "已移除。二进制仍在 $BIN，需要的话手动删除。"
  exit 0
fi

# ---- 前置检查
command -v cargo >/dev/null || die "找不到 cargo：先装 Rust（https://rustup.rs）"
xcode-select -p >/dev/null 2>&1 || die "缺 Xcode Command Line Tools：xcode-select --install"
[[ "$(uname -s)" == "Darwin" ]] || die "这个脚本只用于 macOS；Linux 用 deploy/linux/install.sh"

say "编译 release 版本（首次会久一点，vendored OpenSSL 要编）"
cd "$REPO_ROOT"
cargo build --release -p wxlike-server

mkdir -p "$BIN_DIR" "$DATA" "$DATA/media" "$LOGS" "$HOME/Library/LaunchAgents"
install -m 0755 "$REPO_ROOT/target/release/wxlike-server" "$BIN"

say "渲染 launchd 配置 → $PLIST_DST"
sed -e "s|__BIN__|$BIN|g" \
    -e "s|__DATA__|$DATA|g" \
    -e "s|__LOGS__|$LOGS|g" \
    -e "s|__HTTP__|$HTTP_BIND|g" \
    -e "s|__QUIC__|$QUIC_BIND|g" \
    "$PLIST_SRC" > "$PLIST_DST"

# 先卸再装：launchd 不会自动重载被改过的 plist
launchctl bootout "gui/$UID/$LABEL" 2>/dev/null || true
launchctl bootstrap "gui/$UID" "$PLIST_DST" || launchctl load -w "$PLIST_DST"

say "等待服务起来…"
for i in $(seq 1 30); do
  if curl -fsS "http://127.0.0.1:${HTTP_BIND##*:}/health" >/dev/null 2>&1; then
    say "健康检查通过（第 ${i} 次）"
    break
  fi
  sleep 0.5
  [[ $i -eq 30 ]] && die "30 次自检都没通，看日志：$LOGS/server.err.log"
done

cat <<EOF

已启动。
  二进制   $BIN
  数据     $DATA（wxlike.db 与 media/）
  日志     $LOGS/server.out.log
  接口     http://127.0.0.1:${HTTP_BIND##*:}  /health  /endpoints  /metrics
  媒体     UDP ${QUIC_BIND##*:}（QUIC）· 通话另需放行 SFU 端口段

常用命令：
  launchctl kickstart -k gui/$UID/$LABEL     # 重启
  launchctl print gui/$UID/$LABEL            # 看状态
  tail -f "$LOGS/server.err.log"             # 看日志
  ./deploy/macos/install.sh --uninstall      # 卸载（保留数据）

局域网/公网访问：macOS 防火墙需放行该二进制；公网请在前面放反向代理并配 TLS。
EOF
