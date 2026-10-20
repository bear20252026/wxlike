#!/usr/bin/env bash
# A5 — 打包网页端：tsc 把关 → esbuild 打 bundle → 内容寻址文件名 → 生成 index.html
#
# 产物：ui/dist-web/{index.html,assets/app.<hash>.js,assets/styles.<hash>.css}
# 缓存策略（服务端 server/src/web_static.rs 落地，这里只保证文件名可缓存）：
#   * index.html         no-store —— 壳必须每次最新，否则用户拿到旧 bundle 引用
#   * assets/*.<hash>.*  immutable + 一年 —— 文件名即内容摘要，改一个字节就换名
#
# 编辑本文件必须用 LF（§3.1：deploy/**、*.sh 用 LF）。
set -euo pipefail

cd "$(dirname "$0")/.."
SIZE_GATE=$((400 * 1024)) # A5 验收：gzip 后 < 400 KB

version="$(node -p "require('./ui/package.json').version")"

cd ui
# absolute: the script cd-s into ui/, so a relative OUT would point at ui/ui
OUT="$PWD/dist-web"
if [ ! -d node_modules ]; then
  echo "==> 安装前端依赖"
  npm ci
fi

echo "==> 类型检查"
npx tsc --noEmit

echo "==> 打包 app.js（版本 $version）"
rm -rf "$OUT"
mkdir -p "$OUT/assets"
# __VERSION__ 是编译期常量：网页端没有 Tauri 的 runtime 版本接口
npx esbuild src/web/entry.ts \
  --bundle --format=iife --minify --target=es2022 \
  --define:__VERSION__="\"web-$version\"" \
  --legal-comments=none \
  --outfile="$OUT/assets/app.js"

echo "==> 复制样式"
cp src/styles.css "$OUT/assets/styles.css"

echo "==> 内容寻址"
app_hash="$(sha256sum "$OUT/assets/app.js" | cut -c1-12)"
css_hash="$(sha256sum "$OUT/assets/styles.css" | cut -c1-12)"
mv "$OUT/assets/app.js" "$OUT/assets/app.$app_hash.js"
mv "$OUT/assets/styles.css" "$OUT/assets/styles.$css_hash.css"

sed -e "s/__APP_JS__/app.$app_hash.js/" -e "s/__STYLES_CSS__/styles.$css_hash.css/" \
  src/web/index.html > "$OUT/index.html"

raw="$(wc -c < "$OUT/assets/app.$app_hash.js" | tr -d ' ')"
gz="$(gzip -c -9 "$OUT/assets/app.$app_hash.js" | wc -c | tr -d ' ')"
css_kb=$(( $(wc -c < "$OUT/assets/styles.$css_hash.css") / 1024 ))
echo "==> 产物"
printf '    app.js      %s B (raw)  %s B (gzip)\n' "$raw" "$gz"
printf '    styles.css  %s KB\n' "$css_kb"
printf '    index.html  %s B\n' "$(wc -c < "$OUT/index.html" | tr -d ' ')"

if [ "$gz" -gt "$SIZE_GATE" ]; then
  echo "FAIL: gzip 后 $gz B 超过 $SIZE_GATE B 上限" >&2
  exit 1
fi
echo "OK: $OUT 已生成（gzip $gz B < $SIZE_GATE B）"
