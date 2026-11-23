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
`domain/repo/notes.rs`，版本冲突与输入闸门留在 domain。第十八批清
`keys.rs`：预密钥包、认领关系、身份公钥与一次性密钥弹出语句全部移入
`domain/repo/keys.rs`，包形状校验与签名校验留在 domain。第十九批清
`perms.rs`：频道改名/改类型/删除与成员角色/踢人语句全部移入
`domain/repo/perms.rs`，权限矩阵与提权顺序留在 domain。第二十批清
`content_index.rs`：文章授权回源、媒体引用、候选页、正文与热搜语料语句全部
移入 `domain/repo/content_index.rs`，泳道归并、摘要与热搜打分留在 domain。
第二十一批清 `report.rs`：工单读取、举报人/队列索引与 upsert 语句全部移入
`domain/repo/report.rs`，状态机流转与错误映射留在 domain。第二十二批清
`secret.rs`：会话开关/TTL、身份公钥与核对记录语句全部移入
`domain/repo/secret.rs`，TTL 白名单、安全码计算与核对语义留在 domain。
第二十三批清 `trust.rs`：挂起入队、队列读取、单条取出与删除语句全部移入
`domain/repo/trust.rs`，判定结论、放行投递语义与审计留在 domain。
第二十四批清 `inbox.rs`：收件箱分页、未读计数与两种已读推进语句全部移入
`domain/repo/inbox.rs`，分页裁剪、id 校验与入参约束留在 domain。
第二十五批清 `media.rs`：内容寻址行的幂等写入、归属与元数据读取语句全部移入
`domain/repo/media.rs`（含 SQLite/PG 两条写入语句），摘要、授权与签名留在 domain。
第二十六批清 `devices.rs`：设备 token 读取、注册 upsert 与列表语句全部移入
`domain/repo/devices.rs`，平台白名单与 token 形状校验留在 domain。
第二十七批清 `governance.rs`：治理镜像、群数与举报数语句移入
`domain/repo/governance.rs`，事件分发、缓存清理与断连留痕留在 domain。
第二十八批清 `notification.rs`：免打扰规则 upsert、单条读取与列表语句全部移入
`domain/repo/notification.rs`，级别/关键词校验与 JSON 解码留在 domain。
第二十九批清 `qr_login.rs`：票据字段读取、抢占式消费与用户显示名语句移入
`domain/repo/qr_login.rs`，状态机调用、secret 比对与错误映射留在 domain。
第三十批清 `read.rs`：会话属主读取与登记语句移入 `domain/repo/read.rs`，
参与者闸门与信箱调用留在 domain。至此 `domain/*.rs` 生产代码 SQL 归零。
第三十一批补清 `video_refs.rs`：转码回调的幂等写入与引用读取语句移入
`domain/repo/video_refs.rs`。这两条是 `r#"..."` 原始字符串，此前被门禁的
普通双引号正则漏检；本批同时把 `check_domain_sql.py` 的匹配扩到原始字符串，
收紧口径后仍为 0/0。

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
  冲突、输入闸门与游标裁剪；
- `repo::keys`：预密钥包、认领关系、身份公钥与一次性密钥弹出都在仓储层；
  domain 只保留包形状、签名校验与认领授权；
- `repo::perms`：频道改名/改类型/删除与成员角色/踢人语句都在仓储层；domain
  只保留权限矩阵、提权顺序与成员保护规则；
- `repo::content_index`：索引写入/删除、媒体引用、候选页、正文与热搜语料都在
  仓储层；domain 只保留授权回源顺序、泳道归并、摘要截取与热搜打分；
- `repo::report`：工单读取、举报人/队列索引与 upsert 都在仓储层；domain 只保留
  状态机流转、审计与错误映射；
- `repo::secret`：会话开关/TTL、身份公钥与核对记录都在仓储层；domain 只保留
  TTL 白名单、安全码计算与核对失效语义；
- `repo::trust`：挂起入队、队列读取、单条取出与删除都在仓储层；domain 只保留
  判定结论、放行投递语义与审计；
- `repo::inbox`：收件箱分页、未读计数与已读推进都在仓储层；domain 只保留分页
  裁剪、id 校验与入参约束；
- `repo::media`：内容寻址幂等写入（含方言分支）、归属与元数据读取都在仓储层；
  domain 只保留摘要校验、缩略图、授权与签名；
- `repo::devices`：token 读取、注册 upsert 与列表都在仓储层；domain 只保留平台
  白名单与 token 形状校验；
- `repo::governance`：治理镜像、群数与举报数都在仓储层；domain 只保留事件
  分发、缓存清理与断连留痕；
- `repo::notification`：免打扰规则 upsert、单条读取与列表都在仓储层；domain 只
  保留级别/关键词校验与 JSON 解码；
- `repo::qr_login`：票据字段读取、抢占式消费与用户显示名都在仓储层；domain 只
  保留状态机调用、secret 比对与错误映射；
- `repo::read`：会话属主读取与登记都在仓储层；domain 只保留参与者闸门与信箱
  调用；
- `repo::video_refs`：转码回调的幂等写入与引用读取都在仓储层；domain 只保留
  令牌校验、URL 形状校验与错误映射。

## 第三十二批：crate 根会话/审计 SQL 收口

`domain/*.rs` 清零不等于平台生产 SQL 清零。把门禁口径往外扩一层后，crate 根的
`lib.rs` 还留着 **8 条** SQL：会话签发、token 解析、撤销、用户名解析、审计读写。
它们不属于任何 feature，所以一直留在基础设施文件里。

本批按归属拆开：

- 会话签发/解析/撤销 → `domain/sessions.rs` + `repo/sessions.rs`；认证专用的
  "会话有效且账号未注销"仍是一条 SQL（`lookup_live`），不在 domain 里做两次查询；
- `user_id_by_name` → `domain/users.rs`，走已有的 `SqliteUsersRepo`；
- 审计读写 → `domain/audit.rs` + `repo/audit.rs`；时间戳与条数夹紧留在 domain，
  写失败仍只留日志、不阻断业务。

`Platform` 的公开方法签名与调用点一个都没改；`lib.rs` 只剩构造、路由装配等
基础设施，生产代码 SQL 归零。

## 第三十三批：identity_admin 的 handler 直连 DB 收口

`identity_admin.rs` 是最后一个在 handler 层拿 `platform.db.lock()` 的文件：
5 个管理端点各自在 `spawn_blocking` 闭包里拼"连接 + identity/relations 域调用
+ 群缓存失效"。虽然没跨 `await` 持锁，但 handler 仍然拥有连接和事务顺序。

本批把这套编排移进 `domain/identity_admin.rs`：

- `admin_suspend` / `admin_unsuspend` / `admin_revoke_device` / `admin_user_view`
  各自拿一次连接并调用 identity 域；
- `admin_govern_group` 把 Admin audience 鉴权、relations 治理和群缓存失效
  放进同一个连接守卫；
- identity/relations 错误映射（`id_err` / `rel_err`）随编排一起留在 domain，
  handler 只保留 HTTP 形状和 `spawn_blocking`。

handler 从此不再出现 `.db().lock()`；根层只有 `lib.rs` 的 WAL checkpoint、
`db.rs`、`storage.rs` 三个基础设施文件允许直接拿连接。

## 第三十四批：handler ADR-28 TODO 清零

`friends`、`groups_admin`、`groups` 三个 handler 文件里还挂着 11 个
`TODO(ADR-28)`：通讯录的五个读、公告/申请列表两个群管理读、群详情/成员/我的群
三个读，最后都在 `async fn` 的 await 上下文里直接跑同步查库。

本批逐个改成 `offload` / `offload_api`：

- 单段读直接 `offload(&platform, move |p| p.method(..))`；
- 需要保留 403/404 文案的读（群公告、群详情、成员名单）用 `offload_api`，
  把鉴权、存在性判断和查询收进同一次阻塞执行；
- 群详情原来的 4 次上锁、成员名单的 `is_member + 查询` 都合并到一次任务，
  响应形状与错误码不变。

三个文件里的 `TODO(ADR-28)` 归零；`crates/platform/src/*.rs` 全文搜索也为 0。

## 第三十五批：account handler 的同步查库进阻塞池

`account.rs` 是 handler 层里剩余同步查库最集中的文件：偏好读写/增量同步、
step-up 发证、导出作业的提交/查询/取消、导出链接和账号注销，跨 10 个
`async fn` 的多处同步调用仍直接跑在 await 上下文里。

本批按"一次阻塞任务 = 一段完整业务顺序"处理：

- 单段读（偏好文档、增量、作业详情、作业列表）直接 `offload`；
- 多步写（偏好补丁、step-up、导出/取消/注销）用 `offload_api`，把鉴权
  （`take_step_up` / `step_up_gate` / `confirm_identity`）、写库、审计收进同一
  次阻塞执行，响应码与错误文案不变；
- 口令与 TOTP 只在闭包内参与 `confirm_identity`，审计仍只记用途和结果；
- `download` 保持不动：`take_export_download` 本身是 async，且取件故意不要求
  Bearer。

`account.rs` 同时加入 `adr28_async_db_guard.py` 的 `ENFORCED_FILES`，此后该文件
再出现"async 里直接调同步查库"就会在 CI 判红。当前 guard 名单为
`friends / groups / groups_admin / inbox / devices / keys / account`，加上
`crates/admin/src` 整 crate。

## 防回潮

新增 `ci/jobs/build/check_domain_sql.py` 和逐文件基线
`ci/jobs/policy/domain-sql-baseline.json`：

- 扫描范围是 `crates/platform/src/*.rs`（handler 根层）+ `domain/*.rs`；
  `storage.rs` 是存储端口/迁移 DDL，单独排除；
- 根层 handler 的 `.db().lock()` / `.db.lock()` 是零容忍：除 `lib.rs`
  （WAL checkpoint）、`db.rs`、`storage.rs` 外，出现即失败；
- 基线里的文件，SQL 数只能下降，不能上升；
- 不在基线里的文件出现任何 SQL 直接失败（当前基线为空 = 全部文件零容忍）；
- `--update` 默认拒绝把基线调高，必须显式 `--force`；
- 统计只看生产代码，普通双引号与 `r#"..."` 原始字符串都算，遇到 root 级
  `#[cfg(test)]` 就停止，测试夹具不算。

当前基线是 **0 个文件 / 0 条 SQL**，也就是"任何新 SQL 直接失败"的零容忍
口径；第一批从 **33 个文件 / 425 条**降下来（另有 video_refs 的 2 条原始
字符串 SQL 直到第三十一批才被门禁发现，含它的真实起点是 427 条），
users、sessions、security 三个文件已经是 **0**；第二批把 account 的 **67 条**
清零，第三批把 groups / communities 各 **29 条**清零，第四批把 wallet 的
**28 条**清零，第五批把 articles 的 **27 条**清零，第六批把 moments 的 **21 条**
清零，第七批把 friends 的 **19 条**清零，第八批把 video_feed 的 **17 条**清零，
第九批把 groups_admin 的 **17 条**清零，第十批把 live 的 **15 条**清零，
第十一批把 status 的 **14 条**清零，第十二批把 favorites 的 **14 条**清零，
第十三批把 relations 的 **13 条**清零，第十四批把 stickers 的 **12 条**清零，
第十五批把 recovery 的 **9 条**清零，第十六批把 reconcile 的 **8 条**清零，
第十七批把 notes 的 **8 条**清零，第十八批把 keys 的 **6 条**清零，
第十九批把 perms 的 **5 条**清零，第二十批把 content_index 的 **5 条**清零，
第二十一批把 report 的 **5 条**清零，第二十二批把 secret 的 **5 条**清零，
第二十三批把 trust 的 **5 条**清零，第二十四批把 inbox 的 **4 条**清零，
第二十五批把 media 的 **4 条**清零，第二十六批把 devices 的 **3 条**清零，
第二十七批把 governance 的 **3 条**清零，第二十八批把 notification 的 **3 条**
清零，第二十九批把 qr_login 的 **3 条**清零，第三十批把 read 的 **2 条**清零，
第三十一批补清 video_refs 的 **2 条**，总数从 427 降到 0。

## 收尾与边界

`domain/*.rs` 与 crate 根 handler 的生产 SQL 已清零，handler 也不再直连
`db().lock()`，且 `TODO(ADR-28)` 已全部收口：SQL 与直连锁的清理没有下一批。
ADR-28 的阻塞池覆盖仍按模块推进，已强制 `friends / groups / groups_admin /
inbox / devices / keys / account` 与 `crates/admin/src`，后续只守边界：

1. 新 SQL 只进 `domain/repo/*.rs`；
2. 基线保持 0/0，门禁即零容忍；
3. 想接 PostgreSQL 时改 repo 层实现，不改 domain。

每批的验收标准一致：

- 该模块生产代码 SQL 计数为 0；
- `Platform` 公开签名不变；
- SQL 只留在 `domain/repo`，domain 不再依赖 rusqlite；
- `check_domain_sql.py` 保持 0/0，新增 SQL 即失败；
- CI 的 `pg-migration` / `test` / `audit` 全绿。

## 不要做的事

- 不要把新 SQL 写回 `domain/*.rs`，即使只读一条也不行；
- 不要在 handler 层直接 `db().lock()` 拼 SQL；
- 不要为了让旧测试过而复制一份 SQLite 语句；
- 不要先改 51 个旧签名；它们是对外适配面，迁移顺序应从实现内部开始。
