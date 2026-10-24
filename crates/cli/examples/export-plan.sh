#!/bin/sh
# 导出计划：只生成"要导出哪一段消息"的计划，绝不落账。
# 用法：./export-plan.sh g-demo-1 1 200
set -eu

conv=${1:?用法: export-plan.sh <会话id> <from_seq> <to_seq>}
from=${2:-1}
to=${3:-200}

cli=${WXLIKE_CLI:-wxlike-cli}

# --dry-run：出计划（含规范参数摘要）后退出 0，不发请求、不写数据
"$cli" --json --dry-run msg list \
  --param "conv=$conv" --param "from_seq=$from" --param "to_seq=$to"

# 人复核上面这份计划后，去掉 --dry-run 再执行；导出的每一页都带 seq 区间，
# 便于与 UI 上的同一段对话逐条对照。
