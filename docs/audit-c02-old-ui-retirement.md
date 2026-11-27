# C-02 关闭记录：旧 UI / raw IPC 的 E2EE 降级路径已移除

- 审计基线：`60895e18`
- owner 决策：2026-11-27，旧 UI 与 raw IPC 全部退役，只保留 `apps/desktop-web` + `core_dispatch`
- 结论：**C-02 关闭**；`OLD-UI-RETIREMENT` 仍因功能等价未完成而保持 open，二者不是同一件事。

## 删除清单

- `ui/src/` 全部旧前端源码（25 个文件，含 `ui/src/web/wire.ts`、`hub.ts`、`session.ts` 和 tweb vendor 代码）。
- `ui/scripts/`、`ui/package.json`、`ui/package-lock.json`、`ui/eslint.config.mjs`、`ui/tsconfig.json`。
- `scripts/build-web.sh`（旧 `ui/dist-web` 的唯一构建入口）。
- `ui/dist` / `ui/dist-web` 不再存在，也不再被 `.gitignore`、`.dockerignore`、Tauri 配置或发布 workflow 引用。
- 旧 tweb `SlicedArray` 代码及其 `THIRD-PARTY-NOTICES.md` 条目随源码删除。

保留 `ui/src-tauri/` 仅作为 **Tauri 宿主**：`tauri.conf.json` 的 `frontendDist` 指向 `apps/desktop-web/dist`，不是旧 UI。

## 为什么这关闭了 C-02

旧 UI 的 E2EE 降级只在两条入口上成立：

1. 旧 UI 前端源码被构建/发布，并通过旧路径转发消息；
2. raw IPC 逐功能命令可被旧页面直接 invoke。

两条入口现在都不存在：源码和构建脚本删除，`generate_handler!` 只剩 `core_dispatch` / `core_subscribe` / `core_capabilities`。服务端 `/app` 静态托管文档也改为指向 `apps/desktop-web/dist`，不会再把旧 bundle 发出去。

## 验证

```bash
node scripts/check-tauri-host.mjs
python3 -m unittest ci.jobs.tests.test_ci.OldUiRetirementTest
python3 ci/jobs/contract/check_wire_variants.py
```

`OldUiRetirementTest` 新增 `test_old_ui_archive_is_gone`：`ui/src`、`ui/dist`、`ui/dist-web`、`scripts/build-web.sh` 任一复活即红。`check_wire_variants.py` 已从“Rust ↔ 旧 TS”改为“真实 hub ↔ `crates/client-web/src/wire.rs`”，不再依赖已删除的 `ui/src/web/wire.ts`。

## 边界

`OLD-UI-RETIREMENT` 的功能等价仍 open：通话、秘密聊天、本地历史、钱包/红包、视频、收藏、会话管理等旧独占能力还需要 desktop-web 补齐。C-02 只证明“旧入口和 E2EE 降级”已移除，不证明功能等价。
