#!/usr/bin/env bash
# issue #158 的逃生阀：默认宿主已经是新 UI（ui/src-tauri/tauri.conf.json 的
# frontendDist = ../../apps/desktop-web/dist）。要重出**旧界面**的桌面包，走这里。
#
# 旧 UI 没有被删除：源码在 ui/src/，esbuild 产物在 ui/dist/，宿主配置在
# ui/src-tauri/tauri.conf.old-ui.json（内容就是切默认之前的那份 tauri.conf.json，
# frontendDist 仍指 ../dist = ui/dist）。
#
#   bash scripts/build-old-ui.sh              # 只准备 ui/dist 并打印打包命令
#   bash scripts/build-old-ui.sh --bundle     # 顺手真的跑 tauri build（重，本机慎用）
#
# 可选环境变量：OLD_UI_BUNDLES（默认全部）传给 --bundles。
#
# 编辑本文件必须用 LF（.gitattributes：*.sh text eol=lf）。
set -euo pipefail

cd "$(dirname "$0")/.."
REPO="$PWD"

BUNDLE=0
if [ "${1:-}" = "--bundle" ]; then
  BUNDLE=1
fi

echo "==> 旧 UI 前端（ui/ -> ui/dist，Tauri frontendDist 的 ../dist）"
cd "$REPO/ui"
if [ ! -d node_modules ]; then
  echo "    安装依赖：npm ci"
  npm ci
fi
npm run build

if [ ! -f "$REPO/ui/dist/app.js" ]; then
  echo "FAIL: ui/dist/app.js 不存在（ui 的 esbuild 没产出？）" >&2
  exit 1
fi

CMD=(npx --yes @tauri-apps/cli@2.9.0 build --ci --config src-tauri/tauri.conf.old-ui.json)
if [ -n "${OLD_UI_BUNDLES:-}" ]; then
  CMD+=(--bundles "$OLD_UI_BUNDLES")
fi

if [ "$BUNDLE" = "1" ]; then
  echo "==> 打包旧 UI 桌面安装器：${CMD[*]}"
  "${CMD[@]}"
else
  echo "==> 产物就绪：$REPO/ui/dist"
  echo "    在 ui/ 目录下执行即可出旧 UI 的安装器："
  echo "    ${CMD[*]}"
fi
echo "OK: 旧 UI 逃生阀（frontendDist=../dist）；默认的新 UI 宿主请用 bash scripts/build-web-v2.sh"
