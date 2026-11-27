# H-23：桌面工作区加载的代际污染

关闭日期：2026-11-27。审计基线 `60895e18`，代码基线 `d7ade0e`。

## 问题

`apps/desktop-web/src/shell/store.tsx` 的 `loadWorkspace` 四路 `Promise.all`
返回后直接 `set`，不检查核心代次/当前账号；`refreshSession` 只更新 session，
不清工作区/cache/generation；事件订阅也没有按代次过滤。切号或 401 刷新出
另一个账号时，旧账号的会话、联系人、偏好可以落进新账号界面。

## 修复

- `bootstrap`、`refreshSession`、`loadWorkspace` 在发请求前记录
  `coreSessionSnapshot().generation`；返回后若代次已变，直接丢弃，不 `set`。
- `loadWorkspace` 额外校验 `target` 仍是当前 authenticated 账号。
- `refreshSession` 拿到投影后先分类边界：
  - `signed_out` → `sessionBoundary("logout")`；
  - authenticated → locked → `sessionBoundary("lock")`；
  - 前后账号不同 → `sessionBoundary("account-switch")`；
  然后才写 session，authenticated/locked 再重新 `loadWorkspace`。
- `AppProvider` 的事件订阅捕获订阅时的核心代次；事件到达时代次不一致就丢弃。
  `sessionBoundary` 写入 state 后 `boundaryGeneration` 变化，effect 重订阅，
  新会话事件继续正常接收。
- 没有改 `EventEnvelope` 线格式（核心目前不提供 account/generation）；过滤在
  组合根完成，旧事件不会进入 `applyEvent`。

## 证据

- `npm run typecheck`：通过。
- `npm test`：23 files / 269 tests passed。
- 新增 `src/shell/session-race.test.tsx` 2 条：
  1. `loadWorkspace` 四路请求挂起时调用 `sessionBoundary("account-switch")`，
     再返回旧账号会话数据 —— `conversations=[]`、`workspaceFor=null`；
  2. `refreshSession` 从 user-alpha 刷成 user-beta ——
     `sessionBoundary.reason=account-switch`、旧会话被清空、session 落到 beta。
- 原有 `session-boundary.test.tsx` 7 条继续通过。

## 边界

- `IndexedDB`/本地 outbox/设备密钥 namespace 的账号隔离不在本次改动里；这里
  修的是桌面内存工作区与事件落库代次。若核心缓存仍跨账号复用，需要另开卡。
- 事件过滤依赖 `sessionBoundary` 触发 React 重渲染并重订阅；如果某条路径只
  `resetCoreSession()` 而不写 `sessionBoundary`，订阅不会主动重连。现有
  登录/退出/切号路径都经过 `sessionBoundary`。
- 只在 jsdom 单测验证，未做两台真实设备/两个账号的端到端。
