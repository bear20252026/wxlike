#!/usr/bin/env bash
# 在 Linux 上原生部署 wxlike 服务端（systemd）。
#
#   sudo ./deploy/linux/install.sh              # 安装/升级
#   sudo ./deploy/linux/install.sh --uninstall  # 停止并移除（保留数据）
#
# 会创建系统用户 wxlike，数据放 /var/lib/wxlike，二进制放 /usr/local/bin。
set -euo pipefail

SERVICE="wxlike-server"
USER_NAME="wxlike"
BIN="/usr/local/bin/wxlike-server"
DATA="/var/lib/wxlike"
UNIT_SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$SERVICE.service"
UNIT_DST="/etc/systemd/system/$SERVICE.service"
HTTP_BIND="${WXLIKE_HTTP:-0.0.0.0:8099}"
QUIC_BIND="${WXLIKE_QUIC:-0.0.0.0:9443}"

say() { printf '\033[1;36m[wxlike]\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m[wxlike]\033[0m %s\n' "$*" >&2; exit 1; }

[[ $EUID -eq 0 ]] || die "请用 sudo 运行（要写 /usr/local/bin 与 /etc/systemd/system）"

if [[ "${1:-}" == "--uninstall" ]]; then
  say "停止并移除 systemd 服务（数据保留在 $DATA）"
  systemctl disable --now "$SERVICE" 2>/dev/null || true
  rm -f "$UNIT_DST"
  systemctl daemon-reload
  say "已移除。用户 $USER_NAME 与数据目录 $DATA 保留，按需手动删除。"
  exit 0
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
command -v cargo >/dev/null || die "找不到 cargo：先装 Rust（https://rustup.rs）"

say "编译 release 版本"
cd "$REPO_ROOT"
cargo build --release -p wxlike-server

id -u "$USER_NAME" >/dev/null 2>&1 || {
  say "创建系统用户 $USER_NAME"
  useradd --system --home "$DATA" --shell /usr/sbin/nologin "$USER_NAME"
}

install -d -o "$USER_NAME" -g "$USER_NAME" "$DATA" "$DATA/media"
install -m 0755 "$REPO_ROOT/target/release/wxlike-server" "$BIN"

say "写 systemd unit → $UNIT_DST"
sed -e "s|__USER__|$USER_NAME|g" \
    -e "s|__BIN__|$BIN|g" \
    -e "s|__DATA__|$DATA|g" \
    -e "s|__HTTP__|$HTTP_BIND|g" \
    -e "s|__QUIC__|$QUIC_BIND|g" \
    "$UNIT_SRC" > "$UNIT_DST"
chmod 0644 "$UNIT_DST"

systemctl daemon-reload
systemctl enable --now "$SERVICE"

say "等待服务起来…"
for i in $(seq 1 30); do
  if curl -fsS "http://127.0.0.1:${HTTP_BIND##*:}/health" >/dev/null 2>&1; then
    say "健康检查通过"
    break
  fi
  sleep 0.5
  [[ $i -eq 30 ]] && die "自检失败：journalctl -u $SERVICE -n 100 --no-pager"
done

cat <<EOF

已启动。
  二进制   $BIN
  数据     $DATA（wxlike.db 与 media/）
  日志     journalctl -u $SERVICE -f
  接口     http://127.0.0.1:${HTTP_BIND##*:}  /health  /endpoints  /metrics
  媒体     UDP ${QUIC_BIND##*:}（QUIC）· 通话另需放行 SFU 端口段

防火墙（ufw 示例）：
  ufw allow 8099/tcp && ufw allow 9443/udp
EOF
