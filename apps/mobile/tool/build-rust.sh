#!/usr/bin/env bash
# 把 crates/ffi 交叉编译成 Android 的 .so，放进 jniLibs。
#
#   apps/mobile/tool/build-rust.sh [debug|release]
#
# 为什么需要这一步：Flutter 的 Dart 侧通过 `dart:ffi` 打开一个真实的
# `libwxlike_ffi.so`，而 Kotlin/Swift 那两条 UniFFI 后端在 Flutter 里用不上。
# 绑定文件本身（lib/src/rust/）由 `crates/ffi/regenerate.sh` 生成并提交，
# 这个脚本只产出**平台产物**，不进 git。
set -euo pipefail

cd "$(dirname "$0")/.."
PROFILE="${1:-debug}"
if [[ "$PROFILE" == "release" ]]; then
  CARGO_FLAGS=(--release)
else
  CARGO_FLAGS=()
fi

SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/d/software/Android/Sdk}}"
NDK="$(ls -1 "$SDK/ndk" | sort -V | tail -1)"
export ANDROID_NDK_HOME="$SDK/ndk/$NDK"
echo "==> NDK: $ANDROID_NDK_HOME"

OUT="android/app/src/main/jniLibs"
# debug 产物不剥符号的话一个 .so 就 58 MB，装到手机上 APK 直接破 100 MB。
# 这里只做"够调试用"的裁剪：留符号名（strip=symbols 去掉的是调试信息），
# 不动优化级别，所以 backtrace 里函数名还在。
export RUSTFLAGS="${RUSTFLAGS:-} -C strip=symbols -C debuginfo=0"
# arm64-v8a = 真机；x86_64 = Android 模拟器。armeabi-v7a 需要额外 target，先不做。
cargo ndk -t arm64-v8a -t x86_64 -o "$OUT" build -p wxlike-ffi "${CARGO_FLAGS[@]}"

for abi in arm64-v8a x86_64; do
  if [[ ! -f "$OUT/$abi/libwxlike_ffi.so" ]]; then
    echo "!! $OUT/$abi/libwxlike_ffi.so 没生成" >&2
    exit 1
  fi
  ls -l "$OUT/$abi/libwxlike_ffi.so"
done
echo "完成：$OUT"
