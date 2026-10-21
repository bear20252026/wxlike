# wxlike 移动端（Zone B：Flutter + Rust UniFFI）

三屏：登录 → 会话列表 → 聊天。逻辑全在 Rust，Dart 只画界面。

## 目录

```
lib/main.dart              应用根：打开客户端、决定进登录还是列表
lib/screens/               三屏
lib/src/service.dart       界面能问的全部问题（唯一与绑定层打交道的文件）
lib/src/rust/              UniFFI 生成的 Dart 绑定（不进 git，跑 regenerate.sh 重建）
lib/src/push.dart          B4 的推送骨架（FCM 凭据待接）
android/app/src/main/jniLibs/<abi>/libwxlike_ffi.so   平台产物，不进 git
tool/build-rust.sh         cargo-ndk 出 .so
```

## 改完 Rust 之后

```sh
# 仓库根：重新生成 Kotlin + Dart 绑定
bash crates/ffi/regenerate.sh

# 本目录：重新交叉编译 .so，再装
bash tool/build-rust.sh
flutter run            # 或 flutter build apk --debug
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
