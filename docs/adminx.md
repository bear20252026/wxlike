# adminx — 运营后台（Zone D：D1 只读后台 / D2 审计检索与工单 / D3 RBAC + MFA）

> 2026-10-20 · 对应 `docs/task-assignment.md` §5 Zone D 与 `docs/parallel-tracks.md` 轨道 A
> 代码：`crates/admin`（新 crate，独立进程 `wxlike-admin`）· 蓝图位置：ADR-15 / P6

---

## 1. 它是什么，一句话

一个**独立进程**的运营面板：对平台库**只读**，看数据、检索审计、处置举报工单，
写操作一律转发给正在运行的 `wxlike-server`。

```text
运维浏览器 ──HTTP──> wxlike-admin (:8090)
                        │
                        ├── SQLite(平台库) ──(READ_ONLY + query_only)── 只读
                        │
                        └──写操作──x-admin-token──> wxlike-server (:8099) /api/admin/*
```

三条不可动摇的边界，每条都在测试里被反向证明过：

| 边界 | 谁守着 | 怎么验的 |
|---|---|---|
| adminx 不写平台库 | `SQLITE_OPEN_READ_ONLY` + `PRAGMA query_only=ON` | `read.rs` 的 `strict_read_only_path_blocks_writes` / `fallback_path_also_blocks_writes`；`d1_read_only.rs::a_full_read_battery_leaves_the_platform_db_bytes_untouched` 直接比对库文件字节 |
| 业务规则不在后台 | 工单状态机用 `wxlike-report::Ticket`，审核判定用平台的端点 | `read.rs::reports_read_through_the_real_state_machine_types`、`corrupt_state_string_is_an_error_not_a_default` |
| 没有 MFA 就不能写 | `rbac::authorize` 一处生效 | `d3_rbac_mfa.rs::no_mfa_means_no_write_for_every_role`（三个角色 × 目录每条操作都试，并证明平台侧没落下任何记录） |

---

## 2. 为什么不直接写平台库（这是本卡唯一需要论证的架构决定）

第一反应是"adminx 打开同一个 SQLite，改 `reports` 表就行，省事"。**不行**，理由不在
分层洁癖，而在**内存态**：

* 审核模式（先审后发 / 先发后审）活在平台的 `Mutex<Pipeline>` 里，**库里没有这一列**；
* 风控窗口（`wxlike-risk` 的滑动计数）、群成员缓存（`group_cache`）、通话房间
  （`CallRegistry`）同样是进程内的。

绕过服务直接改库的后果是："运营在后台改了，服务其实没改"——本项目 §3.7 明令禁止的
静默降级，而且它长得特别像成功。

这条不是口头承诺，有锚：`d3_rbac_mfa.rs::mode_switch_reaches_the_running_process_not_just_a_file`
——从活着的平台问出 `mode=pre_review`，经 adminx 切换，再问一次，必须变成
`post_review`。如果哪天有人把转发改成直接写库，这条一定红。

顺带的收益：所有处置都带上 `x-admin-actor: <后台账号名>`，平台的审计日志因此能落到
**具体的人**，而不是一个匿名的 `admin`。

---

## 3. 进程、配置与部署

```bash
# 1) 建第一个管理员（写操作要账号；读面只要令牌）
WXLIKE_ADMIN_TOKEN=<token> WXLIKE_ADMIN_NEW_PASSWORD=<口令> \
  wxlike-admin account add --name ops --role superadmin
# 2) 绑码、激活（否则这个账号只能读）
wxlike-admin account totp-init --name ops            # 打印密钥与 otpauth URI
wxlike-admin account totp-confirm --name ops --code 123456
# 3) 起服务，浏览器打开 http://127.0.0.1:8090
WXLIKE_ADMIN_TOKEN=<token> wxlike-admin
```

| 环境变量 | 必填 | 默认 | 说明 |
|---|---|---|---|
| `WXLIKE_ADMIN_TOKEN` | **是** | — | 读面令牌。缺失或全空白 → **拒绝启动**（`d1_read_only.rs::binary_refuses_to_start_without_the_admin_token` 用真子进程验退出码） |
| `WXLIKE_DB` | 否 | `<数据目录>/wxlike/wxlike.db` | 平台库路径。`":memory:"` 被**拒绝**：独立进程读内存库只会读到空表 |
| `WXLIKE_ADMIN_DB` | 否 | `<数据目录>/wxlike/adminx.db` | adminx 自己的账号/会话/留痕库 |
| `WXLIKE_ADMIN_BIND` | 否 | `127.0.0.1:8090` | 监听地址。默认只绑回环；要对外必须显式改，并且**先上反代 + TLS** |
| `WXLIKE_PLATFORM_URL` | 否 | `http://127.0.0.1:8099` | 写操作转发目标 |
| `WXLIKE_ADMIN_PLATFORM_TOKEN` | 否 | 复用 `WXLIKE_ADMIN_TOKEN` | 平台 `x-admin-token`。轮换后台登录令牌时不该被迫同时轮换平台令牌 |
| `WXLIKE_ADMIN_REPORT_SLA_MS` | 否 | 86 400 000 | 工单超时口径（`overdue` 用它算） |
| `WXLIKE_ADMIN_SESSION_TTL_MS` | 否 | 28 800 000（8 h） | 后台会话时长 |
| `WXLIKE_ADMIN_SHUTDOWN_GRACE` | 否 | 10 | 关闭排空上限（秒） |
| `WXLIKE_ADMIN_LOG` / `RUST_LOG` | 否 | `info` | 日志过滤 |

`wxlike-admin --check` 打印解析后的配置（**不打印令牌**）并验三件事：平台库能只读打开、
必需表齐不齐、账号库可写。不绑端口、不写业务库，适合放进部署流水线。

### 3.1 部署前置：平台侧还差一行（给集成方，不在本卡所有权内）

`server/src/lib.rs` 组装 `PlatformOptions` 时**没有**设 `admin_token`，所以真实部署里
`/api/admin/*` 一律 403（`admin_ok()` 在未配置时恒 false）。adminx 的写面因此现在还
走不通——2026-10-20 的端到端冒烟把这条照得很清楚：MFA 会话齐备、请求发出去了，
平台回 `403 admin endpoint unavailable`，adminx 如实报 502 并留痕 `upstream_error`。

需要的是 `server/src/lib.rs`（热点文件，§6.1）里一处三行：

```rust
let mut platform_options = wxlike_platform::PlatformOptions::default();
// 管理面令牌：未配置 = 管理端点关闭（平台侧已是这个语义，这里只是把它接到环境）。
platform_options.admin_token = std::env::var("WXLIKE_ADMIN_TOKEN")
    .ok()
    .map(|v| v.trim().to_string())
    .filter(|v| !v.is_empty());
```

变量名与 adminx 的读面令牌**故意同名**：一套部署里由运维决定要不要拆成两个
（adminx 侧支持 `WXLIKE_ADMIN_PLATFORM_TOKEN` 覆盖）。合并这行之后，本卡 §13 的第 4 项
才算真的可验。


**启动即失败的两类情况**（都刻意为之，宁可起不来也不要起一个空看板）：

1. 缺 `WXLIKE_ADMIN_TOKEN`；
2. 平台库缺必需表 → 报错并点名缺哪张。adminx **不建表**（见 §9）。

前台运行 + 接 SIGTERM/SIGINT，与 `wxlike-server` 同一套 supervisor 约定
（`deploy/` 的 launchd/systemd 模板可以照抄一份，只是服务名与变量不同）。

---

## 4. 端点清单

`/health` 与 `/` 无鉴权（存活探针 + 静态壳，页面本身不含任何数据）。其余全部分三层。

### 4.1 读面（静态令牌 **或** 会话令牌二选一）

| 方法 路径 | 说明 |
|---|---|
| `GET /health` | `ok / version / uptime_ms / db_open_mode / missing_tables / accounts_present / writes_platform_db:false` |
| `GET /api/stats` | 看板。每个数字的代价见 §9 |
| `GET /api/users?q=&limit=&offset=` | 名字前缀搜索（`%` `_` 已按 LIKE 元字符转义）；**不含**口令哈希与 TOTP 密钥 |
| `GET /api/groups?limit=&offset=` | 群 + 成员数 + 审批/全员禁言开关 |
| `GET /api/audit?actor=&command=&action=&target=&from=&to=&limit=&offset=` | **D2 的检索面**，见 §4.4 |
| `GET /api/reports?state=&handler=&target_id=&reporter=&open_only=&limit=&offset=` | 工单队列；默认只给未终结的 |
| `GET /api/reports/{id}` | 单张工单（含完整流转历史） |
| `GET /api/moderation?limit=&offset=` | 先审后发挂起队列（只读；处置走 §4.3） |
| `GET /api/oplog?actor=&op=&status=&from=&limit=&offset=` | adminx 自己的操作留痕 |
| `GET /api/ops` | 写操作目录（id / 能力 / 上游路径 / 载荷 / 说明） |

### 4.2 会话面（登录令牌）

| 方法 路径 | 说明 |
|---|---|
| `POST /api/login` | `{username, password, totp?}` → `{token, session{role, mfa, ...}}`。账号不存在与口令错**同一个 401**，防枚举 |
| `POST /api/logout` | 吊销当前会话 |
| `GET /api/me` | 当前会话 + 账号概览（只说"有没有绑过码"，不再吐密钥） |
| `POST /api/me/totp/enroll` | 生成密钥（只在这一次响应里出现） |
| `POST /api/me/totp/confirm` | 首码激活 |
| `POST /api/me/totp/disable` | 关闭 MFA，**必须带当前码** |

### 4.3 写面（`POST /api/op/{op}`，op id 见目录）

请求体：`{"target": "<对象 id>", "note": "依据", "action": <处置>, "mode": <模式>}`。
响应：`{op, capability, target, actor, result(平台原样回显), logged}`。

| op id | 能力 | 需要 target | 上游 |
|---|---|---|---|
| `report.triage` | write | 是 | `POST /api/admin/reports/{id}/triage` |
| `report.close` | write | 是 | `POST /api/admin/reports/{id}/close` |
| `report.reject` | write | 是 | `POST /api/admin/reports/{id}/reject` |
| `report.act` | **destructive** | 是 | `POST /api/admin/reports/{id}/act` |
| `moderation.release` | write | 是 | `POST /api/admin/moderation/{id}/release` |
| `moderation.reject` | **destructive** | 是 | `POST /api/admin/moderation/{id}/reject` |
| `moderation.mode` | **destructive** | 否 | `PUT /api/admin/moderation/mode` |
| `wallet.reconcile` | write | 否 | `POST /api/admin/reconcile` |

转发失败一律 502，并**保留平台的结论**：409（非法流转）、404（对象不存在）、
401/403（平台令牌不对，消息里直接点名 `WXLIKE_ADMIN_PLATFORM_TOKEN`）。

### 4.4 审计检索与"agent 写过什么"

平台的命令审计中间件（`crates/platform/src/audit_mw.rs`）落的形状是：

```text
action = "cmd.<命令 id>"   target = 请求路径   detail = "status=200 method=POST"
```

所以 `GET /api/audit?command=group.create` 就是"agent 执行过哪些 `group.create`"。
测试不是自己往库里手搓漂亮数据，而是**真的带 `X-Wxlike-Command` 打平台**再从后台查回来
（`d2_audit_reports.rs::command_id_finds_what_the_agent_actually_wrote`），并顺带证明：
读命令（`group.list`）与写命令分得开、adminx 能读到"打开之后才提交"的行。

筛选值只进 SQL 绑定，不进拼接：`WHERE` 片段是枚举出来的固定形状，值一律 `?n`。
`command=group.create&actor=<uid>` 这类组合有专门用例。区间反了（`from > to`）返回 422
并说清"时间区间是反的"，**不**返回空列表——后者会让人误判成"没人干过这事"。

---

## 5. RBAC

能力三级 `read < write < destructive`，角色累积授权，**默认拒绝**：

| 角色 | read | write | destructive |
|---|---|---|---|
| `viewer` | ✅ | ❌ | ❌ |
| `moderator` | ✅ | ✅ | ❌ |
| `superadmin` | ✅ | ✅ | ✅ |

形状借用本仓库 `crates/cmd::Policy`（同语言同问题）：目录驱动、先报"操作不存在"再报
"没权限"（避免用错误消息探测目录）、**拒绝要可读**——消息里给出该用哪个角色建号
（`--role superadmin`，不是 `--role destructive`，后者是个建不出号的胡话，测试
`role_denial_points_at_the_role_that_actually_grants_it` 就是拦这个的）。

静态令牌**不能**写：它不对应任何一个"人"，写不出可归因的留痕。

---

## 6. MFA（D3 的硬门）

* 写操作要求会话 `mfa=true`，而 `mfa` 只在**登录时验过 6 位动态码**才为真；
* 没绑码的账号可以登录、可以读，**任何**写操作一律 403；
* 自助绑码不需要先有 MFA（否则死锁）：`enroll → confirm(当前码)`；
* **关闭** MFA 必须带当前码，否则"手机丢了 → 用未验码的会话关掉 MFA"是现成的降级路径；
* 改口令 / 停用账号 → 立即吊销该账号全部会话（与平台的"重置密码即下线"同要求）。

凭据原语全部复用平台公开函数：`wxlike_platform::{hash_argon2id, verify_argon2id,
totp_generate_secret, totp_verify, random_hex}`。**不在 adminx 重写一遍**——两套参数、
两套容差只在出事时才会暴露。

账号名只收 ASCII `[A-Za-z0-9-_.@]`：它会被写进 `x-admin-actor` 头发给平台，
头值里出现换行就是请求走私，出现不可见字符就是审计造假。有专门用例打这个形状。

---

## 7. 只读是怎么保证的（两层，不是一句态度）

```rust
Connection::open_with_flags(path, SQLITE_OPEN_READ_ONLY | ...)  // 第一层
conn.pragma_update(None, "query_only", "ON")?;                   // 第二层
```

外加一条**回退**：以只读方式打开一个 WAL 库时，若 `-shm` 恰好不存在（服务刚好干净
退出过），SQLite 会拒绝打开。此时改用读写标志打开，但 `query_only=ON` 立刻生效——
写仍然被数据库拒绝，可读性却不再依赖 `-shm` 出现的时机。

两条路径的"写必失败"各有一个用例（`strict_read_only_path_blocks_writes` /
`fallback_path_also_blocks_writes`），`/health.db_open_mode` 把当前走的是哪条透出去。
文件不存在时**不创建**空库（`nonexistent_db_file_does_not_create_one`）。

---

## 8. 留痕（`admin_oplog`，adminx 自己的库）

每一次"试图写"都落一行，**包括被拒的**：

| 列 | 含义 |
|---|---|
| `at_ms / account_id / actor / op / capability / target` | 谁、什么时候、对什么做了哪条操作 |
| `upstream` | 真正发给平台的方法 + 路径（没发出去的行这里是空） |
| `status` | `ok` / `denied`（角色不足、没 MFA、参数非法）/ `upstream_error`（平台 4xx 5xx）/ `transport_error`（没连上平台） |
| `mfa` | 这次会话验过码没有 |
| `detail` | 平台原样回显（超 2048 字符带 `…(截断)` 标记）或拒绝原因 |

留痕写不进去时，接口响应里 `logged:false` + `tracing::error!`——留痕失败等于操作失去
追责链，不该悄悄过去。

---

## 9. 看板每个数字的代价（性能口径，别把它当监控用）

| 数字 | 代价 | 备注 |
|---|---|---|
| `users_total / groups_total / communities_total / moderation_held` | 全表 `COUNT` | 这几张表规模与运营体量同阶，可接受 |
| `conversations / message_seqs` | 走 `owner_seq`（每会话一行） | **故意不数 `mailbox` 行数**：那是一次与消息总量同阶的全表扫。要精确消息数请走离线统计 |
| `audit_in_sample_24h` | `ORDER BY id DESC LIMIT 1000` 的窗口计数 | 审计表无界，绝不全表扫。窗口大小由 `audit_sample_rows` 自报 |
| `reports_open / reports_over_24h` | 走 `idx_reports_state(state, created_at)` | 有索引 |
| `users?q=` 前缀搜索 | 走 `users.name` 的 UNIQUE 索引 | 中缀 `%x%` 不走索引，所以只做前缀 |
| `/api/audit` 按 `at_ms` 排序 | **无索引**（只有 `idx_audit_actor(actor, at_ms)`） | 见下面的待办 |

**留给串行轨道的两个索引**（adminx 无权加，加索引属于改平台 schema）：

1. `CREATE INDEX idx_audit_at ON audit_log(at_ms)` — 不带 `actor` 的时间窗检索会全表扫；
2. `CREATE INDEX idx_audit_action ON audit_log(action, at_ms)` — `command=` / `action=` 检索。

不阻塞本卡：小体量下全表扫仍然毫秒级，但**上线到百万级审计行之前必须补**。
`crates/platform/src/db.rs` 的 `migrate` 里加两行即可，由集成方处理。

---

## 10. 借用结论（§3.5 要求的三行）

| 面向 | 结论 | 出处与许可 |
|---|---|---|
| 后台信息架构（看板 / 用户 / 群 / 审计 / 工单的分区与处置动作形状） | **参照重写**（只借分层，不逐行翻译） | Mattermost admin console（Tier 0，AGPL-3.0；本机未镜像，未复制任何代码） |
| 权限模型（目录 + 能力分级 + 默认拒绝 + 可读的拒绝） | **照搬形状、同仓库自实现** | 本仓库 `crates/cmd/src/policy.rs`（AGPL-3.0-only，自家代码，无需 vendoring 登记） |
| 工单状态机 / argon2id / TOTP / 只读 SQLite 的形状 | **复用依赖**，不重写 | `wxlike-report`、`wxlike-platform` 的公开函数；`rusqlite`、`axum`、`ureq`、`totp-rs` 都已在 `Cargo.lock` 里 |

没有复制任何外部代码进仓库，因此 `THIRD-PARTY-NOTICES.md` 的 "Vendored code" 表**无需新增条目**。
`ureq` / `totp-rs` 是**已在依赖树里**的 crate（分别由 `crates/client` 与 `crates/platform` 使用），
`crates/admin` 只是多了一条边；`Cargo.lock` 合并时以 main 为准重新生成即可。

---

## 11. 没做（下一张卡的输入，不是"忘了"）

| 项 | 为什么现在不做 |
|---|---|
| **双人复核（maker-checker）** | ADR-15 把它与 MFA 并列为敏感操作的加固。它需要一张待复核队列 + 第二个确认人，是一张独立卡。`Destructive` 已经单独分级，接法留好了 |
| 数据导出 / GDPR 主体请求 | 单页面板的 `limit ≤ 200` 是刻意的；导出是批处理，不该塞进 HTTP 读面 |
| 登录限流与账号锁定 | 默认只绑 127.0.0.1；公网部署前必须补（或交给反代）。目前只有 argon2id 的高开销被动兜着 |
| 封禁 / 提现审核 / 密码重置等**账号级**处置 | 平台侧还没有对应端点（`users` 表没有封禁列）。adminx 不该造一个只有自己看得见的封禁位 |
| adminx 自己的 `/metrics` | 与 `wxlike-server` 的 Prometheus 面是同一件事，留给 F 区 |
| 前端框架 / 构建产物 | 内联单页 HTML，零构建步骤。真要拆，得先解决"页面在哪个目录"这类静默失败 |
| 会话绑定 IP / UA、并发会话上限 | 已记录 `ip` 字段，但还没做校验 |
| HTTPS | 明确交给反代（`deploy/` 的 TLS 模板是 P6 另一张卡） |

---

## 12. 验收命令

```bash
# 六关
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace

# 本卡单独跑
cargo test -p wxlike-admin              # 单元 + 三个集成套
cargo clippy -p wxlike-admin --all-targets -- -D warnings
```

**卡的验收栏 → 具体用例**：

| 验收项 | 用例 |
|---|---|
| D1：`/health` `/api/stats` `/api/users` `/api/groups` `/api/audit` + 单页 HTML | `d1_read_only.rs::health_is_public_and_declares_the_read_only_boundary` / `dashboard_and_lists_come_from_the_real_platform_db` / `page_is_served_with_the_right_type_and_no_data_leak` |
| D1：token 必填，缺失拒绝启动 | `binary_refuses_to_start_without_the_admin_token`（真子进程 + 退出码） |
| D1：只读，任何写路径都不存在 | `no_write_route_exists_on_the_read_surface`（405）+ `a_full_read_battery_leaves_the_platform_db_bytes_untouched`（字节比对）+ `read.rs` 两条写失败用例 |
| D2：`/api/audit?actor=&command=&from=&to=` | `audit_filters_combine_and_narrow` / `audit_rejects_bad_parameters_instead_of_returning_empty` |
| D2：按命令 id 查到 agent 的写操作 | `command_id_finds_what_the_agent_actually_wrote` |
| D2：`/api/reports` | `report_queue_defaults_to_unfinished_and_carries_the_state_machine` / `overdue_follows_the_configured_sla_not_a_hardcoded_number` |
| D3：没有 MFA 不能执行任何写 | `no_mfa_means_no_write_for_every_role` |
| D3：每次写都留痕 | `mfa_triage_reaches_the_platform_and_is_attributed_to_the_operator` / `target_shape_is_checked_before_the_socket_and_logged` / `dead_platform_is_a_transport_error_with_a_trail` |
| D3：RBAC | `role_matrix_who_can_do_what` / `static_token_cannot_write_at_all` |

实测输出见 §13（本机真进程跑出来的，不是抄的口号）；PR 描述里再贴一次命令与退出码。

---

## 13. 端到端实测（2026-10-20，本机 Windows，真进程 + 真 curl）

起 `wxlike-server`（18099）+ `wxlike-admin`（18090），跑完整流程，实测结果：

| 步骤 | 结果 |
|---|---|
| 缺 `WXLIKE_ADMIN_TOKEN` 起真二进制 | 退出码 **1**，stderr 点名变量与生成办法 ✅ |
| `--check` | 打印 平台库可读 `read_only` / 表齐全 是 / 账号库可写；令牌只报长度 ✅ |
| `GET /health` 无令牌 | 200，`writes_platform_db:false`、`accounts_present:false` ✅ |
| `GET /api/stats` 无令牌 / 错令牌 | 401 / 401 ✅ |
| 带 `x-wxlike-command: group.create` 打平台后 `GET /api/audit?command=group.create` | `total:1`，`actor` 是那个用户 id，`detail:"status=200 method=POST"` ✅ |
| 真实举报 → `GET /api/reports` | 工单以 `state:"open"`、`reason:"spam"` 出现，`overdue:false` ✅ |
| `GET /` | `<!doctype html>` + `<html lang="zh-CN">` ✅ |
| `POST /api/stats` | **405**（读面不存在写路由）✅ |
| CLI `account add` → 登录 → 写 | 会话 `mfa:false`；写 → **403**，消息里带"该怎么修" ✅ |
| CLI `totp-init` → 用真 TOTP 算法出码 → `totp-confirm` → 带码登录 | `mfa:true` ✅ |
| 有 MFA 的写 | **502 `upstream refused the admin token (platform 403: admin endpoint unavailable)`** ← §3.1 那一行没接，不是 adminx 的判定 |
| `GET /api/oplog` | 两条：`denied`（无 MFA）+ `upstream_error`（平台 403），字段含 actor/op/capability/target/upstream 路径/mfa ✅ |
| 读一轮之后平台库 md5 | `b9263bbf…` → 同一值，**未改动** ✅ |

## 14. 手工验收清单（这台机器验不了的，留给部署的人逐条打勾）

1. [ ] **§3.1 那一行接上之后**：`superadmin` 带 MFA 的会话分诊一张真工单，
   `GET /api/admin/reports?state=triaged` 能看到它，且 `handler` 是后台账号名；
   `/api/oplog` 出现 `status:"ok"`；
2. [ ] 审核模式经后台切 `post_review` 后，正在跑的 `wxlike-server` 行为真的变了
   （这条在自动化里已用内存态证明，真部署再确认一次）；
3. [ ] 浏览器里填令牌 → 看板数字与 `wxlike-server` 的 `/metrics` 不矛盾；不填令牌时
   页面显示 401 文案而不是空白卡；
4. [ ] `account add --role viewer` 的账号登录 → 工单页按钮全部报"需要 write 级权限"；
5. [ ] 杀掉 `wxlike-server` 再点处置 → 502 + 留痕 `transport_error`；重启后 `/health` 恢复；
6. [ ] **Linux/macOS** 上 `systemctl stop wxlike-admin` / `launchctl bootout` → 日志出现
   `graceful shutdown 完成`、退出码 0。（本机是 Windows，`kill -INT` 走不到 unix 信号分支，
   这条只能在那两个平台验）；
7. [ ] 审计行数上到 10⁶ 级时，`/api/audit?from=&to=` 不带 `actor` 的耗时——这时 §9 那两个
   索引必须已经补上；
8. [ ] 公网部署前：反代 + TLS + 登录限流（§11）。默认只绑 127.0.0.1 是最后一道，不是全部。

