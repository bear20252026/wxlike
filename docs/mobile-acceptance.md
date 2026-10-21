# Zone B 真机验收清单

> 2026-10-20 · 适用于 `feat/zone-b`（B1–B5）· 目标机型：已 root 的小米 MI 8
> （Android 10 / SDK 29 / arm64-v8a）。其他 arm64 机器同样适用，只是第 1 步的
> `arm64-v8a` 与安装命令要跟着改。
>
> **打勾规则**：每条都要写"实际看到什么"。空着 = 没过。本项目明令禁止把没跑过
> 的东西记成绿的（`docs/task-assignment.md` §3.7 的"静默降级"同样适用于验收表）。

---

## 0. 本机已经验过什么（不用你在手机上重复）

| 卡 | 验收命令 | 结果 |
|---|---|---|
| B1 | `cargo test -p wxlike-ffi` | 29 个测试全绿（13 单元 + 16 集成），假 HTTP 服务端在回环上跑 |
| B1 | `bash crates/ffi/regenerate.sh` | 生成 `wxlike_ffi.kt`（103 KB）与 `wxlike_ffi.dart`（184 KB） |
| B1/B3 | `flutter test` | 8 个测试全绿，其中 2 个是**真过 FFI** 的跨界冒烟（加载宿主 `wxlike_ffi.dll`，走完登录→列表→历史→发送→断网读缓存→退避后按同一幂等键补发→服务端拒绝→推送注册→登出清库） |
| B2 | `dart analyze lib test tool` | 无告警（生成物已在 `analysis_options.yaml` 排除） |
| B1b | `bash apps/mobile/tool/build-rust.sh` | 产出 `arm64-v8a` 与 `x86_64` 的 `libwxlike_ffi.so`（`-C strip=symbols -C debuginfo=0`，未剥前 58 MB） |
| B2 | `bash apps/mobile/build_android.sh`（**2026-10-21 复验**） | **通过**：`app-release.apk` **21.1 MB**（arm64，`libwxlike_ffi.so` 4.37 MB 已 strip、`libapp.so` 4.69 MB）。这一条是"新检出能不能出包"的答案——见下面的说明。 |
| B2 | `flutter build apk --debug` | **通过**：`build/app/outputs/flutter-apk/app-debug.apk`，162 MB（10-21 复测；10-20 记录是 168 MB）。大是因为 debug 引擎的 `libflutter.so` 本身就上百 MB 且带两个 ABI；我们的部分只有 `libwxlike_ffi.so` 7.1 MB (arm64) / 8.0 MB (x86_64)（脚本里加了 `-C strip=symbols -C debuginfo=0`，没加之前是 58 MB）。release 装出来的体积要在真机验收时另记一笔。 |
| B1 | `cargo clippy -p wxlike-ffi --all-targets -- -D warnings` | 通过。顺带把六关在本机跑了一遍（`feat/zone-b` 上）：`cargo build --workspace`、`cargo test --workspace`、`cargo fmt --all -- --check`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` **全绿**。MSVC `link.exe` 那句"正在创建库 …"会在 build/test 的链接步骤以 `linker_messages` 告警形式出现（rustc 1.97 的新 lint），但它不影响 `-D warnings` 的 clippy 关——clippy 不链接。Zone B 自己的 crate 与测试文件都单独 allow 掉并写了原因。`cd ui && npx tsc --noEmit` 没跑：本区零 TS 改动，且 worktree 里没有 `ui/node_modules`。 |
| B1 | Kotlin **编译** | ⚠️ 只做到"引用能解析"：本机没有 kotlinc，从 Gradle 缓存手拼编译器。JNA 5.6.0 下 `com.sun.jna.internal.Cleaner.register` 找不到；换 JNA 5.14.0 后该引用解析通过，编译器随后在无关的内部代码生成上崩了。**需要在有 Android Studio / 正常 kotlinc 的机器上补一次**（§9 给了确切命令）。不影响 App——移动端走 Dart 绑定，Kotlin 那一路是卡面要求的能力证明。 |
| B4 | — | **没验**：需要你的 Google Cloud 项目与 `google-services.json`，见 §6。已验的是 Rust 侧那段：`POST /api/devices` 的 body 与鉴权头（跨界冒烟里就在打这条） |

### 为什么新检出直接 `flutter build apk` 会失败

不是工程坏了，是**顺序**。仓库里不提交两样生成物：

| 不提交 | 症状 | 谁生成 |
|---|---|---|
| `lib/src/rust/wxlike_ffi.dart` | `flutter analyze` 报 109 个 error，全是"`OutboxSummary` 不是类型"这类——因为整个绑定的类型都来自这个文件 | `crates/ffi/regenerate.sh` |
| `jniLibs/*/libwxlike_ffi.so` | 编译能过，运行到 `DynamicLibrary.open` 时失败（Dart 侧显示"打开失败"） | `cargo ndk` |

一条命令按顺序补出来：`apps/mobile/build_android.sh`（2026-10-21 加，实测出 21.1 MB 的 release 包）。

**`gradlew` / `gradle-wrapper.jar` 不在其中**：Flutter 构建前会自己注入
（`injectGradleWrapperIfNeeded`）。实测把这三个文件删掉后 `flutter build apk`
仍然成功并重新生成——所以"缺 gradle wrapper"不是原因，别去补它。

> 跨界冒烟为什么把假服务端放在**子进程**里：绑定层在 Dart 侧是同步调用，主 isolate
> 卡在 FFI 里时，同进程的 `HttpServer` 根本没机会 accept，症状是每个请求都"超时"，
> 看着像网络坏了其实是自己锁住自己（`tool/fake_wx_server.dart`）。
> 这也是 §7 里"UI 卡顿"那条待办的实证。

---

## 0.5 2026-10-21 真机实测（MI 8 / Android 10 / arm64）

第一次真的把 release 包装到手机、连真服务端跑。**测出 1 个 blocker + 3 个缺陷**，
全部是单测和 CI 抓不到的（它们跑在宿主上，只验证"能编"，不验证"能联网"）。

### 环境与操作要点（下次直接照做）

```sh
# 1) 服务端（主机，HTTP 只监听回环，靠 adb reverse 转发）
WXLIKE_DEV_INSECURE=1 WXLIKE_DB=... WXLIKE_HTTP=127.0.0.1:8099 ./target/debug/wxlike-server
adb reverse tcp:8099 tcp:8099          # 手机上的 127.0.0.1:8099 → 主机

# 2) 装包：MIUI 会拦 adb install
#    INSTALL_FAILED_USER_RESTRICTED: Install canceled by user
adb shell settings put secure install_non_market_apps 1   # ← 关键，不是 adb_install_need_confirm
adb install -r app-release.apk

# 3) 锁竖屏，否则截图/坐标全乱
adb shell settings put system accelerometer_rotation 0
adb shell settings put system user_rotation 0
```

两个操作坑：

- **`adb shell input text` 会被中文输入法吞掉**：讯飞把 `Alice` 变成候选
  `Alice / 阿里 / 阿狸`，按空格选中候选后落到输入框里的是 `A里`。
  用**纯数字**账号（`20261021`）绕过，数字不触发拼音组合。
- **坐标要用 `uiautomator dump` 取**，不要从截图估。截图被缩放过（1080×2248 的屏
  出的是 540×1080 的图），按图估坐标会点到框外。例如输入框真实 bounds 是
  `[33,1931][815,2029]`，中心 `(424,1980)`。

### 通过项

| 项 | 结果 |
|---|---|
| 装包并启动 | ✅ 登录屏正常，`.so` 加载成功（没有"打开失败"） |
| 注册 | ✅ `POST /api/register` 成功，进会话列表 |
| 在线状态 | ✅ 标题栏绿点"在线"，显示用户 id 前 16 位 |
| 会话列表 | ✅ 建群后点刷新能看到（"真机测试群 · 2 人"） |
| 发消息 | ✅ 气泡显示、服务端落库（seq 1） |
| 对方可见 | ✅ 另一账号用 token 拉到同一条（内容/发送者/seq 都对） |
| 离线入队 | ✅ 断开 `adb reverse` 后发消息 → "待发送 · 第 2 次" + 重试/删除按钮，不是静默失败 |
| 手动重试 | ✅ 恢复通道后点"重试"→ 服务端 seq 3 收到 |

### 不通过项（真机才暴露）

1. **release 包缺 INTERNET 权限（blocker，已修）**。
   症状：`Connect error: Permission denied (os error 13)`——看起来像服务端没起，
   其实是 EACCES。Flutter 模板只把 `<uses-permission INTERNET>` 放在
   `src/debug/AndroidManifest.xml`，main 里没有，所以 **debug 能联网、release 不能**。
   已补进 `src/main/AndroidManifest.xml`（连同 `usesCleartextTraffic`）。
2. **聊天屏没有实时接收**。对方发的消息不会自己出现；退出重进才看到。
   `chat.dart` 里没有 Timer/轮询，也没有订阅 QUIC/WS 推送。
   → 清单 §2 的"对方 3 秒内看到"**不通过**。这不是 bug 而是没实现，
   但文档此前没写，容易被当成偶发。
3. **网络恢复后不自动补发**。断开通道 → 消息入队 → 恢复通道 → 等了 10 秒仍是
   "待发送 · 第 2 次"，必须手动点重试。Mars 那套"网络变化触发队列泵"在
   FFI/移动端这一层没接上（`core::outbox` 本身有 `flush`，缺的是触发源）。
4. **重试成功后消息不进列表**。服务端已收到（seq 3），UI 里那条"待发送"气泡
   消失了，但正常气泡没出现——要退出重进才显示。发送成功的返回值没 append 到
   `_messages`。

> 这三条（2/3/4）都没修，因为它们是**功能缺口**不是编译错误，要单独排期。
> 记在这里是为了让下一次验收不用重新发现一遍。

## 1. 起环境（每次验收前）

```sh
# 服务端（主 checkout 里跑，端口 8099）
WXLIKE_DEV_INSECURE=1 WXLIKE_DB=/tmp/wx/wxlike.db WXLIKE_HTTP=0.0.0.0:8099 \
  WXLIKE_QUIC=0.0.0.0:9443 WXLIKE_MEDIA_DIR=/tmp/wx/media cargo run -p wxlike-server

# 手机能连到的地址（不是 127.0.0.1！）
ipconfig | grep -A1 "无线局域网适配器"

# Rust → 绑定 + .so → APK（一条命令；新检出必须先跑）
cd ../..                     # 仓库根
apps/mobile/build_android.sh --debug

# 装
adb install -r build/app/outputs/flutter-apk/app-debug.apk
adb shell monkey -p com.wxlike.wxlike_mobile 1
```

- [ ] 服务器地址那一栏填的是**开发机的局域网 IP**，登录成功
- [ ] 会话列表出现（至少一个直聊 + 一个群）
- [ ] 点进去能发消息，对方 3 秒内看到

---

## 2. 双机收发

第二台设备（另一台手机 / 桌面端 `ui`）与手机互发。

| # | 步骤 | 期望 | 实际 |
|---|---|---|---|
| 2.1 | A 给 B 发一条文本 | B 的会话列表未读 +1，标题正确 | |
| 2.2 | B 进会话 | 消息按 seq 升序，B 发的气泡在右、A 的在左 | |
| 2.3 | B 停留 2 秒退出 | A 侧看到已读；B 列表未读归 0 | |
| 2.4 | A 连发 10 条 | 顺序不乱、不重、不丢 | |
| 2.5 | 同一句话 A 快速点两次发送 | 服务端只有一条（幂等键在 `task_id` 上）| |

> 注：移动端**没有**长连接，实时性 = 推送（第 6 节）或回到列表页主动拉。
> 2.1 里"B 3 秒内看到"在未接 FCM 时不成立——那时 B 要下拉刷新才更新。
> 这条先按下拉刷新验，接了 FCM 再验自动到达。

---

## 3. 切网（WiFi ↔ 4G）

| # | 步骤 | 期望 | 实际 |
|---|---|---|---|
| 3.1 | 停在会话列表，关 WiFi 切 4G | 下拉刷新后列表还在（顶栏先"离线"再"在线"） | |
| 3.2 | 聊天页正在输入时切网 | 输入内容不丢；下一次发送成功 | |
| 3.3 | 切网瞬间点发送 | 气泡标"待发送"，不报"发送成功" | |
| 3.4 | 恢复后 ≤ 35 秒 | 队列自己发出（补发线程空闲节拍 30 秒）；或进会话页立刻发出 | |

---

## 4. 后台与保活

MI 8 的 MIUI 省电策略会把后台应用冻住，**这一步必须先手动放行**：
设置 → 应用管理 → wxlike_mobile → 省电策略 → 无限制。

| # | 步骤 | 期望 | 实际 |
|---|---|---|---|
| 4.1 | 应用退到后台 30 分钟，期间对方发 3 条 | 回前台后 3 条都在，顺序对 | |
| 4.2 | 后台 30 分钟内电量消耗 | 记录百分比；待机不应持续发热（补发线程空闲时 30 秒一跳） | |
| 4.3 | 从最近任务划掉应用再打开 | 仍在登录态（token 在本地库），不需要重新输密码 | |
| 4.4 | 划掉应用**之前**发一条没发出去的消息，重开 | 该消息仍在队列并自动补发（outbox 落盘） | |

---

## 5. 弱网与离线

限速用 Linux 侧 `tc`，或直接拿 4G 信号格当弱网。

| # | 步骤 | 期望 | 实际 |
|---|---|---|---|
| 5.1 | 限速 200 kbps 下发送 | 最终成功；期间显示"待发送" | |
| 5.2 | 开飞行模式进会话 | 历史照样能翻（`from_cache`，页面顶部有"离线中 · 显示的是本地缓存"） | |
| 5.3 | 飞行模式下发一条 | 立刻显示"待发送"，不弹"成功" | |
| 5.4 | 关飞行模式 | 自动补发，且服务端只有一条（幂等） | |
| 5.5 | 反复 5.3/5.4 五次以上 | 第 5 次失败后进"发送失败"，给出原因 + 重试/删除按钮 | |
| 5.6 | **已知问题**：请求在 UI isolate 上是同步的，`crates/client` 单次超时 10 秒 | 弱网时界面会卡住最多 10 秒。记下实际卡顿秒数；改进方向见 §7 | |

---

## 6. 推送（B4，需要你先建 FCM）

当前状态：`crates/ffi` 的 `register_push_token` 已通（本机会验：打到
`POST /api/devices`，body 是 `{"platform":"android","device_token":…}`），
但 App 里**没有** `firebase_messaging` 依赖——加了它，构建期就必须要
`google-services.json`，B2 的 `flutter build apk` 会被 B4 的凭据卡住。

要接通，按这个顺序：

1. Google Cloud 项目 → Firebase → 添加 Android 应用，包名 `com.wxlike.wxlike_mobile`；
2. 下载 `google-services.json` → 放 `apps/mobile/android/app/`；
3. `apps/mobile/pubspec.yaml` 加 `firebase_messaging: ^14.9.0`；
   `android/app/build.gradle.kts` 顶部插件块加 `id("com.google.gms.google-services")`；
4. 把 `lib/src/push.dart` 的 `ManualTokenSource` 换成
   `FirebaseMessaging.instance.getToken()` 的实现（接口已经留好，只改这一个文件）；
5. 服务端：`crates/push` 有 FCM HTTP v1，缺**凭据配置**与 token 失效清理。

| # | 步骤 | 期望 | 实际 |
|---|---|---|---|
| 6.1 | 装好后 `adb logcat \| grep -i firebase` 抓 token | 服务端 `SELECT * FROM devices` 有这行 | |
| 6.2 | 杀掉 App，对方发一条 | 10 秒内出系统通知 | |
| 6.3 | 点通知 | 进入**对应会话**（不是停在列表） | |
| 6.4 | 应用在前台收消息 | 不重复弹通知 | |
| 6.5 | 换账号登录同一台机器 | 服务端 token 被替换（`(user, platform)` 唯一） | |
| 6.6 | 服务端把该 token 判为无效（换一个假 token 再发） | 设备行被清理，不无限重试 | |

---

## 7. 已知没做的（别在验收时以为是 bug）

| 项 | 现状 |
|---|---|
| E2EE 密聊 | 移动端没有。桌面端的 `send_secret_text` 路径未进绑定；`crates/core::e2ee` 在那边，不在这里 |
| 图片/语音/视频消息 | 只有 `media_id` 字段透传，没有渲染器，也没有上传 |
| 撤回 / 群管理 | `Policy` 明确关掉破坏性命令（`crates/ffi/src/client.rs` 构造处），移动端不出现这两个入口 |
| 实时到达 | 无 QUIC/WS 长连接。原因写在 `crates/ffi` 模块头：`crates/net` 里 hub 与 axum 同 crate，链它等于把 web server 装进手机 |
| 昵称/头像 | 直聊标题是对方 `user_id` 原文（服务端 `conv.list` 给的就是这个），没做 profile 解析 |
| UI 卡顿 | FFI 调用同步跑在 UI isolate 上。接口已经是 `Future`，换成 isolate actor 只改 `lib/src/service.dart` 一个文件 |
| 凭据与正文明文落库 | 与应用私有目录同口径（桌面端 `LocalStore` 也一样）。要加固就换 Android Keystore + 字段加密 |
| iOS | 完全没做。UniFFI 的 Swift 后端是官方支持的，缺 Xcode 工程与 Podfile |

---

## 8. 绑定层的两个坑（改 Rust 接口前先读）

这两个都不是"看代码能看出来"的，踩中时的症状都是 **Rust 侧测试全绿、App 一调就崩**：

1. **`uniffi` 必须开 `scaffolding-ffi-buffer-fns`**。`uniffi-bindgen-dart` 生成的
   绑定走 ffibuffer 调用约定（一次调用的参数打包成一个 buffer 递进去），而
   uniffi 0.32 默认**不导出** `uniffi_ffibuffer_*` 符号。没开的时候症状是
   `Failed to lookup symbol 'uniffi_ffibuffer_wxlike_ffi_fn_constructor_wxlikeclient_new'`。
2. **`crates/ffi/uniffi.toml` 的 `library_name` 必须显式等于 crate 名，并且要
   `--config` 传进去**。同一个值既当 `DynamicLibrary.open` 的默认库名，又当
   辅助符号前缀 `ffi_<name>_rustbuffer_from_bytes`；默认值是 `uniffi_<ns>`
   （Kotlin `loadLibrary` 的约定），于是每个调用都在 lookup 上失败。
   library mode 下生成器**不会**自己去 crate 根找这个文件。

复现与验证都由 `apps/mobile/test/ffi_smoke_test.dart` 兜着——它是唯一真的过一遍
C ABI 的测试，别把它 skip 掉了就当没事。

---

## 9. 交给集成方的三条命令

```sh
cargo test -p wxlike-ffi                 # B1
bash crates/ffi/regenerate.sh            # 重生成 Kotlin + Dart 绑定
cd apps/mobile && bash tool/build-rust.sh && flutter build apk --debug   # B2
```

Kotlin 编译补验（在有 Android Studio 的机器上）：

```sh
# 需要 JNA >= 5.14：5.6.0 没有 com.sun.jna.internal.Cleaner.register
kotlinc -cp jna-5.14.0.jar -d /tmp/kt \
  crates/ffi/generated/kotlin/uniffi/wxlike_ffi/wxlike_ffi.kt
```

---

## 10. 结论

| 卡 | 判定 | 签字 |
|---|---|---|
| B1 UniFFI 绑定层 | | |
| B2 Flutter 三屏 | | |
| B3 离线与队列 | | |
| B4 推送 | | |
| B5 本清单 | | |

录屏：`adb shell screenrecord --time-limit 180 /sdcard/wxlike-b5.mp4` 拉下来存
`docs/`（或贴在 PR 里）。
