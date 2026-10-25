# wxlike desktop-web（G10：React 桌面与网页核心界面）

wxlike 的桌面端与网页端共用这一套 React 应用。它只通过两条边界与 Rust 核心对话：

- `CoreHandle.dispatch(request) / subscribe(cursor, handler)` — 信封契约 v1（`ActionRequest` / `ActionResponse` / `EventEnvelope`），见 `src/shell/core-contract.ts`；
- `PlatformPort` — 窗口、托盘、自启、全局快捷键、通知、剪贴板、截图、文件、本地秘密（`src/shell/platform/port.ts`）。

界面不持有任何永久真相：会话、消息、关系、偏好全部来自核心投影；所有写操作带幂等键；错误、空态、加载、权限拒绝都有可操作的界面；平台不支持时如实说明原因，不用 toast 或静态数据冒充完成。

## 运行

```bash
npm install        # 在 apps/desktop-web 内
npm run dev        # http://localhost:5183
npm run typecheck  # tsc --noEmit
npm test           # vitest run（34 个用例）
npm run build      # typecheck + vite build
```

## 数据源选择（App 的 selection）

- `tauri`：Tauri 桌面壳注入 `window.__WXLIKE_TAURI__` 时走真实 Rust 核心 + `platform_*` 命令（`src/shell/platform/tauri-platform.ts`）。
- `unavailable`：生产网页构建没有 Tauri 桥时，显示"核心不可用"的说明页，不连任何后端、不伪造数据。
- `fixture`：仅 DEV/测试（`import.meta.env.DEV`）的内存核心（`src/shell/adapters/fixture/`），顶栏常驻"非验收证据"横幅；它不是 mock 测试替身以外的任何东西，不作为集成验收依据。

## 职责组 G10 卡片与实现位置

| 卡片 | Issue | 实现 | 测试 |
| --- | --- | --- | --- |
| G10-01 产品壳与登录 | #152 | `src/shell/`（router/store/app-shell）、`src/features/auth/` | `src/features/auth/auth.test.tsx` |
| G10-02 聊天闭环 | #154 | `src/features/chat/` | `src/features/chat/chat.test.tsx`、`upload.test.ts` |
| G10-03 联系人与群 | #157 | `src/features/contacts/` | `src/features/contacts/contacts.test.tsx` |
| G10-04 平台适配 | #158 | `src/shell/platform/`、设置-平台页 | `src/features/settings/settings.test.tsx`（平台证据走真机待验） |
| G10-05 搜索与设置 | #161 | `src/features/search/`、`src/features/settings/` | `src/features/search/search.test.tsx`、`settings.test.tsx` |
| G10-06 扫码登录 | #160 | `src/features/auth/qr-login-page.tsx` | `src/features/auth/qr.test.tsx` |

跨组根清单请求（新根依赖、workspace 登记、`platform_*` 命令挂载）在 `integration-requests/G10/G10-01.yaml` … `G10-06.yaml`，由 G00 串行执行；本组不改根 manifest、总 router、迁移编号与生成绑定。

## 已验证（本机，2026-10-24）

- `npm run typecheck`：0 错误。
- `npm test`：7 个文件 34 个用例全绿（登录/锁定/恢复/守卫、扫码六态、发送状态机 LOCAL_QUEUED→DEVICE_STORED、断网 FAILED_RETRYABLE→重试、草稿恢复、1GB 拦截、分块上传幂等/取消、联系人接受/备注/踢人权限、设置持久化/口令迁移/备份校验导入、搜索分类/事件重查/⌘K overlay）。
- `npm run build`：vite 产物 398.72 kB JS（gzip 121.44 kB）。
- 夹具演示（DEV）：`npm run dev` 打开 `#/login`，账号 `user-alpha` 口令 `demo1234`（或扫码页 DEV 模拟按钮体验六态）。

## 待验清单（上游合入或真机才能验，不得标 ACCEPTED）

以下依赖上游卡片（G00-02 #100、G00-03 #101、G01-02 #105、G01-03 #106、G02-05 #114、G02-07 #115、G03-05 #124、G04-01 #117、G04-05 #125、G16-04 #192）尚未合入，本分支按冻结契约先行开发：

1. Tauri 真桌面：窗口控制、托盘角标、开机自启、全局快捷键（`CommandOrControl+Alt+9` 探针）、系统通知、OS 密钥库 secrets —— 需 G00/G02 挂载 `platform_*` 命令后在 Win/Mac/Linux 实测。
2. 真实手机批准扫码登录（REJECTED/EXPIRED 不能进入账号）—— 需 G02-07 会话签发与 G04 真机。
3. 双账号跨端文本/附件互收、断网重启状态一致 —— 需 G01/G02/G03 真核心与第二设备。
4. 1GB 合成文件在云端的流式上传/断点续传/取消/hash 证据 —— 需 G04-05/G16-04。
5. 真核心下的分页/游标行为与夹具差异（夹具仅覆盖契约形状）。

## 已知边界

- 生产网页（无 Tauri）不连后端：这是 ADR-02/04 的要求（网页没有可用的核心传输时明确显示不可用），网页版可运行指的是 UI 与平台降级路径可运行。
- 内容/朋友圈搜索分类按契约返回"未接入"（G05-05），界面如实展示原因。
