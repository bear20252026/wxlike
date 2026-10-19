# P3 信箱（mailbox）：已有的与缺的

> 这份文档是一次**纠错**的产物。上一轮我在 `crates/platform` 里新写了一套
> `read_cursor` + 未读接口，写完才发现 `crates/message` 早就实现了同样的东西，
> 而且更完整。重复实现已回滚（commit `45ddbc3`）。**做 P3 之前先读这份。**

## `crates/message` 已经有的（不要再写一遍）

`MessageStore`（899 行）就是 ADR-16/17 的信箱基础设施，三支柱共用：

| 能力 | 方法 | 说明 |
|---|---|---|
| 三态属主 | `OwnerType::{Direct, Group, Channel}` | 一张 `mailbox` 表服务三种会话 |
| 每属主序列号 | `head_seq(owner)` / `mailbox(owner_type, owner_id, seq)` | **会话内**连续，正是读扩散要的 |
| 已读游标 | `ack(owner, user, seq)` | `last_ack_seq`，只前进不后退 |
| 未读数 | `unread(owner, user)` | `head − cursor`，O(1)，不随未读数增长 |
| 撤回 | `withdraw` / `withdraw_batch` | 逻辑删除 + 变更事件 |
| 变更流 | `changes(since)` | 客户端增量拉 |
| **五层差分** | `sync_plan(owner, client_version)` | `UpToDate` / `Delta` / `BatchCollapse` / `FullResync`——蓝图 P3 的"五层"就是它 |
| 成员分页 | `member_snapshot(owner, after_user, limit)` | 万人群不全量拉 |
| 成员变更 | `member_changes_since` / `append_member_change` | 入群退群也是流 |

## 真正的缺口

**只有 channel（`communities.rs`）和 wallet（红包卡片）在用 `MessageStore`。**
direct 与 group 的消息仍然走 `core::Store` + 平台扇出：

```
direct/group: hub → core::Store::append → 扇出给每个成员
channel:      hub → MessageStore::append(OwnerType::Channel) → 信箱
```

所以现在：

* 群的未读只能靠"数消息"（`COUNT(*) WHERE seq > cursor`），因为 `core::Store`
  的 `seq` 是**全局** AUTOINCREMENT，会话之间交错；
* 多设备同步没有统一的版本链；
* 撤回、变更事件在两条路径上语义不同。

## 进度（2026-10-19）

* ✅ **步骤 2 的 channel 部分已完成**：`GET /api/conv/{c}/unread`、
  `POST /api/conv/{c}/read`、`GET /api/conv/{c}/sync?version=` 直接调
  `MessageStore::{head_seq,unread,ack,sync_plan}`，没有新表。
  新增 `conv_owner(conv_id, owner_type)` 只解决"conv_id → 属主"的映射
  （direct 靠 `d-` 前缀推导，群/频道建的时候落表）。
* ⬜ **步骤 1 未做**：direct/group 仍未写进信箱，所以它们的未读是 0。

## 正确的下一步（按顺序）

1. **direct/group 也写进信箱**：hub 的 append 路径改成
   `MessageStore::append(OwnerType::Direct|Group, ...)`，`core::Store` 退化为
   "归档/搜索源"，不再承担序列号职责。
2. **HTTP 直接暴露信箱**：`GET /api/conv/{id}/unread` → `MessageStore::unread`；
   `POST /api/conv/{id}/read` → `ack`；`GET /api/conv/{id}/sync?version=` →
   `sync_plan`。**不新建表、不新建游标**。
3. **删掉平行机制**：`core::Store` 不要再长序列号；平台层不要再有 `read_cursor`。
4. 迁移：老库的 `device_cursor.last_seq`（全局 seq）按会话翻译一次。

## 教训

"先查参考项目"这条规矩同样适用于**自己的仓库**。`crates/message` 从 P3 开始
就在那里，`lib.rs` 顶部注释写得清清楚楚（三支柱、owner_type、序列号），
我却从 `platform/read.rs` 从零写起。开工前的三步里应该加第 0 步：
**先 `grep` 本仓库有没有同名能力**。
