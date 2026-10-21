#!/usr/bin/env bash
# 重新生成移动端绑定。
#
#   crates/ffi/regenerate.sh [release]
#
# 产出两处：
#   crates/ffi/generated/kotlin/  —— B1 验收用的 Kotlin（不提交，.gitignore）
#   apps/mobile/lib/src/rust/     —— Flutter 直接 import 的 Dart（提交进仓库）
#
# 先 build 再 generate：proc-macro 模式的 UniFFI 没有 .udl 文件，接口形状是
# 从**编译产物**里读出来的（`--library`），所以必须先有 cdylib。
set -euo pipefail

cd "$(dirname "$0")/../.."
PROFILE="${1:-debug}"
if [[ "$PROFILE" == "release" ]]; then
  CARGO_FLAGS=(--release)
  PROFILE_DIR=release
else
  CARGO_FLAGS=()
  PROFILE_DIR=debug
fi

TARGET_DIR="${CARGO_TARGET_DIR:-target}"
case "$(uname -s)" in
  Linux*) LIB="$TARGET_DIR/$PROFILE_DIR/libwxlike_ffi.so" ;;
  Darwin*) LIB="$TARGET_DIR/$PROFILE_DIR/libwxlike_ffi.dylib" ;;
  *) LIB="$TARGET_DIR/$PROFILE_DIR/wxlike_ffi.dll" ;;
esac

echo "==> build cdylib ($PROFILE)"
cargo build -p wxlike-ffi --features bindgen,dart "${CARGO_FLAGS[@]}"

KOTLIN_OUT="crates/ffi/generated/kotlin"
DART_OUT="apps/mobile/lib/src/rust"
mkdir -p "$KOTLIN_OUT" "$DART_OUT"

echo "==> kotlin 绑定 → $KOTLIN_OUT"
cargo run -q -p wxlike-ffi --features bindgen --bin uniffi-bindgen -- \
  generate --library "$LIB" --language kotlin --out-dir "$KOTLIN_OUT"

echo "==> dart 绑定 → $DART_OUT"
# --config 必须显式给：library mode 下 uniffi-bindgen-dart 不会自己去 crate 根
# 读 uniffi.toml，而 `library_name` 决定符号前缀，默认值会让绑定每个调用都在
# DynamicLibrary.lookup 上失败（见 crates/ffi/uniffi.toml）。
cargo run -q -p wxlike-ffi --features bindgen,dart --bin uniffi-bindgen-dart -- \
  generate "$LIB" --out-dir "$DART_OUT" --config crates/ffi/uniffi.toml

echo "==> dart format"
dart format "$DART_OUT" >/dev/null 2>&1 || true
echo "完成：$(ls "$KOTLIN_OUT" | tr '\n' ' ')| $(ls "$DART_OUT" | tr '\n' ' ')"
