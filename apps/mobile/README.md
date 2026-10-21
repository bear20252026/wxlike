# wxlike 移动端（Zone B：Flutter + Rust UniFFI）

三屏：登录 → 会话列表 → 聊天。逻辑全在 Rust，Dart 只画界面。

## 出 APK（一条命令）

```sh
apps/mobile/build_android.sh            # release，arm64，约 21 MB
apps/mobile/build_android.sh --debug    # debug，含 x86_64（模拟器）
```

**新检出必须先跑这个。** 仓库里不提交两样生成物（和 `ui/dist/app.js` 同口径）：

| 不提交的东西 | 为什么 | 谁生成 |
|---|---|---|
| `lib/src/rust/wxlike_ffi.dart` | UniFFI 从编译产物里读接口形状，几百 KB 生成代码 | `crates/ffi/regenerate.sh` |
| `android/app/src/main/jniLibs/*/libwxlike_ffi.so` | 交叉编译产物，release 5.8 MB / debug 56 MB | `cargo ndk` |

所以直接 `flutter build apk` 会在 `lib/src/service.dart:17` 的
`import 'rust/wxlike_ffi.dart';` 处失败——不是工程坏了，是**顺序**：
先有 `.so` 才有绑定，先有绑定 Dart 才能编译。脚本按这个顺序串好了。

> `android/gradlew` 与 `gradle-wrapper.jar` 也**不需要**提交：
> Flutter 在构建前会自己注入（`injectGradleWrapperIfNeeded`）。实测删掉后
> `flutter build apk` 仍然成功，并且把它们重新生成出来。

## 目录

```
lib/main.dart              应用根：打开客户端、决定进登录还是列表
lib/screens/               三屏
lib/src/service.dart       界面能问的全部问题（唯一与绑定层打交道的文件）
lib/src/rust/              UniFFI 生成的 Dart 绑定（不进 git，跑 regenerate.sh 重建）
lib/src/push.dart          B4 的推送骨架（FCM 凭据待接）
android/app/src/main/jniLibs/<abi>/libwxlike_ffi.so   平台产物，不进 git
build_android.sh           一条命令：绑定 → .so → APK（新检出先跑这个）
tool/build-rust.sh         只出 .so（调试交叉编译时单独用）
```

## 改完 Rust 之后

```sh
# 仓库根：一条命令重做绑定 + .so + APK
apps/mobile/build_android.sh --debug

# 只想跑在已连的设备上（不打包）
bash crates/ffi/regenerate.sh && bash apps/mobile/tool/build-rust.sh
cd apps/mobile && flutter run
```

`lib/src/rust/wxlike_ffi.dart` 是生成物，分析器已经把它排除
（`analysis_options.yaml`），所以它不符合本仓库的 lint 风格是正常的。

## 为什么绑定是 Dart 而不是 Kotlin

ADR-11 定的是 UniFFI，但 Mozilla 的官方后端只有 Kotlin / Swift / Python / Ruby。
本项目用 `uniffi-bindgen-dart`（MIT，依赖 `uniffi_bindgen 0.32.2`）从同一份
UniFFI 元数据出 Dart 绑定，只在生成期使用（`cargo build -p wxlike-ffi` 默认不带
`dart` feature）。Kotlin 那一路照样生成，作为 B1 卡面要求的能力证明，见
`crates/ffi/generated/`（不提交）。登记见 `THIRD-PARTY-NOTICES.md`。

## 验收

跑什么、怎么算过、哪些必须上真机：见仓库根 `docs/mobile-acceptance.md`。
