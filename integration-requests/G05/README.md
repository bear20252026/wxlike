# G05 集成请求（内容、社交、收藏与检索）

六张卡（#126 #128 #131 #132 #135 #129）。**本组的实现选择与 G00 两个冻结项的
关系需要集成方拍板**，先说结论再列动作：

## 与 G00 纯移动拆分（#100）的冲突

G05 计划的三个 crate（`crates/features/content`、`crates/favorites`、
`crates/discovery-content`）**在冻结后的工作区里不存在**，而 #100 已把
`moments.rs` 等巨型文件的所有权划归 platform 并在拆分中。为避免出现"两份
权威实现"或一个不编译的工作区，G05 直接在 `crates/platform` 内实现：

- `moments.rs`（扩展：分组可见/草稿/评论层级/删帖联动/举报）
- `articles.rs`、`notes.rs`、`favorites.rs`（重写）、`stickers.rs`、
  `video_feed.rs`、`content_index.rs`、`inbox.rs`、`status.rs`（新模块）
- 路由在 `Platform::router` 合并，schema 在 `Db::migrate_g05`

等 #100 的目录搬迁落地后，再按本目录各 YAML 的 `paths` 整体搬出为独立
crate——模块边界已经按"每模块一个 `routes()` + 一个域"切开，搬迁是纯移动。

## 与 G00-03 契约冻结（#101）的关系

新增三个域契约：`contracts/domains/content`（content-v1）、
`contracts/domains/favorites`（favorites-v1）、`contracts/domains/search`
（search-v1），各带验收 fixtures。schema 草稿在 `schema/proposals/G05/`。

## 跨模块改动（需要属主确认）

1. `crates/report/src/lib.rs` 的 `TARGET_KINDS` 增加
   `moment / article / video / status`——内容举报必须进同一条工单流，否则
   G05-01 的举报卡无法真实落地。仅白名单加四项，无行为变更。
2. `crates/platform/src/db/schema.rs`（G00-02 搬出的常量）目前 `const` 为
   私有，父模块 `use schema::{SCHEMA, SCHEMA_P4}` 编译不过——已改为
   `pub(super)` 修复；G00-02 收尾时请保留该可见性。
3. `Db::migrate_g05` 挂在 `Db::open` 尾部；`SCHEMA_G05` 常量暂放 db.rs
   （避免与 schema.rs 的拆分冲突），搬迁时可随迁移一起走。

## 文件

| 文件 | 任务 | 要点 |
|---|---|---|
| `G05-01.yaml` | 朋友圈分组可见/草稿/层级/删帖联动/举报 | 读时判定可见性；删帖同事务清索引+撤收藏 |
| `G05-02.yaml` | 公众号与文章 | 渲染层拒绝恶意输入；version 乐观锁；未读游标 |
| `G05-03.yaml` | 收藏对象化/笔记/表情包 | 快照 vs 引用两类收藏；license 白名单 |
| `G05-04.yaml` | 视频号 | 转码 succeeded 门禁；三种信息流 |
| `G05-05.yaml` | 检索/热搜/通知 | 先授权后取正文；事务内通知 |
| `G05-06.yaml` | 我的状态 | 24h 服务端到期；sweeper 后台任务 |

验收命令统一为 `cargo test -p wxlike-platform --test g05_content`（8 个
端到端用例，全部真实 SQLite + 真实路由，无 mock）。
