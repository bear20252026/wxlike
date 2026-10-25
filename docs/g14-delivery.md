# G14 交付说明：管理后台与运营闭环

分支 `feat/FP-G14`（base `f3c1a2c`）｜组跟踪 #95｜任务 #176 #178 #180 #182 #185

本文件是 G14 五张卡的交付底稿：做了什么、怎么验证的、哪些还没结项。**只改
`crates/admin/**`**；凡超出 G14 独占写路径的东西都登记在
`integration-requests/G14/G14-0X.yaml`，由 G00 执行。

---

## 1. 结论速览

| 卡 | 范围 | 代码 | 用例 | 未结项（阻塞 ACCEPTED） |
|---|---|---|---|---|
| G14-01 | 管理身份、RBAC、审计骨架 | `auth.rs` `audit.rs` `store.rs` `migrations/0001` | `tests/g14_01_auth_audit.rs`（8）+ 单测 17 | G00-02 网关层 audience；G00-03 迁移编号 |
| G14-02 | 用户/设备/群/申诉 | `identity.rs` | `tests/g14_02_identity.rs`（7）+ 单测 10 | G02-05 四个身份管理端点（真实域） |
| G14-03 | 内容/小程序/服务审核 | `publishing.rs` | `tests/g14_03_publishing.rs`（7）+ 单测 9 | G05-05 / G07-07 / G08-04 端点 |
| G14-04 | 商户/卡券/资金/对账 | `commerce.rs` | `tests/g14_04_commerce.rs`（7）+ 单测 11 | G09 退款出账与对账口径 |
| G14-05 | 作业/隐私/可观测 | `jobs.rs` | `tests/g14_05_jobs.rs`（5）+ 单测 14 | 域侧作业回报通道 |

**为什么还不能标 ACCEPTED**：五张卡的**域侧效果**（真的封号、真的下架、真的出账）
在 G02/G05/G07/G08/G09 的端点里，目前用任务单允许的 fixture 端口并行开发。
业务规则、权限、审计、幂等、脱敏这些**属于 G14 的部分全部是真实实现**并有断言；
从 fixture 换成真实域只需要改 `App::open` 的装配点（`G14::with_http`）与用例里的注入。

---

## 2. 架构与三条边界

```
运维浏览器 ──HTTP──> adminx (:8090) ──┬── 平台库 SQLite（只读）
                                     ├── adminx.db（账号 / 会话 / g14_* 运营域表）
                                     └── 命令转发 x-admin-token ──> wxlike-server (:8099)
```

1. **不写平台库**。G14 只在自己的 `adminx.db` 里建 `g14_*` 表（`0001` 迁移，
   `PRAGMA user_version = 1`），平台库仍以只读方式打开。所有会改线上状态的动作
   走域侧端口（`identity::PlatformPort` / `publishing::ContentPort` /
   `commerce::MoneyPort`），由平台进程执行——绕过它改库会留下"后台改了、服务没改"。
2. **不复制别组规则**。谁能做什么在 `auth` 的矩阵里；业务状态机（工单、审核项、
   发行、作业）在本组；资金与内容真值在域侧。跨组需要的东西用契约 fixture 交接。
3. **写操作可追责**。静态运营令牌只能读；写必须后台会话；敏感动作还要当场验过
   动态码（step-up，15 分钟窗口）。`g14_audit` 与 `g14_journal` 由 SQLite 触发器
   拒绝 `UPDATE/DELETE`。

模块与路由：

| 模块 | 职责 | 路由前缀 |
|---|---|---|
| `auth.rs` | audience、资源×动作矩阵、step-up 窗口 | `/api/g14/auth/matrix`、`/api/g14/session/stepup` |
| `audit.rs` | append-only 审计 + 哈希链校验 + 脱敏 | `/api/g14/audit`、`/api/g14/audit/verify` |
| `identity.rs` | 工单状态机、两阶段危险动作、元数据投影 | `/api/g14/identity/*` |
| `publishing.rs` | 审核队列、规则版本、签包/发布/撤回、服务卡片 | `/api/g14/publishing/*` |
| `commerce.rs` | 商户隔离、退款双人、分录、对账差异、导出 | `/api/g14/commerce/*` |
| `jobs.rs` | 作业重试/取消、批量回执、隐私请求、健康面、限流 | `/api/g14/jobs`、`/privacy/*`、`/health/summary` |

---

## 3. 每卡实现要点与验收证据

### G14-01 管理身份、RBAC 与审计骨架

- **独立 audience**：`auth::Audience` 分 `operator`（静态令牌，只读）/ `admin_session`
  / `end_user`（`wxu_*`，一律 403 并说明走错门）/ `unknown`（401）。
- **资源级权限**：`Resource`（16 类）× `Action`（read/write/destructive/export_pii/
  sign_package/approve_money），角色 `viewer/moderator/publisher/finance/superadmin`。
  新岗位在老 `rbac::OPS` 目录里**只有读**，防止"给财务 Write 顺手给了工单处置权"。
- **step-up**：`Destructive / ExportPii / SignPackage / ApproveMoney` 四类动作除角色外
  还要 15 分钟内的验码窗口；过期消息带"多久前验的、怎么续"。
- **审计**：每行 `actor / role / action / object / reason / before_digest / after_digest /
  trace / outcome / detail(脱敏) / prev_hash / hash`；`verify()` 从头重算，改一行即断链。
- 证据：`crates/admin/tests/g14_01_auth_audit.rs`
  （audience、静态令牌只读、只读岗越权留痕、step-up 三段、前后摘要与脱敏、
  链校验与过滤、参数错误 422、上游超时 502 + failed 审计、健康面不泄漏）。

### G14-02 用户、设备、群与申诉管理

- 域侧动作经 `PlatformPort`：`suspend/unsuspend/revoke_device/govern_group`，
  回执 `sessions_revoked` 就是"封禁即时撤销会话"的验收数值。
- 危险动作两阶段：`dry_run` 出预览（一次性令牌，5 分钟，指纹绑对象+动作+理由+时长）→
  带 `confirm` 执行；重放/换对象/换理由/过期各有明确拒绝理由。
- 工单状态机 `open → triaged → actioned|rejected → closed`，禁跳步，每步必带理由。
- E2EE 边界：`project()` 白名单投影，只出元数据，返回 `dropped_fields` 计数。
- 证据：`tests/g14_02_identity.rs`（封禁踢会话 + 解封留痕、令牌单次、工单全流程、
  无明文、设备/群动词送达、非法动词前置拒绝、批量逐条回执 + 逐条审计）。

### G14-03 内容、小程序与服务发布审核

- 审核（moderator）与签包（publisher）**两个动作**：`sign_package` 只有 publisher 有，
  且要 step-up；发布要求已签名；撤回按 Destructive 归 superadmin。
- 下架/恢复先打域侧再改库：域侧失败则库里状态不变（避免"库里下架、线上还在"）。
- 幂等：提交按 `idem`；发布按状态机，失败停在 `signed`，重试不产生新版本。
- 配置只存引用：`key_ref` 必须以 `vault://` 开头，明文密钥在入口 422；回执只回
  `secret_configured` 布尔；每次成功写入 `version + 1`。
- 证据：`tests/g14_03_publishing.rs`（空队列 + 规则版本、idem、下架/恢复驱动域侧、
  权限分离、发布失败可重试、明文密钥拒绝 + 版本递增、只读岗不能判定）。

### G14-04 商户、卡券、资金与对账台

- **没有余额列**：`no_balance_column_exists_anywhere` 用 `PRAGMA table_info` 断言
  `g14_merchant/order/card/journal` 都没有 `balance`；人工补账只能写借贷分录且必须平。
- 退款：同 `idem` 一张单；≥ ¥5000 要两个**不同**审批人（同一人第二次被拒）；
  已批准再批准幂等、不重复打款。
- 商户隔离：`MerchantScope::Only` 在 SQL 层过滤，越权单查返回 `None`（不给 403 免探测）。
  HTTP 面 `?as_merchant=` 仅 superadmin 可用（商户自助账号的绑定见 REQ-G0X）。
- 导出：要求 `ExportPii` + step-up；CSV 对 `= + - @` 开头单元格加 `'`（公式注入）。
- 证据：`tests/g14_04_commerce.rs` 8 例（隔离、双人审批、小额单人、分录平衡与重放、
  差异收敛、导出防护、idem 与额度上限）。

### G14-05 运营作业、隐私请求与可观测入口

- 作业：`queued/running/succeeded/failed/cancelled`；只有 failed/cancelled 可重试，
  重试**恢复原作业**（同一行，`attempts+1`），连点幂等；取消是终态。
- 批量处置：逐条命令、逐条审计、逐条回执；取消标记在每项之前读库，所以能落在中间，
  回执 `processed + skipped == total`。
- 隐私请求：令牌只回一次，库中只存 sha256；24h 过期；错令牌 403；无令牌 422；
  `fulfill` 用受影响行数判定，重复交付被拒。
- 可观测：只给计数 + 审计链状态 + uptime，`withheld` 字段明写"配置/连接串/密钥/令牌/内容不可见"。
- 限流：令牌桶（60 容量 / 2 QPS per actor），429 带"约 N 秒后可重试"。
- 证据：`tests/g14_05_jobs.rs` 5 例 + `jobs` 单测 14 条（含中途取消、逐项失败不吞、限流）。

---

## 4. 迁移与回滚

- 迁移：`crates/admin/migrations/0001_g14_ops.sql`（内嵌进二进制，`include_str!`）。
  只 `CREATE TABLE/INDEX/TRIGGER`，**不 `ALTER` 任何既有表**，与 D1/D2/D3 的
  `admin_accounts / admin_sessions / admin_oplog` 完全正交。
- 版本闸门：先读 `PRAGMA user_version` 再决定迁移；**更大的版本直接拒启动**
  （拒绝在更新结构上按旧代码写数据），单测 `newer_schema_is_refused_instead_of_guessed`。
- 回滚：`0001_g14_ops.down.sql` 先删触发器再删表，最后 `user_version = 0`；
  单测 `rollback_removes_only_its_own_tables` 验证"只删自己、既有表还在、可再次升级"。
- 生产回滚步骤（写进 PR 说明）：① 停 adminx 写（单写者，WAL 下换 schema 会与在途事务冲突）
  ② `sqlite3 adminx.db "VACUUM INTO 'adminx-<ts>.bak'"` 归档（含审计链，删表即销毁证据）
  ③ 跑 down 脚本 ④ 回退二进制。

---

## 5. 最小权限与日志脱敏证据

| 面 | 做法 | 用例 |
|---|---|---|
| 审计脱敏 | `message/content/body/plaintext/ciphertext/secret/token/password/phone/...` 整值 → `[redacted]`；`*_ref/_digest/_id/_at` 等引用类不脱敏 | `audit::tests::sensitive_values_never_reach_storage`、`digests_are_stable_and_ignore_injected_plaintext` |
| 用户视图 | 白名单投影，只出元数据 + `dropped_fields` | `identity::tests::projection_drops_every_content_field`、`g14_02::admin_never_sees_message_plaintext` |
| 密钥 | 只存 `vault://` 引用，明文入口 422；回执只回布尔 | `publishing::tests::service_config_never_accepts_or_returns_plaintext_secrets` |
| 健康面 | 只有计数 / 链状态 / uptime，`withheld` 明写不可见项 | `jobs::tests::health_summary_exposes_counts_and_never_secrets`、`g14_01::health_summary_never_leaks_secrets` |
| 隐私令牌 | 库中只存哈希，明文只回一次，24h 过期 | `g14_05::privacy_export_token_is_the_only_way_to_fetch` |
| 付款资料 | `mask_payout` 只留后四位 | `commerce::tests::payout_details_are_masked` |

---

## 6. 怎么跑

```bash
# 单元 + 集成（G14 全部验收面）
cargo test -p wxlike-admin --lib
cargo test -p wxlike-admin --test g14_01_auth_audit
cargo test -p wxlike-admin --test g14_02_identity
cargo test -p wxlike-admin --test g14_03_publishing
cargo test -p wxlike-admin --test g14_04_commerce
cargo test -p wxlike-admin --test g14_05_jobs
# 既有 D1/D2/D3 不能被改坏
cargo test -p wxlike-admin --test d1_read_only --test d2_audit_reports --test d3_rbac_mfa
```

CLI 侧不需要新的环境变量：`WXLIKE_ADMIN_TOKEN` / `WXLIKE_ADMIN_DB` / `WXLIKE_PLATFORM_URL`
沿用 D1/D2/D3 的那一套。新增角色可用
`wxlike-admin account add --name fin --role finance`。

---

## 7. 未结项清单（ACCEPTED 前必须清）

1. **真实域端点**：`integration-requests/G14/G14-02.yaml` 的 REQ-G02-05 四个身份端点、
   `G14-03.yaml` 的 G05/G07/G08 端点、`G14-04.yaml` 的 G09 出账端点。
   合入后把 `App::open` 用 `G14::with_http`（已是生产路径）跑一遍真实平台，
   并把对应用例的 fixture 注入换成 `common::serve_platform`。
2. **G00-02 网关 audience**：adminx 端口不应直接对公网；网关层要拒用户令牌。
3. **G00-03 迁移编号**：给 `0001_g14_ops` 登记串行编号。
4. **G00-03 契约命名**：本卡引入的 action 词表（`admin.login` / `operator.role` /
   `audit.query` / `admin.user.suspend` / `device.revoke` / `review.case` /
   `admin.content.review` / `mini.publish|revoke` / `service.configure` /
   `admin.merchant.manage` / `reconcile.review` / `refund.approve` /
   `admin.job.list|retry|cancel` / `privacy.export|delete` / `health.summary`）
   需在契约冻结时批准或改名。**注意**：G14 是后台，不属于 331 条 UI 入口账，
   因此没有 entry_id；跨组引用用 action 词表 + 任务号。
5. **apps/admin 前端**：本 PR 未提交前端页面。原因不是省事：任务单自己写着
   "目标路径须经 G00 纯拆分后创建"，而 `apps/` 下目前只有 `mobile/`，`apps/admin`
   还不存在——在别人的目录里先落地会破坏 G00 的路径裁决。当前 `/api/g14/*` 的响应
   形状就是前端契约（§2 的模块表 + 用例里的实际断言），目录一经 G00 划出即可按
   react-admin（MIT）壳接上，不需要改后端。
6. **撤回权限归属**：小程序撤回目前按 Destructive 归 superadmin。若业务要求发布岗
   自行撤回，需要改矩阵 + 补用例（写在这里而不是偷偷放宽）。

---

## 8. PR 交接清单

- 改动路径：`crates/admin/src/{auth,audit,store,identity,publishing,commerce,jobs,g14}.rs`、
  `crates/admin/src/{lib,http,error,rbac,accounts}.rs`（仅接线与新增角色/方法）、
  `crates/admin/migrations/0001_g14_ops.{sql,down.sql}`、
  `crates/admin/tests/g14_common/mod.rs`、`crates/admin/tests/g14_0{1..5}_*.rs`、
  `integration-requests/G14/G14-0{1..5}.yaml`、本文件。
- 契约版本：`admin-g14-v1`。
- 测试命令与结果（本地实跑）：
  `--lib` **116 passed**（G14 新增单测 61 条）；
  `g14_01` 8 / `g14_02` 7 / `g14_03` 7 / `g14_04` 7 / `g14_05` 5 = **34 条集成用例全绿**；
  既有 `d1_read_only` 11 / `d2_audit_reports` 8 / `d3_rbac_mfa` 14 全绿（无回归）；
  `cargo fmt -p wxlike-admin --check` 干净，编译零 warning。
- 已知代价：`sha2`/`hex`/`async-trait` 加入 `crates/admin` 依赖，但都是
  workspace 已有条目，未新增第三方代码版本。

## 2026-10-25 合并整改

在 main `936b411fb3fdb72133fbdb28749991c1f0ef0d5f` 上完成 G14 已有代码的合并整改。业务入口、授权/步进验证先后、退款双人审批、审计与写锁边界、批量逐项取消和回执语义保持原实现；不增加产品功能、不改质量基线。

- 拆分审计构造/筛选/验链、退款校验与落库、分录校验与落库、处置结果留痕、批量作业阶段和隐私请求存储。
- 运营各列表共用分页计数与绑定参数处理；新旧管理入口共用同一个令牌比较实现，上游回执引用只保留一份解析。
- 命名限流默认值，以列名读取稳定 SQL 字段；SQL 与说明字符串整理为完整字面量。
- 五项门禁相对原基线：>60 行函数 41/41、>100 行 10/10、深嵌套 63/63、重复窗口 63/66、魔法数字 162/162。质量探针、门禁脚本与基线均未改动。
- 保留全部既有测试，补一条跨商户过滤+分页偏移回归：总数只计当前商户且先于 offset，越权商户过滤结果与总数均为空。
- Clippy `-D warnings`、格式、SBOM 与当前授权矩阵检查通过。最终云端结果以 #271 当前 head 检查为准。

本节是代码合并证据，不是 UI 全入口、真实供应商、生产接线或多端真机验收；原跨组集成请求仍按实际结果验收。
