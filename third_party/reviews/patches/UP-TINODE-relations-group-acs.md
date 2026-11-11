# UP-TINODE · `server/topic.go` → `crates/features/relations/src/group.rs`

@@HEAD@@
- 次要来源：`server/store/types/types.go`（`ModeCPublic/ModeCFull/ModeCAdmin/ModeCSharer`）

## 抄了哪个 upstream_path 的哪些行为

1. `Topic.getPerUserAcs`（`topic.go:237`）"要问权限就问这两列"的口径 → [`member_acs`]；
2. `Topic.accessFor(authLvl)`（`topic.go:3971`）：默认位 = `selectAccessMode(lvl, accessAnon, accessAuth, 类目默认)`
   → [`given_of_member`] 的三档（显式行 / 角色缺省 / 非成员）；
3. `anotherUserSub`（`topic.go:1986-2100`）**四条审批纪律一条不落**：
   - `hostMode := modeGiven & modeWant; if !hostMode.IsSharer() → 拒绝`（→ [`grant_member`] 第一段、[`may_invite`]）；
   - `modeGiven != Unset && !hostMode.IsAdmin() → "sharer cannot set explicit modeGiven"`
     （→ 整串赋值要求 `is_admin()`，增量谁都能表达）；
   - `modeGiven.IsOwner() && t.owner != asUid → "attempt to transfer ownership by non-owner"`
     （→ 只有 owner 走得到 `O` 位，且转让把老 owner 的 `O` 从 want/given 两边 `&^` 掉）；
   - 群成员数上限 `subsCount() >= maxSubscriberCount → ErrPolicy`（→ 既有 `MAX_GROUP_MEMBERS`）；
4. `t.isReadOnly()` 之后的 `ErrPermissionDenied`（群冻结后禁止写）→ 解散后
   [`given_of_member`] 把整群位夹到 `J|R`（只读，看得见发不了）；
5. `ModeCFull / ModeCPublic / ModeCAdmin(OA) / ModeCSharer(OAS)` 的**位定义**用于角色映射
   （[`acs_of_role`]、[`role_cap`]）；
6. `userIsReader`（`topic.go:258`）决定"这个人算不算读者"→ [`may_see`] 现在只看 `R` 位。

## Rust 化改了什么

- 上游的判定inline 在 `Topic` 的巨型状态机里、读内存 `t.perUser`；这里是纯函数 + SQL，
  并且**保留 `group_members.role` 作为平台兼容列**，位存在新表 `group_member_acs`，
  两者由 `set_role` / `grant_member` 双向同步（改角色 → 位按新角色重写，避免"降级却仍带 A"）；
- 新增**角色上限** `role_cap`：库里存在但越权的位（member 行写着 `O`）读的时候夹掉，
  合法位保留、越权位丢弃——上游没有这一层，因为它不信任 `given` 之外的第二真相源，
  而本平台同时有 `role` 与位两列，必须说明谁压倒谁；
- `kick` 的三段 `match role` → [`can_kick`]（`A`/`O` 位判定），
  并补了"目标必须是成员"的前置：公开群的非成员手里有 `J|R`（第 3 档默认位），
  不卡这一条 owner 就能"踢"一个从没进来过的人；
- 错误形状与既有契约对齐（`tests/group_rbac.rs`、`tests/corrupt_rows.rs`）：
  目标不是成员 → `NotFound`；发起方那行读不懂 → `Corrupt`，且**先读发起方**；
  拉人越权与"对方不接受入群"→ 同一个 `Forbidden("cannot invite")`。

## 没抄什么

- `share` 通知扇出（`sendImmediateSubNotifications`、`ModeCSharer` 用来决定"通知谁"）：
  通知归 G03/G11，本 crate 只落审计行；
- 频道（channel）那一档：`ModeCChnWriter/ModeCChnReader` 常量抄了，
  但 `isChan`/`asChan`/`verifyChannelAccess` 的产品形态本平台还没有，不接；
- `topic.go` 的 P2P 与 grp 共用一套 `Topic` 结构（本平台好友与群分开建模，两处各评一次位）；
- Tinode 的 `me`/`fnd`/`slf` 三个特殊话题本体（只借它们的默认位常量）。
