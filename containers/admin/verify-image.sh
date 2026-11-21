#!/usr/bin/env bash
# Verify the adminx image before it is allowed to be published.
#
#   ./containers/admin/verify-image.sh wxlike-admin:test
#
# 不变量与 server 镜像逐条相同（非 root 10001、read-only 根 fs、只有 /data
# 可写、无烘焙密钥、HEALTHCHECK、SIGTERM），检查实现复用
# containers/server/verify-image.sh；这里只提供 adminx 的二进制与一个
# 不需要数据库的探针（`--help`），因为 `--check` 会去开账号库。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

exec bash "$HERE/../server/verify-image.sh" \
  "${1:?usage: verify-image.sh <image>}" \
  /usr/local/bin/wxlike-admin \
  --help
