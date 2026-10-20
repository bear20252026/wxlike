# 仿微信参考项目 → Rust 能力对照表

> 2026-10-20。起因：有人给了一份按 star 排序的 11 个"仿微信"项目清单，
> 要求"转化为 Rust 为核心的本项目"。
>
> **这份表的作用**：把那些项目里的**功能面**逐条映射到我们的 Rust 实现上，
> 标出"已经有 / 部分有 / 没有"，从而把"照抄 UI"变成"补 Rust 契约"。
> 参考项目是 Java/ObjC/Swift/Vue/ArkTS，我们一个字符都不抄；抄的是**能力清单**。

## 一、一句话结论

**那 11 个项目几乎全是"UI 克隆"，而我们的 Rust 侧已经覆盖了它们 90% 的功能面。**
真正缺的只有两样：**赞/评论的通知**、**按人可见（部分可见 / 不给谁看）**。
其余缺口全在 UI 层——这不是 Rust 的活，是 Zone A/B 的活。

## 二、对照表（只写核实过的）

| 能力 | 参考项目里的样子 | 我们的 Rust 侧 | 状态 |
|---|---|---|---|
| 朋友圈 feed | `refs/false-world` 列表页；`refs/hpf-circle` 的性能手法 | `platform/src/moments.rs::feed`（`seq` 游标、`limit≤50`） | ✅ |
| 朋友圈详情 | `false-world` 详情页 | `detail` + `moment_comments`（回复树） | ✅ |
| 九宫格图片 | `hpf-circle` 的 `NineImageAdapter`；`false-world` 的 XML | `MAX_MEDIA_PER_MOMENT = 9` + `media_ids`（校验存在性） | ✅ 数据面 |
| 点赞 | `false-world` 评论点赞区 | `like_moment` / `unlike_moment`（幂等，`ON CONFLICT DO NOTHING`） | ✅ |
| 评论与回复 | `false-world` 评论列表 | `comment_moment` + `reply_to` 邻接表（客户端自行还原嵌套） | ✅ |
| **删评论** | `false-world` 长按删除 | **本次新增** `delete_comment`（动态作者可删任何、评论者可删自己） | ✅ **新** |
| **私密（仅自己）** | 微信三态之一 | **本次新增** `Visibility::Private` + feed 的可见性过滤 | ✅ **新** |
| 部分可见 / 不给谁看 | 微信发布页的"谁可以看" | 无（只有 public/friends/private 三档，没有按人白/黑名单） | ❌ |
| 赞 / 评论通知 | 各项目都有红点 | 无 | ❌ |
| 语音条 | `LQRWeChat`、`box-im` 的 `chat-record` | `core::msgmeta::VOICE`（波形元数据） | ✅ 数据面 |
| 表情面板 | `hpf-circle` 的 `EmojiPanelView` | **Rust 侧不需要**（emoji 是 Unicode，不需要服务端表） | — |
| 红包 | `LQRWeChat`、`fake-world` | `core::types` + `ledger`（复式账本、守恒测试） | ✅ |
| 转账 | `LQRWeChat` | `ledger`（`transfer_out` 方向编码） | ✅ |
| 消息引用 | `box-im`、`LQRWeChat` | `Envelope.quote_of` | ✅ |
| @提醒 | `box-im` 的 `chat-at-box` | `core::types` 的 mention + `mq_hub` 的定向投递 | ✅ |
| 群已读 | `box-im` 的 `chat-group-readed` | `message` 的 read receipt（万人群默认关） | ✅ |
| 通讯录 / 好友 | 所有项目 | `platform/friends.rs` + `groups.rs` | ✅ |
| 钱包 / 交易记录 | `LQRWeChat`、`fake-world` | `ledger` + wallet | ✅ |

## 三、本次补的两处（有测试）

### 1. `Visibility::Private`（仅自己可见）

之前只有 `public` / `friends`。加 `private` 时**必须同时改 feed**，否则会出现
最难查的一类隐私泄漏：**详情接口挡住了，列表里却还在**。

```sql
-- 之前：feed 只按"作者是好友"过滤，私密动态会出现在好友的列表里
WHERE m.deleted = 0 AND m.author IN (...)

-- 现在：
WHERE m.deleted = 0 AND m.author IN (...)
  AND (m.visibility <> 'private' OR m.author = ?me)
```

测试 `a_private_moment_is_invisible_to_friends_but_not_to_its_author` 同时盯
两个入口；**把过滤去掉后它会红**（已实测：好友 feed 里出现了 `"visibility":"private"` 的那条）。

### 2. `DELETE /api/moments/{id}/comments/{cid}`

规则照微信：**动态作者可删任何人的评论，评论作者可删自己的**，第三人 404。
软删除（`deleted = 1`）——硬删会让 feed 的计数子查询与分页游标对不上。

测试 `comment_deletion_follows_the_wechat_rule`（三种身份）+ 
`deleting_a_comment_lowers_the_feed_count`（软删除不留幽灵计数）。

## 四、剩下的两个真缺口（建议各一张卡）

| 缺口 | 为什么不能顺手做 | 建议卡 |
|---|---|---|
| 赞 / 评论通知 | 需要一个"通知"抽象（不是消息、不进时间线、可聚合），当前没有；而且要和推送（`crates/push`）打通 | **D4 · 通知中心**：`notifications(user_id, kind, target, read)` + 朋友圈点赞/评论落一条 + 会话未读之外的红点 |
| 按人可见 | 要加两张表（白名单/黑名单）+ feed 的 SQL 变复杂（`NOT IN (SELECT ...)`），且要定"好友删除后可见性如何变化" | **C5 · 朋友圈可见性扩展**：`moment_audience(moment_id, user_id, allow)`，`public/friends/private/partial/exclude` 五档 |

## 五、那 11 个项目该怎么用（不是抄代码）

| 项目 | 语言 | 对我们有用的部分 | 用法 |
|---|---|---|---|
| `box-im`（697★，MIT，活跃） | Java + uni-app + Vue | **uni-app 6,674 行 / web 13,827 行**的交互面 | Zone A/B 的施工参考（`refs/box-im`） |
| `false-world`（MulanPSL-2.0） | Java + 153 XML | 朋友圈列表/详情/九宫格的**布局结构** | Zone B 朋友圈页的布局参考 |
| `hpf-circle`（无许可） | Java | 5 条 feed 性能手法（滑动不加载图、Adapter 不计算、Bean 预变换、弱引用缓存、减层级） | 只借思路，Flutter/web 通用 |
| 其余 8 个 | ObjC/Swift/Vue/Electron/ArkTS | 交互细节、红点位置、发布页字段 | 眼睛看，不动代码 |

**许可证边界**（写进 `reference-mirror.md`）：`refs/` 在仓库外、不进构建产物；
`hpf-circle` 无许可证，**只借思路**；`false-world` 是 MulanPSL-2.0，复制代码需保留声明；
另外四个（vue-WeChat / LZEasemob3 / Wechat_HarmonyOS / vue-wechat-desktop）**无 LICENSE，
一行都不能进源码**。
