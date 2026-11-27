# ui/ —— Tauri 宿主（旧 UI 已退役）

> **状态：旧 UI 与 raw IPC 已退役（2026-11-27，owner 决策）。**
> 默认宿主只加载 `apps/desktop-web`（React 19 新架构）。
> `ui/src/`、`ui/dist`、旧 UI 构建脚本与 npm 元数据已删除，不再保留历史归档；
> 浏览器实时入口由 `apps/desktop-web` + `crates/client-web` 的 WASM 协议承担。
> 任何重新引入旧 UI 桌面入口的提交都必须先经过 owner 评审。

## 新旧边界（事实）

| 项 | 新 UI（唯一发布路径） | 旧 UI（已退役） |
|---|---|---|
| 源码 | `apps/desktop-web/src` | 已删除（C-02，2026-11-27） |
| 产物 | `apps/desktop-web/dist` | 已删除 |
| 宿主配置 | `ui/src-tauri/tauri.conf.json` | `tauri.conf.old-ui.json` 已删除 |
| 与 Rust 接口 | `core_dispatch` / `core_subscribe` / `core_capabilities` | 原 70 个 Tauri 命令中的 67 个已从 `generate_handler!` 移除（其中 62 个旧 UI 独占） |
| 守卫 | `scripts/check-tauri-host.mjs` 要求默认指向新 UI，且要求旧配置不存在 | 同左 |

## 命令面（唯一允许）

`ui/src-tauri/src/lib.rs` 的 `generate_handler!` 只保留：

- `core_dispatch`
- `core_subscribe`
- `core_capabilities`

其余旧命令函数可以暂时留在源码里做迁移参考，但**不再注册为 Tauri command**，前端无法通过 invoke 调用。新增命令必须走 `core_dispatch` 的 action 面，不能直接加 raw IPC。

## 发布门禁

- `tauri.conf.json` 的 `frontendDist` 必须指向 `apps/desktop-web/dist`；
- `tauri.conf.old-ui.json` 必须不存在；
- `scripts/build-old-ui.sh` 必须不存在；
- 旧 UI 源码不得被 release workflow、CI 打包或复制进产物；
- `core_bridge` 仍是新 UI 的唯一宿主泵，不得回退到旧 `commands.rs` 的逐功能命令。

## 仍待完成（不冒充已关闭）

旧 UI 独占功能的桌面端覆盖仍需逐项补齐：通话 UI、秘密聊天、本地历史、钱包/红包、
视频、收藏、会话管理。`OLD-UI-RETIREMENT` 台账在功能覆盖完成前保持 open；
本文件只证明**发布入口和 raw IPC 面已退役**，不证明功能等价。
