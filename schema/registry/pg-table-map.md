# PG 表映射（SQLite → PostgreSQL）— 评审草稿

> 生成于 2026-11-21，基线 `39b9315f`。这是 **G00-04 S2 的开工前置**，不是已执行的迁移。
> 机器可读版本：`schema/registry/pg-table-map.json`；本文是评审用摘要。
>
> **⚠️ 2026-11-21 口径修正**：G02/G04/G05 的 SQL 是 SQLite DDL（`AUTOINCREMENT`、
> `MediaDb::open`、`Db::migrate_g05()`），之前被误计入 PG 目标。实际 PG DDL 只有
> G06/G07/G09（61 张）；116 张平台表里只有 20 张 `commerce_*` 同名，真实缺口是
> **96 张**（89 port + 7 rewrite）。本文下面的 48/49/67 统计是旧口径，**已作废**，
> 在重算完成前只能当历史草案。M011 已按真实 PG 目标写，不依赖这些统计。
> 详见 `docs/g00-04-s2-pg-target-dialect.md`。

## 1. 口径

本表覆盖平台主库（`Db::open` + `storage.rs` 迁移链）的 **116 张业务表**：
`crates/platform/src/**/*.rs` + `crates/features/**/*.rs` 中的 `CREATE TABLE`（已剔除 `#[cfg(test)]`）。
**不含** adminx 独立库、client 本地库，也不含 message/video/core/net 的邻接表（见 §5）。

| 动作 | 数量 | 含义 |
|---|---:|---|
| direct | 67 | PG 已有同名表且列名对齐；复核类型/PK/约束 |
| port | 42 | PG 缺表或缺列；补同名 DDL，SQL 不改 |
| compat-view | 0 | 加兼容视图；本草案未使用 |
| rewrite | 7 | PG 目标模型不同；repo/SQL 改写 |
| drop | 0 | 遗留表；本草案未使用 |

**缺失表 48 张**（PG 无同名对象），其中 41 张建议 port、7 张建议 rewrite；另有 1 张同名表 `group_meta` 缺列（frozen_at），按 port 处理。

## 2. 先做 P0：没有这两张表，PG 上第一条平台 SQL 就会失败

| SQLite | 动作 | PG 目标 | 引用 | 说明 |
|---|---|---|---:|---|
| `users` | port | `users` | 56 | 56 处 SQL 引用；G02 提案假设 users 已存在，却没有给 PG DDL。先同名 port，并带上 deleted_at。 |
| `sessions` | port | `sessions` | 40 | 40 处引用；PG identity_refresh 是刷新令牌链，不是访问会话。先同名 port，并带上 audience。 |

## 3. 48 张缺失表的逐张裁决

| # | SQLite | 动作 | PG 目标 | 域 | 优先级 | 引用 | 置信度 | 理由 / 未决问题 |
|---:|---|---|---|---|---|---:|---|---|
| 1 | `sessions` | port | `sessions` | identity | P0 | 40 | high | 40 处引用；PG identity_refresh 是刷新令牌链，不是访问会话。先同名 port，并带上 audience。 **未决**：长期是否把 HTTP 会话合并到 identity_session_token？ |
| 2 | `users` | port | `users` | identity | P0 | 56 | high | 56 处 SQL 引用；G02 提案假设 users 已存在，却没有给 PG DDL。先同名 port，并带上 deleted_at。 **未决**：是否保留 users 作为账户根表，还是建 identity_user + users 兼容视图？ |
| 3 | `captcha` | port | `captcha` | identity | P1 | 3 | high | 图形验证码；PG identity_challenge 是短信挑战（phone_e164/code_hash），字段不可互换。 |
| 4 | `devices` | port | `devices` | identity | P1 | 4 | medium | 推送 token 注册表（每 user+platform 一行）；PG 没有对等表。先 port 保 4 处 SQL。 **未决**：与 message.push_device 明显重复（device_id/token/epoch），是否统一到 push_device？ |
| 5 | `identity_retry` | port | `identity_retry` | identity | P1 | 9 | high | TTL 重试桶；PG identity_throttle 是固定窗口（window_start/blocked_until），语义不同，不能直接替。 **未决**：是否把 identity_throttle 改成 TTL 桶，删掉 identity_retry？ |
| 6 | `identity_session_token` | port | `identity_session_token` | identity | P1 | 7 | high | 访问会话元数据（auth_level/features/serial）；PG identity_refresh 是刷新链，不对等。 |
| 7 | `identity_token_serial` | port | `identity_token_serial` | identity | P1 | 2 | high | 单行全局 token 序列号；PG 用一行表或 sequence 都可，先 port。 |
| 8 | `prekey_bundles` | rewrite | `security_device_key` | identity | P1 | 8 | medium | SQLite 是 user 级单 bundle；PG security_device_key 是 device 级（device_id/epoch/revoked）。 **未决**：prekey API 必须开始带 device_id；旧客户端单设备如何回填 device_id？ |
| 9 | `qr_ticket_creator` | port | `qr_ticket_creator` | identity | P1 | 2 | medium | 扫码票 creator/verifier 审计列；PG identity_qr 只有 scanned_by/decided_at，不能一一对应。 **未决**：是否把 creator/verifier 并进 identity_qr，删掉独立表？ |
| 10 | `recovery_codes` | port | `recovery_codes` | identity | P1 | 5 | high | 账户找回码；PG 无对等表。 |
| 11 | `secret_settings` | port | `secret_settings` | identity | P1 | 2 | high | 会话级阅后即焚开关/TTL；PG 无对等表。 |
| 12 | `secret_verifications` | port | `secret_verifications` | identity | P1 | 3 | medium | 会话级 peer identity 校验；PG security_device_verify 是设备级（peer_device/safety_number），粒度不同。 **未决**：E2EE 多设备后是否统一到 security_device_verify？ |
| 13 | `user_settings` | rewrite | `identity_pref` | identity | P1 | 4 | high | 只有 allow_strangers 一列；PG identity_pref 是 user/key/value 模型，改写 4 处查询即可。 **未决**：allow_strangers 的 key 命名与默认值要在 G02 侧固定。 |
| 14 | `contact_log` | port | `contact_log` | relations | P1 | 5 | high | 联系人变更流水；PG 没有对等表。 |
| 15 | `friend_acs` | port | `friend_acs` | relations | P1 | 2 | high | 好友访问控制模式；PG relations.sql 未覆盖。 |
| 16 | `friend_policy` | port | `friend_policy` | relations | P1 | 2 | high | 好友默认访问策略；PG relations.sql 未覆盖。 |
| 17 | `group_announcement` | port | `group_announcement` | relations | P1 | 2 | high | 群公告；PG relations.sql 未覆盖。 |
| 18 | `group_invites` | port | `group_invites` | relations | P1 | 3 | high | 群邀请 token；PG relations.sql 未覆盖。 |
| 19 | `group_join_requests` | port | `group_join_requests` | relations | P1 | 4 | high | 入群申请；PG relations.sql 未覆盖。 |
| 20 | `group_member_acs` | port | `group_member_acs` | relations | P1 | 5 | high | 群成员访问控制模式；PG relations.sql 未覆盖。 |
| 21 | `group_mutes` | port | `group_mutes` | relations | P1 | 4 | high | 群禁言；PG relations.sql 未覆盖。 |
| 22 | `channel_role` | port | `channel_role` | content | P1 | 5 | high | 频道角色；PG 没有社区/频道模型。 |
| 23 | `channels` | port | `channels` | content | P1 | 10 | high | 社区频道；PG 没有社区/频道模型。 |
| 24 | `communities` | port | `communities` | content | P1 | 4 | high | 社区主表；PG 没有社区模型。 |
| 25 | `community_members` | port | `community_members` | content | P1 | 12 | high | 社区成员；PG 没有社区模型。 |
| 26 | `favorites` | port | `favorites` | content | P1 | 16 | high | 收藏是独立内容实体（source_conv/source_msg_id/kind/media_id），不能塞进 content_index。 |
| 27 | `moment_comments` | port | `moment_comments` | content | P1 | 11 | high | PG G05 没有朋友圈评论表。 |
| 28 | `moment_likes` | port | `moment_likes` | content | P1 | 7 | high | PG G05 没有朋友圈点赞表。 |
| 29 | `moments` | port | `moments` | content | P1 | 11 | high | PG G05 只有 moment_drafts（草稿），没有已发布朋友圈主表。 |
| 30 | `notification_settings` | port | `notification_settings` | content | P1 | 4 | high | 按会话的免打扰级别/关键词；PG 无对等表。 |
| 31 | `thread_messages` | port | `thread_messages` | content | P1 | 3 | high | 帖子回复；PG 没有社区/频道模型。 |
| 32 | `thread_subs` | port | `thread_subs` | content | P1 | 8 | high | 帖子订阅；PG 没有社区/频道模型。 |
| 33 | `threads` | port | `threads` | content | P1 | 5 | high | 频道帖子；PG 没有社区/频道模型。 |
| 34 | `media` | rewrite | `media_object, media_ref` | media | P1 | 12 | medium | media 的 uploader/content_type/has_thumb 对应 media_object.owner/mime；conv_id/public 对应 media_ref.audience/purpose。 **未决**：12 处 SQL 是改写还是先建 media 兼容视图 + INSTEAD OF 触发器？ |
| 35 | `conv_owner` | port | `conv_owner` | messaging | P1 | 2 | high | 会话 owner_type 投影；PG 没有对等表。 |
| 36 | `message_requests` | port | `message_requests` | messaging | P1 | 7 | high | 陌生人消息请求；PG 没有对等表。 |
| 37 | `ledger` | rewrite | `ledger_journals, ledger_entries` | ledger | P1 | 8 | high | 单边钱包流水（user_id/kind/amount）必须映射到 G06 双分录（journal + entries + accounts）。 **未决**：旧 ledger 行如何生成 journal/entries/accounts 的开户与期初余额？需要数据迁移脚本。 |
| 38 | `wallet_idem` | rewrite | `commerce_transfers, ledger_holds` | ledger | P1 | 4 | medium | 幂等键在 PG 由 commerce_transfers/ledger_holds 的 business_key + request_digest 承担。 **未决**：通用 wallet 接口是否统一走 commerce_transfers，还是保留独立 idem 表？ |
| 39 | `wallets` | rewrite | `ledger_accounts` | ledger | P1 | 16 | high | balance 列在 PG 模型里不存在；余额只能由 ledger_entries 聚合，账户用 ledger_accounts。 **未决**：读余额的 16 处查询改成聚合视图还是物化余额表？ |
| 40 | `redpackets` | rewrite | `commerce_redpackets, commerce_redpacket_claims, commerce_redpacket_shares` | commerce | P1 | 12 | medium | PG 把红包拆成 packet/claims/shares；SQLite 的 claimed/claimed_total/claimers 是聚合列。 **未决**：聚合列是否保留为视图，还是全部改成分表查询？ |
| 41 | `idempotency` | port | `idempotency` | ops | P1 | 2 | high | 通用 key→response 幂等表；PG 的 services_order_idempotency 只覆盖服务订单，不通用。 |
| 42 | `moderation_queue` | port | `moderation_queue` | ops | P1 | 8 | high | 审核队列；PG 无对等表。 |
| 43 | `reports` | port | `reports` | ops | P1 | 10 | high | 内容举报单（状态机 + action/history）；PG 无对等表。 |
| 44 | `reconcile_runs` | port | `reconcile_runs` | ledger | P2 | 4 | high | 对账运行摘要；PG commerce_reconciliation_exceptions 只存异常，不存每次 run 的合计。 |
| 45 | `live_sessions` | port | `live_sessions` | live | P2 | 12 | medium | 直播场次；PG G09 只有 calls，没有 live 表。 **未决**：live 域是否随平台 PG 一起迁，还是继续 SQLite？ |
| 46 | `live_viewers` | port | `live_viewers` | live | P2 | 8 | medium | 直播观众；PG G09 只有 calls，没有 live 表。 |
| 47 | `video_refs` | port | `video_refs` | live | P2 | 5 | medium | 视频转码产物索引；PG G09 只有 calls，没有 video 表。 **未决**：video 域是否随平台 PG 一起迁，还是独立 SQLite sidecar？ |
| 48 | `audit_log` | port | `audit_log` | ops | P2 | 3 | high | 通用审计流水；PG 治理审计表按身份/群拆分，无法一一对应。 |

## 4. 缺列/补列清单（含 port/rewrite）

同名 `direct` 表的列名已对齐；下列是需要额外补列或列映射的条目：

| SQLite | PG | 缺列 | 来源 | 处理 |
|---|---|---|---|---|
| `group_meta` | `group_meta` | `frozen_at` | `features/relations/src/governance.rs` ALTER 补列 | port：补进 PG DDL |
| `users` | `users`（port） | `deleted_at` | `storage.rs` ALTER 补列 | port 时一并建 |
| `sessions` | `sessions`（port） | `audience` | `features/identity/src/lib.rs` ALTER 补列 | port 时一并建 |
| `media` | `media_object`+`media_ref`（rewrite） | `conv_id` / `public` | `storage.rs` ALTER 补列 | 迁到 `media_ref.audience/purpose` |
| `prekey_bundles` | `security_device_key`（rewrite） | `signature` | `storage.rs` ALTER 补列 | PG 目标已有 signature |

## 5. 邻接表：不在 116 张里，但今天和平台共用一个 SQLite 文件

`server/src/lib.rs:208-232` 用同一个 `opts.db_path` 打开 platform / message / ledger / video 等 store。
割接前必须裁决这 23 张邻接表是并入 PG，还是拆独立 SQLite sidecar；**不要因为它们不在这张表里就当作不存在**。
`crates/ledger` 的 6 张表已在 M010 落地，列在下表最后一行仅作对账。

| 来源 | 数量 | 表 |
|---|---:|---|
| `crates/message` | 16 | `conversation_clear`, `conversation_event`, `conversation_pref`, `conversation_projection`, `delivery_task`, `device_cursor`, `ephemeral`, `mailbox`, `mailbox_change`, `member_log`, `member_snapshot`, `message_submit`, `msg_user_state`, `owner_seq`, `push_device`, `read_cursor` |
| `crates/video` | 3 | `video_jobs`, `video_owners`, `watch_events` |
| `crates/core` | 2 | `delivery_outbox`, `outbox` |
| `crates/net` | 1 | `offline_inbox` |
| `crates/rtc` | 1 | `call_room_audit` |
| `crates/ledger`（已在 M010） | 6 | `ledger_accounts`, `ledger_closing_balances`, `ledger_closings`, `ledger_entries`, `ledger_holds`, `ledger_journals` |

## 6. 开工顺序

1. **P0**：`users` / `sessions` 同名 port（带 `deleted_at` / `audience`），先把平台的基础查询在 PG 上跑通。
2. **P1 身份**：`devices`、`identity_session_token`、`identity_token_serial`、`identity_retry`、`captcha`、`qr_ticket_creator`、`recovery_codes`、`secret_settings`、`secret_verifications`、`notification_settings` port；`user_settings→identity_pref`、`prekey_bundles→security_device_key` rewrite。
3. **P1 关系/内容**：`friend_*` / `group_*` / `favorites` / `moments*` / `communities` / `channels` / `threads` port；`media→media_object+media_ref` rewrite 单独做。
4. **P1 钱**：`ledger` / `wallets` / `redpackets` / `wallet_idem` rewrite 到最后，跟数据导入、影子读对账一起做。
5. **P2**：`live_*` / `video_refs` / `audit_log` / `reconcile_runs` port，等 scope 决策。

## 7. 必须先拍的 8 个问题

1. users：保留 users 作账户根表，还是 identity_user + users 兼容视图？
2. sessions：保留同名会话表，还是合并到 identity_session_token / identity_refresh？
3. media：media(conv_id/public) 直接改写 media_object+media_ref，还是先建 media 兼容视图？
4. ledger/wallets：单边钱包流水迁双分录，旧行如何开户、期初余额与对账口径？
5. prekey_bundles：user 级 bundle 迁 device 级 security_device_key，旧单设备客户端如何回填 device_id？
6. message/video/core/net 23 张邻接表：并入同一 PG，还是拆独立 SQLite sidecar？
7. devices 与 message.push_device 重复：合并到 push_device，还是保留两张表？
8. 所有 SQLite ALTER 补列（users.deleted_at / sessions.audience / group_meta.frozen_at）必须进 PG DDL，否则同名表也会缺列。

## 8. 本草案不做什么

- 不新建 `schema/registry/pg/*.sql` 包装层；DDL 仍以 `schema/proposals/**` 为唯一来源。
- 不分配 M011+ 编号，不改 `migrations.json`；编号分配留给 G00 串行执行。
- 不修改任何 Rust 代码；这是评审输入，不是执行结果。
