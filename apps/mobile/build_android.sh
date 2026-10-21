#!/usr/bin/env bash
# 一条命令出可安装的 Android 包。
#
#   apps/mobile/build_android.sh              # release，只 arm64（真机）
#   apps/mobile/build_android.sh --debug      # debug，含 x86_64（模拟器）
#
# ## 为什么需要这个脚本
#
# 仓库里**不提交**三样东西，它们都是可再生的生成物：
#
#   crates/ffi/generated/                        UniFFI 的 Kotlin 绑定（B1 验收用）
#   apps/mobile/lib/src/rust/                    UniFFI 的 Dart 绑定（Flutter 直接 import）
#   apps/mobile/android/.../jniLibs/*/libwxlike_ffi.so   Rust 交叉编译产物
#
# 所以 `git clone` 之后直接 `flutter build apk` 一定失败——`lib/src/service.dart`
# 第 17 行 `import 'rust/wxlike_ffi.dart';` 指向一个不存在的文件。
# 这不是"工程坏了"，而是**顺序问题**：先有 .so，才有绑定；先有绑定，才有 Dart 编译。
# 这个脚本按那个顺序把三样补出来。
#
# ## 依赖
#
#   flutter          （3.47 验证过）
#   cargo / cargo-ndk（`cargo install cargo-ndk`）
#   Android SDK + NDK（`ANDROID_HOME` 指向 SDK，NDK 在 `$ANDROID_HOME/ndk/*`）
#   rust target      `rustup target add aarch64-linux-android`
#                    （debug/模拟器还要 `x86_64-linux-android`）
#
# 缺哪个都会在下面明确报出来，而不是等到 gradle 阶段报一句看不懂的错。
set -euo pipefail

cd "$(dirname "$0")/../.."   # 仓库根

MODE="release"
ABIS=("arm64-v8a")
TARGETS=("aarch64-linux-android")
FLUTTER_PLATFORM="android-arm64"
GRADLE_TASK="assembleRelease"
FLUTTER_FLAGS=(--release)

for arg in "$@"; do
  case "$arg" in
    --debug)
      MODE="debug"
      ABIS=("arm64-v8a" "x86_64")
      TARGETS=("aarch64-linux-android" "x86_64-linux-android")
      FLUTTER_PLATFORM="android-arm64,android-x64"
      GRADLE_TASK="assembleDebug"
      FLUTTER_FLAGS=(--debug)
      ;;
    --emulator)
      # release 也想在模拟器上跑时，只加 x86_64
      ABIS+=("x86_64")
      TARGETS+=("x86_64-linux-android")
      FLUTTER_PLATFORM="${FLUTTER_PLATFORM},android-x64"
      ;;
    -h|--help)
      sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "未知参数：$arg（可用 --debug / --emulator / --help）" >&2
      exit 2
      ;;
  esac
done

# ---------------------------------------------------------------- 依赖检查
# 逐条检查并在第一条缺失处停下：一次报一个比一次报五个更容易照着修。
need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "缺依赖：$1 —— $2" >&2
    exit 1
  fi
}

need flutter "装 Flutter SDK 并把 bin 加进 PATH"
need cargo "装 Rust 工具链"
need cargo-ndk "cargo install cargo-ndk"

if [[ -z "${ANDROID_HOME:-}${ANDROID_SDK_ROOT:-}" ]]; then
  echo "缺环境变量 ANDROID_HOME（指向 Android SDK 目录）" >&2
  exit 1
fi

for t in "${TARGETS[@]}"; do
  if ! rustup target list --installed | grep -qx "$t"; then
    echo "缺 rust target：$t —— rustup target add $t" >&2
    exit 1
  fi
done

# ---------------------------------------------------------------- 1. 绑定
# `regenerate.sh` 会先 `cargo build -p wxlike-ffi --features bindgen,dart`，
# 再从编译产物里读接口形状（proc-macro 模式没有 .udl）。所以它自己就能跑。
echo "==> 1/3 生成 UniFFI 绑定（Dart + Kotlin）"
bash crates/ffi/regenerate.sh "$MODE"

# ---------------------------------------------------------------- 2. .so
# 不加 `--features bindgen,dart`：那会把 uniffi_bindgen 链进手机上的库。
echo "==> 2/3 交叉编译 libwxlike_ffi.so（${ABIS[*]}，$MODE）"
NDK_ARGS=()
for abi in "${ABIS[@]}"; do
  NDK_ARGS+=(-t "$abi")
done
CARGO_FLAGS=()
[[ "$MODE" == "release" ]] && CARGO_FLAGS+=(--release)

cargo ndk "${NDK_ARGS[@]}" \
  -o apps/mobile/android/app/src/main/jniLibs \
  build -p wxlike-ffi "${CARGO_FLAGS[@]}"

# release 的 .so 还带符号表，strip 掉能少一半以上。找不到 llvm-strip 就跳过——
# 大一点总比因为缺工具直接失败好。
if [[ "$MODE" == "release" ]]; then
  # 注意**不要**加 `-type f`：NDK 里 llvm-strip 是指向 llvm-strip-18 的符号链接。
  STRIP=$(find "${ANDROID_HOME:-$ANDROID_SDK_ROOT}/ndk" -name 'llvm-strip*' 2>/dev/null | sort | tail -1 || true)
  if [[ -n "$STRIP" && -x "$STRIP" ]]; then
    for abi in "${ABIS[@]}"; do
      so="apps/mobile/android/app/src/main/jniLibs/$abi/libwxlike_ffi.so"
      [[ -f "$so" ]] && "$STRIP" --strip-unneeded "$so"
    done
    echo "    strip 完成（$STRIP）"
  else
    echo "    警告：找不到 llvm-strip，保留未裁剪的 .so（体积会大一些）" >&2
  fi
fi

# ---------------------------------------------------------------- 3. APK
echo "==> 3/3 构建 APK（$GRADLE_TASK）"
cd apps/mobile
flutter build apk "${FLUTTER_FLAGS[@]}" --target-platform "$FLUTTER_PLATFORM"

APK="build/app/outputs/flutter-apk/app-${MODE}.apk"
if [[ -f "$APK" ]]; then
  echo
  echo "完成：$PWD/$APK"
  du -h "$APK" | awk '{print "体积：" $1}'
  echo "装到手机：adb install -r \"$APK\"（或直接把文件传到手机点击安装）"
else
  echo "构建结束但没找到 $APK —— 看上面的 gradle 输出" >&2
  exit 1
fi
