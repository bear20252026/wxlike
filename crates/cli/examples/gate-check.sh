#!/bin/sh
# 体检门禁：坏配置（无法解析的 scope、缺 token）直接退出非 0。
# 用法：WXLIKE_TOKEN=... ./gate-check.sh
set -eu

cli=${WXLIKE_CLI:-wxlike-cli}

echo "## doctor"
"$cli" --json doctor || {
    echo "doctor 发现问题，见上面的 problems 字段" >&2
    exit 1
}

echo "## 覆盖报告（未做项必须带负责组与 issue）"
"$cli" --json coverage

echo "## 动作目录（条数与域名）"
"$cli" --json list
