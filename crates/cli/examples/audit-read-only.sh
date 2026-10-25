#!/bin/sh
# 只读审计快照：只用 read 命令，不改任何数据。
# 用法：WXLIKE_TOKEN=... WXLIKE_AGENT_ALLOW_CONV='g-1' ./audit-read-only.sh
set -eu

: "${WXLIKE_TOKEN:?需要 WXLIKE_TOKEN（只读令牌即可）}"
cli=${WXLIKE_CLI:-wxlike-cli}

echo "## 我是谁"
"$cli" --json me info

echo "## 会话清单（白名单外的会话不会出现）"
"$cli" --json conv list

echo "## 权限判定（每条命令的允许/拒绝）"
"$cli" --json policy
