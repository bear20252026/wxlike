#!/usr/bin/env bash
# G10 — 打包新版 Web UI（apps/desktop-web）：npm ci -> vite build（内含 tsc 把关），
# 然后打印产物体积。Tauri 宿主的默认配置 ui/src-tauri/tauri.conf.json 直接加载
# 这里的产物（issue #158：默认宿主已经是新 UI，不再需要 --config）。
#
# 旧界面（ui/ 那套 esbuild 产物）没删：要打包旧 UI 用 bash scripts/build-old-ui.sh。
#
# 产物：apps/desktop-web/dist/（vite 默认 outDir；production 不带 sourcemap）
# 打包前会跑 scripts/check-tauri-host.mjs --require-built，钉住「宿主指得到这份产物、
# 且双 CSP 交集放行 core_dispatch」，否则就地失败而不是装出一个打不开的界面。
#
# 编辑本文件必须用 LF（.gitattributes：*.sh text eol=lf）。
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$PWD"
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

echo "==> 桌面宿主把关（frontendDist 对得上、双 CSP 放行桥）"
node "$REPO/scripts/check-tauri-host.mjs" --require-built

echo "OK: $DIST 已生成，且是默认 Tauri 宿主的 frontendDist"
