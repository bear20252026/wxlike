# H-24：移动端冷启动应用锁异步恢复

修改日期：2026-11-27。审计基线 `60895e18`，代码基线 `5b32bb3`。

审计原文：`AUDIT-60895e18.md:300-305`。`AppLockController.restore()` 在
`initState` 里以 `..restore()` 启动但不 await；同时立即 `_mountConversations()` 和
`_syncPush()`。`_locked` 默认 false，Keystore 慢时首帧先显示缓存消息并在锁状态未知前
联网。

## 修复

`apps/mobile/lib/shell/mobile_app.dart`：

- 增加 `_lockReady`；`initState` 只创建 `AppLockController`、注册 listener 并调用
  `_restoreLock()`，不再立即挂会话或登记推送。
- `_restoreLock()` `await restore()` 后才 `_lockReady=true`；只有 session 存在且未锁定
  才 `_afterUnlock()`（挂 `ConversationsController` + `_syncPush`）。
- 首帧在 `_lockReady=false` 时只显示 `CircularProgressIndicator`；锁定时 `_home()`
  只渲染 `AppLockScreen`，不构建 `AppShell`。
- `AppLockController` listener：
  - 上锁时把 `AppShell` 从树上移除并释放 `ConversationsController`（停轮询/订阅）；
  - 解锁后重新挂控制器并补推送登记；
  - `AppLockController.restore()` 对非 UNAVAILABLE 错误本来就保持 `_locked=true`，
    未知/IO 失败 fail closed；UNAVAILABLE 仍按原策略停用锁并显示警告。

## 验证状态（重要）

本机没有 Flutter/Dart SDK，**未执行 `flutter test` / `dart format`**。本次只完成代码
修改和静态审读；`apps/mobile/test/g12/mobile_app_test.dart` 的 widget 测试尚未补
“慢 Keystore 冷启动首帧不得出现 NavigationDestination、恢复后出现 AppLockScreen”的
用例。H-24 在证据口径上仍是**代码已改、测试待跑**，不能记作完全关闭。

下一步在有 Flutter 的环境补：

1. 自定义 `SecretStore`，`read()` 延迟若干毫秒；
2. `pumpWidget` 后只 `pump()` 一帧，断言只有 loading，无 `NavigationDestination`；
3. 延迟结束后 `pumpAndSettle`，断言出现 `AppLockScreen`；
4. 输入正确 PIN 解锁，断言 `AppShell` 才出现。
