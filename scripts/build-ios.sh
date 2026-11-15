#!/usr/bin/env bash
# D1 / ADR-29 — 把 wxlike-ffi 打包成 iOS 可用的 XCFramework + UniFFI Swift 绑定。
#
#   bash scripts/build-ios.sh [release|debug]
#
# 产出：
#   crates/ffi/generated/ios/wxlike_ffi.xcframework  —— 三 target 静态库 fat 包
#   crates/ffi/generated/swift/                      —— UniFFI Swift 绑定 + modulemap
#
# 这是 P0 脚手架项：iOS 原生壳（apps/mobile-ios，待建）消费这份 XCFramework +
# Swift 绑定 + tools/codegen/out/swift/Contract.swift（D5 契约）。
#
# **只能在 macOS 上跑**（需要 rustup 的 iOS target、lipo、xcodebuild）。CI 的
# ios job 当前因 apps/mobile/ios 不存在而 NOT_APPLICABLE；本脚本是那条路径
# 接通后的构建入口，先在仓库里把步骤固化下来，避免真机阶段临时拼命令。
#
# 编辑本文件必须用 LF（.gitattributes：*.sh text eol=lf）。
set -euo pipefail

cd "$(dirname "$0")/.."
PROFILE="${1:-release}"
if [[ "$PROFILE" == "release" ]]; then
  CARGO_FLAGS=(--release)
  PROFILE_DIR=release
else
  CARGO_FLAGS=()
  PROFILE_DIR=debug
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "FAIL: build-ios.sh 只能在 macOS 上跑（需要 lipo / xcodebuild）。" >&2
  exit 1
fi

# 三个 iOS target：真机 arm64 + 模拟器 arm64/x86_64（Apple Silicon 模拟器是 arm64）。
DEVICE_TARGET="aarch64-apple-ios"
SIM_TARGETS=("aarch64-apple-ios-sim" "x86_64-apple-ios")

echo "==> 确认 rustup target 已安装"
for target in "$DEVICE_TARGET" "${SIM_TARGETS[@]}"; do
  if ! rustup target list --installed | grep -q "^$target$"; then
    echo "==> 安装 target $target"
    rustup target add "$target"
  fi
done

TARGET_DIR="${CARGO_TARGET_DIR:-target}"
LIB_NAME="libwxlike_ffi.a"

echo "==> 编译真机静态库（$DEVICE_TARGET）"
cargo build -p wxlike-ffi --features bindgen "${CARGO_FLAGS[@]}" --target "$DEVICE_TARGET"
DEVICE_LIB="$TARGET_DIR/$DEVICE_TARGET/$PROFILE_DIR/$LIB_NAME"

echo "==> 编译模拟器静态库（${SIM_TARGETS[*]}）"
SIM_LIBS=()
for target in "${SIM_TARGETS[@]}"; do
  cargo build -p wxlike-ffi --features bindgen "${CARGO_FLAGS[@]}" --target "$target"
  SIM_LIBS+=("$TARGET_DIR/$target/$PROFILE_DIR/$LIB_NAME")
done

OUT_DIR="crates/ffi/generated/ios"
mkdir -p "$OUT_DIR"
SIM_FAT="$OUT_DIR/libwxlike_ffi_sim.a"
echo "==> lipo 合并模拟器 fat 库"
lipo -create "${SIM_LIBS[@]}" -output "$SIM_FAT"

echo "==> 生成 UniFFI Swift 绑定（从真机编译产物读接口形状）"
SWIFT_OUT="crates/ffi/generated/swift"
mkdir -p "$SWIFT_OUT"
cargo run -q -p wxlike-ffi --features bindgen --bin uniffi-bindgen -- \
  generate --library "$DEVICE_LIB" --language swift --out-dir "$SWIFT_OUT"

echo "==> xcodebuild 打包 XCFramework"
XCFRAMEWORK="$OUT_DIR/wxlike_ffi.xcframework"
rm -rf "$XCFRAMEWORK"
# modulemap 用 UniFFI 生成的那份（wxlike_ffiFFI.modulemap）。
xcodebuild -create-xcframework \
  -library "$DEVICE_LIB" -headers "$SWIFT_OUT" \
  -library "$SIM_FAT" -headers "$SWIFT_OUT" \
  -output "$XCFRAMEWORK"

echo "OK: $XCFRAMEWORK 已生成"
echo "    Swift 绑定：$SWIFT_OUT"
echo "    下一步：iOS 壳工程以 binaryTarget 引入 XCFramework，源码 target 引入"
echo "    Swift 绑定 + tools/codegen/out/swift/Contract.swift（D5 契约）。"