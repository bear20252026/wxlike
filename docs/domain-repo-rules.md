# 旧仓储规则迁移：domain 只留规则，repo 只留 SQL

## 结论

`crates/platform/src/domain/*.rs` 过去同时承担业务规则和 SQL，导致两个问题：

1. 想换 PostgreSQL 时，规则和 SQLite 语句绑死；
2. handler 层能看到 `&Connection`，新代码很容易再写一条绕过仓储层的 SQL。

本批先把身份域的 **users / sessions / security** 三个模块收口：它们不再直接写
SQL，全部走 `domain/repo` 的端口；`Platform` 的公开方法签名保持不变，旧调用点
不需要改。第二批继续清 `account.rs`：资料补丁、注销、导出与清理语句全部
移入 `domain/repo/account.rs`，账户域生产代码 SQL 归零。第三批清
`groups.rs` 与 `communities.rs`：群成员/角色/可见性、社区频道话题的语句与
`execute_batch(format!)` 拼接全部移入各自仓储层，建群/建社区/建话题改用绑定
参数与事务。第四批清 `wallet.rs`：余额、账本、幂等记录、红包状态与过期退款
全部移入 `domain/repo/wallet.rs`，资金守恒的事务边界仍留在 domain。第五批清
`articles.rs`：发布者、订阅、文章主行/版本、列表、阅读游标与 reaction 语句
全部移入 `domain/repo/articles.rs`，渲染与可见性规则留在 domain。第六批清
`moments.rs`：发布、草稿、主行与计数、可见性查询、feed/相册分页、点赞与评论
语句全部移入 `domain/repo/moments.rs`，读时可见性判定与分页裁剪留在 domain。
第七批清 `friends.rs`：好友申请、通讯录行、备注、双向删除、增量游标语句全部
移入 `domain/repo/friends.rs`，申请状态机与授权顺序留在 domain。第八批清
`video_feed.rs`：发布/复核、可见性、点赞、评论、关注与删除语句全部移入
`domain/repo/video_feed.rs`，三道内容门禁与 feed 模式选择留在 domain。第九批清
`groups_admin.rs`：公告、禁言、审批开关、邀请 token、入群请求语句全部移入
`domain/repo/groups_admin.rs`，管理员断言与状态机留在 domain。第十批清
`live.rs`：在播会话、推流状态、观众进出、弹幕收件人语句全部移入
`domain/repo/live.rs`，凭据签名、权限校验与计数刷新时机留在 domain。第十一批清
`status.rs`：状态软清除、可见候选、点赞计数、背景草稿与到期清扫语句全部移入
`domain/repo/status.rs`，可见性判定与到期审计留在 domain。第十二批清
`favorites.rs`：收藏读页、对象快照、排序游标、标签更新与撤权语句全部移入
`domain/repo/favorites.rs`，撤权后清洗与游标解析留在 domain。第十三批清
`relations.rs`：拉黑、陌生人请求、直发许可、用户开关与好友元数据语句全部移入
`domain/repo/relations.rs`，关系状态机与输入校验留在 domain。第十四批清
`stickers.rs`：表情包、单张表情、安装行与排序语句全部移入
`domain/repo/stickers.rs`，媒体闸门与安装语义留在 domain。第十五批清
`recovery.rs`：验证码、恢复码与重置密码语句全部移入
`domain/repo/recovery.rs`，摘要校验与风控闸门留在 domain。第十六批清
`reconcile.rs`：余额、供给、红包算术与运行记录语句全部移入
`domain/repo/reconcile.rs`，差异判定与告警留在 domain。第十七批清
`notes.rs`：笔记主行、版本行、分页与删除语句全部移入
`domain/repo/notes.rs`，版本冲突与输入闸门留在 domain。

## 现在的边界

- `domain/*.rs`：DTO、校验、状态机、错误映射、调用仓储端口。
- `domain/repo/*.rs`：SQLite/PostgreSQL SQL 与行映射；可见性
  `pub(in crate::domain)`，handler 层编译期看不到。
- `wxlike-persistence`：跨 crate 的 `UsersRepo` / `SessionsRepo` trait 与 DTO，
  不依赖 rusqlite/sqlx 驱动。
- `Platform`：旧方法的适配层，保持既有调用面；后续可以按同一模式逐个模块迁移。

本批新增的端口能力：

- `UsersRepo::active_exists`：未注销账号存在性，和资料更新使用同一口径；
- `UsersRepo::update_profile`：三条资料 UPDATE 在一个事务里完成；
- `UsersRepo::totp_secret` / `totp_enabled`：TOTP 读取不再由 domain 拼 SQL；
- `SessionsRepo::device_name_by_sid`：刷新轮换沿用旧设备名；
- `SessionsRepo::device_id_by_token`：区分“会话行不存在”与“设备 id 为空”；
- `SessionsRepo::count_for_user`：注销前统计撤销的会话数；
- `repo::account`：导出六个域的取数、注销清理表清单与脱敏语句都只留在仓储层；
- `repo::groups` / `repo::communities`：群成员、角色、可见性、频道与话题语句
  全部下沉；domain 只保留成员上限、角色继承和发送闸门；
- `repo::wallet`：余额、钱包建立、账本读写、幂等行、红包行与过期扫描都在
  仓储层；domain 只保留金额校验、份额分配、领取状态机与事务边界；
- `repo::articles`：发布者主行、订阅关系、文章主行与版本、分页读模型、
  阅读游标和 reaction 行都在仓储层；domain 只保留渲染、可见性与文本校验；
- `repo::moments`：动态/草稿主行、计数列、feed 与相册读时可见性 SQL、点赞和
  评论行都在仓储层；domain 只保留输入闸门、可见性组合规则与游标裁剪；
- `repo::friends`：申请行、通讯录行、备注、双向删除与 contact_log 游标都在
  仓储层；domain 只保留申请状态机、权限校验与事务边界；
- `repo::video_feed`：视频主行、复核行、点赞/评论行、可见性查询与关注关系都在
  仓储层；domain 只保留转码/审核门禁、状态机与 feed 模式选择；
- `repo::groups_admin`：公告、禁言、审批开关、邀请 token 与入群请求行都在
  仓储层；domain 只保留管理员断言、成员上限与审批状态机；
- `repo::live`：在播会话、推流状态、观众行与弹幕收件人都在仓储层；domain 只
  保留推流凭据签名、权限校验、计数刷新时机与弹幕审核；
- `repo::status`：状态行、可见候选、点赞行、背景草稿与到期行都在仓储层；
  domain 只保留可见性组合、媒体闸门与到期审计；
- `repo::favorites`：收藏读页、对象快照、排序游标、标签更新与撤权语句都在
  仓储层；domain 只保留输入校验、快照裁剪与撤权清洗；
- `repo::relations`：拉黑、陌生人请求箱、直发许可、用户开关与好友元数据都在
  仓储层；domain 只保留关系状态机、权限顺序与输入校验；
- `repo::stickers`：表情包、单张表情、安装行与排序都在仓储层；domain 只保留
  媒体闸门、许可白名单与安装语义；
- `repo::recovery`：验证码、恢复码与重置密码语句都在仓储层；domain 只保留
  摘要校验、时效判断与风控闸门；
- `repo::reconcile`：余额、供给、红包算术与运行记录都在仓储层；domain 只保留
  差异判定、报告组装与告警；
- `repo::notes`：笔记主行、版本行、分页与删除都在仓储层；domain 只保留版本
  冲突、输入闸门与游标裁剪。

## 防回潮

新增 `ci/jobs/build/check_domain_sql.py` 和逐文件基线
`ci/jobs/policy/domain-sql-baseline.json`：

- 已列入基线的文件，SQL 数只能下降，不能上升；
- 新文件出现任何 SQL 直接失败；
- `--update` 默认拒绝把基线调高，必须显式 `--force`；
- 统计只看生产代码，遇到 root 级 `#[cfg(test)]` 就停止，测试夹具不算。

当前基线是 **13 个文件 / 53 条 SQL**；第一批从 **33 个文件 / 425 条**降下来，
users、sessions、security 三个文件已经是 **0**；第二批把 account 的 **67 条**
清零，第三批把 groups / communities 各 **29 条**清零，第四批把 wallet 的
**28 条**清零，第五批把 articles 的 **27 条**清零，第六批把 moments 的 **21 条**
清零，第七批把 friends 的 **19 条**清零，第八批把 video_feed 的 **17 条**清零，
第九批把 groups_admin 的 **17 条**清零，第十批把 live 的 **15 条**清零，
第十一批把 status 的 **14 条**清零，第十二批把 favorites 的 **14 条**清零，
第十三批把 relations 的 **13 条**清零，第十四批把 stickers 的 **12 条**清零，
第十五批把 recovery 的 **9 条**清零，第十六批把 reconcile 的 **8 条**清零，
第十七批把 notes 的 **8 条**清零，总数从 425 降到 53。

## 下一批顺序

按剩余 SQL 数量排：

1. `keys.rs`（6）：密钥环；
2. `perms.rs`（5）/ `content_index.rs`（5）；
3. 其余文件按 feature 逐个清到 0。

每批的验收标准一致：

- 该模块生产代码 SQL 计数为 0；
- `Platform` 公开签名不变；
- SQL 只留在 `domain/repo`，domain 不再依赖 rusqlite；
- `check_domain_sql.py` 基线只降不升；
- CI 的 `pg-migration` / `test` / `audit` 全绿。

## 不要做的事

- 不要把新 SQL 写回 `domain/*.rs`，即使只读一条也不行；
- 不要在 handler 层直接 `db().lock()` 拼 SQL；
- 不要为了让旧测试过而复制一份 SQLite 语句；
- 不要先改 51 个旧签名；它们是对外适配面，迁移顺序应从实现内部开始。
