#!/usr/bin/env bash
# 重新生成移动端绑定。
#
#   crates/ffi/regenerate.sh [release|debug] [--dart]
#
# 默认产出两处（原生移动端，ADR-29 D1）：
#   crates/ffi/generated/kotlin/  —— B1 验收用的 Kotlin（不提交，.gitignore）
#   crates/ffi/generated/swift/   —— D1 原生 iOS 壳用的 Swift（不提交，.gitignore）
#
# 加 --dart 才额外产 apps/mobile/lib/src/rust/ 的 Dart（Flutter 旧壳）。
# **默认不产 Dart**：D3 的 EventSink 回调接口不被第三方 uniffi-bindgen-dart
# 0.2.0 支持（生成的 Dart 编译即失败）。Flutter 壳退役过渡（disabled-jobs.json），
# Dart 链路仅在显式 --dart 且生成器恢复回调支持后才可用。
#
# 先 build 再 generate：proc-macro 模式的 UniFFI 没有 .udl 文件，接口形状是
# 从**编译产物**里读出来的（`--library`），所以必须先有 cdylib。
set -euo pipefail

cd "$(dirname "$0")/../.."
PROFILE="debug"
WITH_DART=0
for arg in "$@"; do
  case "$arg" in
    release|debug) PROFILE="$arg" ;;
    --dart) WITH_DART=1 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done
if [[ "$PROFILE" == "release" ]]; then
  CARGO_FLAGS=(--release)
  PROFILE_DIR=release
else
  CARGO_FLAGS=()
  PROFILE_DIR=debug
fi
# Dart 后端需要 dart feature；默认只 build bindgen（kotlin/swift 用官方后端）。
if [[ "$WITH_DART" == "1" ]]; then
  BUILD_FEATURES="bindgen,dart"
else
  BUILD_FEATURES="bindgen"
fi

TARGET_DIR="${CARGO_TARGET_DIR:-target}"
case "$(uname -s)" in
  Linux*) LIB="$TARGET_DIR/$PROFILE_DIR/libwxlike_ffi.so" ;;
  Darwin*) LIB="$TARGET_DIR/$PROFILE_DIR/libwxlike_ffi.dylib" ;;
  *) LIB="$TARGET_DIR/$PROFILE_DIR/wxlike_ffi.dll" ;;
esac

echo "==> build cdylib ($PROFILE, features=$BUILD_FEATURES)"
cargo build -p wxlike-ffi --features "$BUILD_FEATURES" "${CARGO_FLAGS[@]}"

KOTLIN_OUT="crates/ffi/generated/kotlin"
SWIFT_OUT="crates/ffi/generated/swift"
DART_OUT="apps/mobile/lib/src/rust"
mkdir -p "$KOTLIN_OUT" "$SWIFT_OUT"

echo "==> kotlin 绑定 → $KOTLIN_OUT"
cargo run -q -p wxlike-ffi --features bindgen --bin uniffi-bindgen -- \
  generate --library "$LIB" --language kotlin --out-dir "$KOTLIN_OUT"

echo "==> swift 绑定 → $SWIFT_OUT"
# D1：官方 UniFFI 后端原生支持 Swift（与 Kotlin 同一个 uniffi-bindgen）。
# iOS 壳消费这份绑定 + tools/codegen/out/swift/Contract.swift（D5 契约）。
# XCFramework 包装（lipo + xcodebuild）需 macOS 工具链，排在 P0 spike，不在本脚本。
cargo run -q -p wxlike-ffi --features bindgen --bin uniffi-bindgen -- \
  generate --library "$LIB" --language swift --out-dir "$SWIFT_OUT"

if [[ "$WITH_DART" == "1" ]]; then
  mkdir -p "$DART_OUT"
  echo "==> dart 绑定 → $DART_OUT"
  # --config 必须显式给：library mode 下 uniffi-bindgen-dart 不会自己去 crate 根
  # 读 uniffi.toml，而 `library_name` 决定符号前缀，默认值会让绑定每个调用都在
  # DynamicLibrary.lookup 上失败（见 crates/ffi/uniffi.toml）。
  # 注意：当前 uniffi-bindgen-dart 0.2.0 对 EventSink 回调生成非法 Dart，
  # 此步骤仅在生成器恢复回调支持后才可用（ADR-29 D1 退役过渡）。
  cargo run -q -p wxlike-ffi --features bindgen,dart --bin uniffi-bindgen-dart -- \
    generate "$LIB" --out-dir "$DART_OUT" --config crates/ffi/uniffi.toml
  echo "==> dart format"
  dart format "$DART_OUT" >/dev/null 2>&1 || true
  echo "完成：$(ls "$KOTLIN_OUT" | tr '\n' ' ')| $(ls "$SWIFT_OUT" | tr '\n' ' ')| $(ls "$DART_OUT" | tr '\n' ' ')"
else
  echo "完成：$(ls "$KOTLIN_OUT" | tr '\n' ' ')| $(ls "$SWIFT_OUT" | tr '\n' ' ')（Dart 已退役，加 --dart 显式启用）"
fi
