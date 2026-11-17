# ui/ —— 旧桌面 UI（逃生阀，待退役）

> **状态：退役过渡（登记 `ci/jobs/policy/known-gaps.json` 的 `OLD-UI-RETIREMENT`，2026-11-17）。**
> 默认桌面宿主装载的是 **apps/desktop-web**（React 19 新 UI），不是本目录。
> 本目录保留为**逃生阀**：新 UI 出严重问题时可用旧配置打包回退。
> 退役条件与命令清单见下文；退役前这里的功能改动只修阻塞性 bug，不加新功能。

## 新旧边界（事实，勿凭印象）

| 项 | 新 UI（默认） | 旧 UI（本目录，逃生阀） |
|---|---|---|
| 源码 | `apps/desktop-web/src` | `ui/src`（纯 TS + DOM，jotai，esbuild） |
| 产物 | `apps/desktop-web/dist` | `ui/dist` |
| 宿主配置 | `ui/src-tauri/tauri.conf.json`（frontendDist 指 desktop-web） | `ui/src-tauri/tauri.conf.old-ui.json`（frontendDist 指 `../dist`） |
| 构建 | `scripts/build-web-v2.sh` | `scripts/build-old-ui.sh`（`npm run build:old-ui-host`） |
| 与 Rust 的接口 | `core_dispatch` / `core_subscribe` / `core_capabilities`（规范动作面，经 `core_bridge` 分域碎片） | 62 个遗留专用命令（见下） |
| 守卫 | `scripts/check-tauri-host.mjs` + `host-config.test.ts` 钉死默认指向，且**要求逃生阀存在** | 同左（逃生阀检查） |

**命令面盘点（2026-11-17，`lib.rs generate_handler!` 共 70 个）：**

- 新 UI 直接 invoke 的 8 个：`core_dispatch`、`core_subscribe`、`core_capabilities`、
  `register`、`login`、`logout`、`me`、`friends`。
- 其余 **62 个为旧 UI 专用**（新 UI 的对应功能走 `core_dispatch` 动作面）：
  `set_server`、`conv_unread`、`conv_mark_read`、`call_state`、`call_action`、
  `call_signal_post`、`call_signal_take`、`call_rtc_start`、`call_rtc_accept`、
  `call_rtc_hangup`、`call_rtc_status`、`safety_number`、`verify_peer`、
  `update_profile`、`friend_request`、`friend_requests_incoming`、`friend_accept`、
  `friend_decline`、`groups_create`、`wallet_balance`、`wallet_redpacket_cmd`、
  `wallet_claim`、`upload_media`、`download_media`、`connect_hub`、`send_text`、
  `outbox_status`、`outbox_retry`、`outbox_drop`、`net_diag`、`send_image`、
  `send_redpacket_msg`、`start_recv_pump`、`send_typing`、`forward_message`、
  `recall_msg`、`send_read_receipt`、`merge_forward`、`load_history`、
  `search_messages`、`favorite_add`、`favorites_list`、`sessions_list`、
  `session_revoke`、`sessions_revoke_others`、`favorite_remove`、`secret_enable`、
  `secret_enabled`、`secret_configure`、`secret_state`、`send_secret_text`、
  `send_voice`、`send_file`、`send_sticker`、`upload_video`、`send_video`、
  `download_video`、`upload_avatar`、`local_open`、`local_autopen`、`local_ready`、
  `local_history`。

注意：`core_bridge` 宿主泵（`app_state.rs` 的 recv pump → `forward_inbound`）
目前仍走旧渲染路径（`render_event_with` 扁平行），与旧命令同属待退役面；
规范事件链（QuicEventSource/subscribe_events）尚无生产调用方，见
`MOBILE-NATIVE-SHELLS` 台账证据。

## 退役条件（close_when，与台账同源）

1. 新 UI 覆盖旧 UI 独占的全部功能：通话 UI、秘密聊天、本地历史、钱包/红包、
   视频、收藏、会话管理。
2. 显式决策后移除：`ui/src`、`tauri.conf.old-ui.json`、`scripts/build-old-ui.sh`；
   62 个旧命令从 `generate_handler!` 移除（或改挂 `core_bridge` 动作面）。
3. `scripts/check-tauri-host.mjs` 的逃生阀检查同步更新（它现在**要求**旧配置存在，
   直接删文件会让守卫红——退役必须连守卫一起改）。
4. 从 `known-gaps.json` 移除 `OLD-UI-RETIREMENT` 条目。

## 现在怎么打包旧 UI（仅回退用）

```sh
bash scripts/build-old-ui.sh           # 准备 ui/dist 并打印打包命令
bash scripts/build-old-ui.sh --bundle  # 真的跑 tauri build（重，本机慎用）
```