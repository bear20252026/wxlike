# Zone C：社群层（社区 / 频道 / 话题）

> 2026-10-20 · 对应 `docs/task-assignment.md` §5 Zone C（C1–C4）与合同 §3.4（ADR-16 三支柱之三）
> 代码：`crates/platform/src/communities.rs` · `crates/platform/src/perms.rs` ·
> `crates/platform/src/db.rs` · `crates/message`（`sender_of` / `withdraw_as`）

## 一、两件事分开存：频道时间线 vs 话题

| 面 | 存在哪 | 序列号 |
|---|---|---|
| 频道时间线 | 读扩散信箱（`MessageStore`，owner=`Channel`） | 每频道一条单调 seq |
| 话题内回复 | `thread_messages`（平台库） | 每话题自持 AUTOINCREMENT seq |

话题回复**绝不写频道信箱**（不变式 3）。这条由专测钉住：
`thread_replies_never_touch_the_channel_timeline`、`c3_thread_mute_moves_fanout_not_reading`
——回复两条，频道时间线仍是 1 条。

技术上怎么保证的：话题回复的推送信封用 `MsgBody::Custom { kind: "thread.reply" }`，
而 `thread.reply` **故意不登记进消息类型注册表**（`channel_class_of`）。
`mirror_of` 认不出的类型不进时间线，所以这条通知只在会话里飘过。
加登记 = 打开一条"话题回复漏进主时间线"的路，别加。

## 二、两层角色，各管一件事

等分社区角色 + 频道角色是这次最容易写歪的地方，规矩说死：

| 层 | 表 | 档位 | 管什么 |
|---|---|---|---|
| 社区 | `community_members.role`（整数） | 0 成员 / 1 群主 / 2 管理员 | **建/删频道、踢人、提管理员** |
| 频道 | `channel_role.role`（字符串） | owner / moderator / member / guest | **在这个频道里能说什么、能不能撤别人的消息** |

频道没有专属行时按 `owner→owner`、`admin→moderator`、`member→member` 继承社区角色；
有行则以行内为准。于是"把某人压成 guest：只准打字、不准传附件"不必动他的社区身份。

权限集是纯函数（`channel_role_perms`），端点只问权限点不问角色名：

| 权限点 | guest | member | moderator | owner |
|---|---|---|---|---|
| `post-message`（文字） | ✅ | ✅ | ✅ | ✅ |
| `post-media`（图/文件/语音/视频/贴纸） | ❌ | ✅ | ✅ | ✅ |
| `create-thread` | ✅ | ✅ | ✅ | ✅ |
| `post-announce`（在公告频道发） | ❌ | ❌ | ✅ | ✅ |
| `pin-message` | ❌ | ❌ | ✅ | ✅ |
| `delete-any-message`（撤他人消息） | ❌ | ❌ | ✅ | ✅ |
| `manage-channel` / `manage-members` | ❌ | ❌ | ❌ | ✅ |
| `manage-roles`（授予频道角色） | ❌ | ❌ | ❌ | ✅ |

两条刻意的不对称：

* **moderator 提不动权**。授角色只归 owner——和 `perms.rs` 里"提权必须群主亲自做"
  同一条理由：被盗号的管理员不该能自我复制。
* **owner 不能靠这条路授予**。频道 owner 就是社区群主，转让是另一套流程；
  同时社区群主的行**不可被覆盖**，谁都降不动他（`set_channel_role` 直接 403）。

发一条消息要过三道闸，顺序写死：

1. **类型白名单**（`CLIENT_SENDABLE` = text + 5 种附件）。这是**白名单而不是黑名单**：
   `system` / `msg.forward` / `wallet.redpacket` 这些卡片由服务端逻辑落库，
   放开端点等于让任何人伪造成系统口吻发消息。名单外一律 422。
2. **类型权限点**：文字要 `post-message`，附件要 `post-media`（guest 没有）。
3. **频道形态**：announce 型频道再要一个 `post-announce`。

公告频道里的图片要同时持有两个点——"频道是广播口"这件事，不该被当成
"我已经是 moderator 所以什么都能发"。

## 三、话题订阅：**不收推送 ≠ 看不到**（与微信的差异点）

微信的"话题/群消息免打扰"是把整个会话压成一条角标：你**看不见**里面在说什么，
要点进去才展开。这里不是。

| | 微信 | wxlike |
|---|---|---|
| 话题的根消息 | 在群时间线里 | 在频道时间线里（**一直在**，与订阅无关） |
| 未订阅的人 | 靠角标提示，内容折叠 | **在频道时间线看得到根消息，点得进话题，翻得到全部回复** |
| 静音（`muted=1`） | 无此层 | 仍在订阅名单里、仍能读，只是从**推送扇出名单**里掉出去 |
| 退订 | 等同不再提醒 | 从扇出名单掉出去，读面**完全不受影响** |

一句话：**订阅只决定"谁被打扰"，不决定"谁能看"**。权限决定能不能看
（社区成员就能看），订阅决定要不要弹。

扇出名单是信箱 `MessageStore::members` **之外**的一层：

```
members(频道)  ── 谁拥有这个会话（未读、同步、能不能收）
thread_fanout  ── 其中谁要为这条话题被打扰 = 订阅 ∩ 仍在社区 ∩ 未静音 − 作者本人
```

`GET /api/threads/{id}/subscribers` 同时返回 `subscribers`（含 `muted` 标记）和
`fanout`（真正要推的那批），前端不必自己再算一遍交集——**算错的交集就是漏推送**。

顺带补了一个真缺的东西：频道的信箱成员快照。之前只有群（`groups.rs`）会
`reset_member_snapshot`，频道的快照一直是空的——也就是说频道时间线的扇出名单
根本不存在，未读只是靠"head − cursor"看着对。现在建频道落一次、入社区刷一次。
成本是 频道数 × 成员数 行整表重写（低频操作，宁可慢不要漏）；
真到万人社区 × 几十个频道，这里要换成增量成员日志（`append_member_change` 已留位置）。

## 四、迁移

`db.rs::migrate_communities()`，与其它 P3/P4 迁移同一手法：

* `channel_role` 是新表，`CREATE TABLE IF NOT EXISTS` +
  `CHECK (role IN ('owner','moderator','member','guest'))`——
  写坏的角色行比没有角色行更糟，字典必须在库里也说一遍。
* `thread_subs.muted` 走**老库原地补列**（`ALTER TABLE ... ADD COLUMN`，
  先 `PRAGMA table_info` 探一遍）。既有订阅行取默认 `0` = 不静音：
  升级不能把用户已有的订阅悄悄改成静音（专测 `c1_...` 钉住这条）。
* 角色名用字符串不用整数：社区级整数字典里**没有 guest**，两套整数迟早会有人拿错。

## 五、验收对应

| 卡 | 验收 | 测试 |
|---|---|---|
| C1 | 三张表 + 迁移；`cargo test -p wxlike-platform --test communities` | `c1_channel_role_table_and_muted_column_migrate_idempotently`（含重开幂等） |
| C2 | 非成员 403 / guest 不能发媒体 / moderator 能撤他人 | `c2_channel_roles_gate_message_types_and_recall` + 内联矩阵单测 |
| C3 | 订阅/退订影响扇出名单 | `c3_thread_mute_moves_fanout_not_reading`（Recorder 信封逐条对名单） |
| C4 | CLI 用 `wxlike-cmd` 调到 | `community.threads` / `channel.roles` 两条命令 + `c4_community_thread_rollup_and_role_endpoints` |
