# UP-TINODE · `server/topic.go` → `crates/features/relations/src/friend.rs`

@@HEAD@@
- 次要来源：`server/store/types/types.go`（`Subscription.ModeWant/ModeGiven` 两列）、
  `server/utils.go`（`getDefaultAccess`）

## 抄了哪个 upstream_path 的哪些行为

`server/topic.go` 的订阅权限推导（1840-2100 行段），按"一对一关系"重写：

1. **累积权限 = `modeGiven & modeWant`**（`userIsReader:258`、`passesPresenceFilters:246`、
   `sub`/`leave` 里到处用的那一句）→ [`acs_toward`]，全仓库只有这一个出口；
2. `ownSub`（1780-1980）的三条纪律：
   - 订阅方**只能改 want，不能改 given**（→ [`set_want`] 只写 `mode_want`）；
   - `!want.IsJoiner()` 是自禁，重新订阅要解除（→ [`rejoin`]）；
   - 解除自禁时 `modeWant = modeGiven | accessFor(lvl)`，注释原文
     *"Set permissions NO WORSE than default, but possibly better"*（→ [`rejoin`] 的实现就是这一行）；
3. `anotherUserSub`（1986-2075）的 P2P 夹紧：`modeGiven = (modeGiven & ModeCP2P) | ModeApprove`
   （→ [`grant`] 里的 `clamp_p2p`：单聊里 `S/D/O` 无意义，且批准权摘不掉）；
4. `subs.ModeWant/ModeGiven` 作为**每一对关系的两列**这一存储形状
   （→ `friend_acs(user_id, peer_id, mode_want, mode_given)`，方向各存一份）；
5. `types.DefaultAccess{Auth, Anon}` 的"两类访问者各一把默认位"
   （→ `friend_policy.access_auth/access_anon` + [`policy`]）；
6. 拉黑/停权的判定位置：上游在 `sub` 里 `!modeGiven.IsJoiner() → ErrPermissionDenied`，
   本文件的 [`given_to`] 同样把"任一方向拉黑"短路成 `Acs::NONE`。

## Rust 化改了什么

- 上游这些判定散落在 700 行的 `Topic.sub` 状态机里，且直接对着 `t.perUser` 内存表；
  这里全部改成**显式的 SQL 读 + 纯函数推导**，没有内存订阅缓存，也就没有"缓存与库不一致"这一类 bug；
- 错误：`ErrPermissionDenied` / `errors.New("topic access denied; ...")` → `RelError::Forbidden`，
  且**新增**一条产品纪律：`send()` 里"被拉黑"与"对方关掉了加好友"回**同一个**
  `Forbidden("blocked")`，HTTP 层（`crates/platform/src/relations.rs`）映射成同一个响应，
  不给枚举留缝；
- 存储的字符串读不懂 → `RelError::Corrupt`（上游把 `ParseAcs` 的错误直接抛给客户端），
  这里刻意不折成 `NONE`：一行坏字符不该把两个人的关系判成互相屏蔽；
- `Option<&str>` / `String` 入参统一 `&str`；`policy()` 无行时返回 `DefaultAcs::PLATFORM_DEFAULT`
  而不是报错（下面"没抄什么"第 3 条解释了为什么这是有意的 fail-open）。

## 没抄什么

- `t.perUser` 内存订阅表、`computePerUserAcsUnion`（`modeWantUnion/modeGivenUnion`，代理会话用的那套）；
- 插件钩子 `pluginSubscription(&sub, plgActUpd)`、`notifySubChange` 的在场通知扇出（G03 的活）；
- `usersUpdateUnread` 的未读计数联动（本平台未读在消息域算）；
- `store.Subs.Get(... deleted=true)` 那条"被拒申请的历史 modeWant 要复用"的逻辑：
  本平台的 `friend_requests` 状态机已经承担了这件事，不再从订阅表回捞；
- **默认策略方向与上游相反**（有意的）：上游 `getDefaultAccess(fnd) = ModeNone`、
  匿名档 `accessAnon` 通常为空，即"默认不让加"；本平台 G02 之前的既有行为是
  "任何账号都能发申请，被加方有权不接"，所以 `PLATFORM_DEFAULT` 的 anon 档给了
  `ModeCPublic`（含 `S`）。若改成上游口径，所有存量好友关系会一夜之间加不了——
  登记在此，避免以后被当成抄漏。
