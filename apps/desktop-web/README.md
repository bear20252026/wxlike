# apps/desktop-web

UI v2 React 客户端的功能目录（FP 交付计划）。当前状态：

- **脚手架最小化**：产品壳（侧栏/聊天/登录/总路由）归 G10-01、G00-02。本目录当前只有
  `src/main.tsx` 一个开发壳（仅挂载 G11 页面）+ 共享 lib（`src/lib/`），两者都会在
  G00/G10 合入后收编或替换；登记见 `integration-requests/G11/G11-01.yaml`。
- **功能模块**（G11 独占路径）：

  | 目录 | 任务 | 状态 |
  |---|---|---|
  | `src/features/content` | G11-01 朋友圈/收藏/文章/表情 | moments/favorites/report 已接真实端点；articles/publishers/stickers 契约界面（G05 未开通） |
  | `src/features/files` | G11-02 文件中心 | 计划中 |
  | `src/features/video` `src/features/calls` | G11-03 视频流/通话/直播 | 计划中 |
  | `src/features/commerce` | G11-04 钱包/收付款/红包/卡包 | 计划中 |
  | `src/features/mini` `src/features/discover` | G11-05 小程序/扫码/服务目录 | 计划中 |

- **测试栈**：Vitest + jsdom + Testing Library。fixture 传输（`src/test/fixture.ts`）
  用于上游卡未合入时的并行开发；同一批测试在 G04/G05/G06/G07/G08 合入后通过
  `setTransport(fetchTransport)` 指向真实服务端跑集成验收。

## 命令

```bash
npm install        # 首次
npm run typecheck  # tsc --noEmit
npm test           # vitest run
npm run build      # vite 生产构建
npm run dev        # 本地开发（需服务端，见 server/）
```

## 与服务端的关系

客户端不实现业务规则：moments/favorites/report/wallet/transfer/redpacket/call/live/media/video
等直接走 `crates/platform`、`crates/video` 的真实 HTTP 端点；错误统一 `{"error": "…"}`；
写操作携带 `Idempotency-Key`。服务器再鉴权是唯一授权依据，UI 的显隐只是体验优化。
