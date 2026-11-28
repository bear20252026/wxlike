# 审计整改后的 CI 基线恢复

日期：2026-11-27
触发提交：`5583221`（H-30），前一失败基线 `3883913`
工作流：`ci` run `46022152317`、`security` run `46022152191`

## 结论

`audit` 工作流已通过；`ci` / `security` 的失败不是新的生产缺陷，而是三类“整改后没同步”的问题：

1. 代码风格/文档门禁被新代码触发；
2. 测试仍按整改前的语义断言；
3. 旧 UI 退役、依赖变更后，锁文件与 provenance 登记没跟上。

本文件记录逐项修法，作为 `main` 恢复绿色基线的证据索引。绿色的判定标准仍是修复提交后的完整 `ci` + `security` 全绿，不以本文件替代。

## 逐项修复

| 失败 job | 根因 | 修法 |
|---|---|---|
| `clippy` | H-41 的 `wire_aliases` 嵌套 `if` 触发 `collapsible_if` | 合并外层条件，保留字段别名语义 |
| `doc` | H-22 改动后 `CallbackPolicy::resolve` 不在 `jobs.rs` 作用域 | 改为可解析的 `crate::CallbackPolicy::resolve` |
| `test` ×3 / `coverage` | H-14 在 `totp_enroll` 撤销全部会话，测试仍用 enroll 前 token 读 `/api/me` | enroll 后用密码重新登录；confirm 后断言 enroll 前后两枚 token 均 401 |
| `e2e` / `image` | HTTP 发送每次都重新生成 `sent_at_ms`，hub 的完整 envelope 比较把“同 key 同 payload”的重试判成冲突；e2e 对异 payload 的旧期望也需要更新 | HTTP 层先回读已落库消息并按调用方字段判定重放；hub 冲突后再回读一次，同请求返回原 msg_id、异请求 409。e2e 同步改为同 payload 200、异 payload 409 |
| `pg-migration` | H-05 的 `redpacket_claims.user_id` 外键指向兼容视图 `users`；PG 不允许外键指向视图 | 改为 `identity_user(user_id)`（M011 已声明这是新表唯一合法目标） |
| `security (source + policy)` | C-02 删除旧 UI 后，registry 仍登记 `ui/src/vendor/tweb/*` 两个文件；H-34 改了 `identity/session.rs` 但登记 SHA 未更新 | 删除已不存在文件的两条登记；更新 `session.rs` 的 `local_sha256` |
| `security (authz + abuse + decode)` | C-04 新增 `rand`/`libc` 依赖，`tests/security/Cargo.lock` 未更新 | 重新生成锁文件（仅新增依赖边） |
| `pg-migration`（修完 SQL 后） | `PostgresPool` 的 `PoolConnection` 在 `block_on` 之外 drop；普通 `#[test]` 线程没有 Tokio 上下文，sqlx 归还连接时 panic | `with_conn` / `with_conn_atomic` 把连接归还放进专属 runtime 的 `block_on` 内 |
| `audit` / `desktop` | 同一依赖变更导致 `ui/src-tauri/Cargo.lock` 在 `--locked` 下不可用 | 重新生成桌面锁文件（仅新增 `rand 0.8.8`、`libc`） |

## 语义说明

- **幂等键**：同一 key + 同一 payload 返回原消息；同一 key + 不同 payload 返回 409。后者是 H-41 的有意收紧，避免“换内容重放”被静默吞掉。e2e 的旧断言属于整改前语义，已更新。
- **MFA enroll/confirm**：两者都改变认证边界，都会撤销既有会话；测试必须重新登录后才能继续读 `/api/me` 或 confirm。
- **HTTP 幂等**：同 key 同 payload 返回原 `msg_id`；同 key 换正文/引用返回 409。HTTP 层只比较调用方字段（会话、发送者、收件人、正文、引用），`seq`/`msg_id`/`sent_at_ms` 是服务端生成的不参与；hub 原有的完整 envelope 校验不变，继续防“同 msg_id 换内容”的伪造帧。
- **PG 外键**：`users` 是 M011 的兼容视图，只供旧 SQL 读写；新表外键必须落 `identity_user`。这是 schema registry 已写明的约束，H-05 建表时漏了。

## 验证边界

- 本地验证：`cargo fmt --check`；`cargo metadata --locked`（桌面与 security 两套锁）；`cargo test -p wxlike-admin --test d3_rbac_mfa`（14 passed）；`cargo clippy -p wxlike-client-core -p wxlike-video -p wxlike-admin --all-targets`；`RUSTDOCFLAGS="-D warnings" cargo doc -p wxlike-video --no-deps`；`sourceaudit provenance check`（fail=0）；`python3 -m py_compile tests/e2e/run.py`。
- 本地 e2e（修复后的二进制）：`smoke` 与 `errors` 两个套件全过；其中 `errors` 覆盖“同 payload 200、异 payload 409”。
- 未在本地验证：PostgreSQL 19 实际迁移（本机无 `psql`）；Redis 集成用例（本机无 Redis）；Flutter/Dart 测试（本机无工具链）。这些仍以 CI 为唯一真实证据。
- 下一道门：修复提交推送后，`ci` 与 `security` 两个 workflow 的全部非可选 job 必须为 success，`gate` 才会通过。

## 第三轮（`89b1a3e` 之后）

`89b1a3e` 的 `ci` run `46043972474` 仍有 `test ×3` / `coverage` 失败，`security` run `46043972330` 的 `source + policy` 失败：

| 失败 job | 根因 | 修法 |
|---|---|---|
| `test ×3` / `coverage` | `g02_09_live_evidence` 的 Admin 侧车 fixture 把 `auth_level` 写成 `'roo'`、`features` 写成 `'VLX'`；严格解析分别报 `corrupt stored row: auth_level` / `features` | 改为合法值 `'root'` / `'VL'`；本地 `cargo test -p wxlike-admin --test g02_09_live_evidence` 2 passed |
| `security (source + policy)` | C-02/C-04 改动锁文件后 committed SBOM 未重生成，20 条 `@esbuild/*@0.28.1` 漂移 | `sourceaudit sbom generate` 重生成；`sbom check`、`advisories check --offline`、`authz check` 本地全 PASS |

本地同时通过：`sbom check`（components=1392，fail=0）、`provenance check`（fail=0 pass=78）、`advisories check --offline`（fail=0 warn=10）、`authz check`（fail=0）。

### 第三轮补丁：红包对账 fixture

`test ×3` / `coverage` 还暴露 `crates/platform/tests/p4_ops.rs::reconcile_clean_after_wallet_ops`：fixture 用字面 `d-a~b` 直聊 id，而 H-01/H-02/H-03 已按真实用户 id 校验会话成员，`claim_redpacket` 返回 `Forbidden`。改为用注册返回的 `a`/`b` 拼 `d-{a}~{b}`；本地 `cargo test -p wxlike-platform --test p4_ops` 18 passed。

`image` job 的失败是 `anchore/sbom-action` 下载 syft v1.42.3 时 GitHub 返回 504，属外部瞬态，代码与镜像检查未失败。

### 第三轮补丁：转发幂等

`mailbox_mirror::http_forward_copies_into_the_target_conversation` 的第二次同 key 转发返回 500：`build_forward` 每次重新生成 `sent_at_ms`，hub 的完整 envelope 比较把正常重试判成 `QuoteReplayConflict`，`forward_message` 又把它统一映射成 500。转发入口改为与 `/send` 共用 `http_send_replay`：先回读已落库转发，同请求直接返回原 `msg_id`；并发冲突后再回读一次，同请求成功、异请求 409。本地 `cargo test -p wxlike-server --test mailbox_mirror` 13 passed。
