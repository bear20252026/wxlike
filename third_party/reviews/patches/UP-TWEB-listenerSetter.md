# UP-TWEB · `src/helpers/listenerSetter.ts` → `ui/src/vendor/tweb/listenerSetter.ts`

- 上游：`morethanwords/tweb` @ `ef41b29db1e5356c6ba3b03285013b8ddeea673b`
  永久链接：<https://github.com/morethanwords/tweb/blob/ef41b29db1e5356c6ba3b03285013b8ddeea673b/src/helpers/listenerSetter.ts>
- 许可：GPL-3.0-only
- 本地 sha256：`63f505d1379f669a0ece9ac0876c8aa89d8b9da8774ff64a0820cf0e5f5602b6`

## 拿了什么

事件监听器的集中登记与清理：`add` / `remove` / `removeManual` / `addCleanup` / `removeAll`。
用途是"点外面关闭"——消息菜单与贴纸面板原来各自手写一遍自清监听器，切视图时漏删，
表现为菜单关不掉、监听器泄漏。

## 改了什么（两处）

1. 去掉 tweb 内部类型 `EventListenerBase` 的 import 与 union 成员（我们传原生 `EventTarget`）；
2. `strict` 下补 `let listener: Listener | undefined` 的初始化。

## 为什么不是自研

这套 API 的坑在"`removeAll` 之后 `addCleanup` 的回调顺序"。自写一遍等于重踩。

## 复核方式

同 `UP-TWEB-slicedArray.md`：sha256 与 `registry.json` 必须一致。

## 2026-10-25 换行复核

旧指纹来自 Windows CRLF 工作区。已逐字节验证：仓库 LF 内容仅将换行转换为 CRLF 后即匹配旧指纹；代码内容无变化。现登记 Git 中 LF 字节的 sha256，`.gitattributes` 固定该文件以 LF 检出，保证 Linux / Windows 复核一致。
