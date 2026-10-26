#!/usr/bin/env bash
# G10 — 打包新版 Web UI（apps/desktop-web）：npm ci -> vite build（内含 tsc 把关），
# 然后打印产物体积。Tauri 宿主通过 ui/src-tauri/tauri.conf.new-ui.json 加载这里的产物。
#
# 产物：apps/desktop-web/dist/（vite 默认 outDir，sourcemap 开着——桌面端排障要用）
#
# 编辑本文件必须用 LF（.gitattributes：*.sh text eol=lf）。
set -euo pipefail

cd "$(dirname "$0")/.."
cd apps/desktop-web

if [ ! -d node_modules ]; then
  echo "==> 安装前端依赖"
  npm ci
fi

echo "==> 类型检查 + 打包（npm run build = tsc --noEmit && vite build）"
npm run build

DIST="$PWD/dist"
if [ ! -d "$DIST" ]; then
  echo "FAIL: $DIST 不存在（build 没产出？）" >&2
  exit 1
fi

echo "==> 产物体积"
total="$(du -sb "$DIST" | cut -f1)"
printf '    dist 总计   %s B（约 %s KB）\n' "$total" "$((total / 1024))"
echo "    最大的 5 个文件："
find "$DIST" -type f -printf '%s\t%P\n' | sort -rn | head -5 | while IFS=$'\t' read -r size name; do
  printf '      %8s B  %s\n' "$size" "$name"
done

echo "OK: $DIST 已生成"
