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
| `e2e` / `image` | H-41 把“同 key 不同 payload”改为 409；e2e 仍期望 200 + 同 msg_id | 重放用例改为同 payload 200/同 msg_id；新增异 payload 409 断言 |
| `pg-migration` | H-05 的 `redpacket_claims.user_id` 外键指向兼容视图 `users`；PG 不允许外键指向视图 | 改为 `identity_user(user_id)`（M011 已声明这是新表唯一合法目标） |
| `security (source + policy)` | C-02 删除旧 UI 后，registry 仍登记 `ui/src/vendor/tweb/*` 两个文件；H-34 改了 `identity/session.rs` 但登记 SHA 未更新 | 删除已不存在文件的两条登记；更新 `session.rs` 的 `local_sha256` |
| `security (authz + abuse + decode)` | C-04 新增 `rand`/`libc` 依赖，`tests/security/Cargo.lock` 未更新 | 重新生成锁文件（仅新增依赖边） |
| `audit` / `desktop` | 同一依赖变更导致 `ui/src-tauri/Cargo.lock` 在 `--locked` 下不可用 | 重新生成桌面锁文件（仅新增 `rand 0.8.8`、`libc`） |

## 语义说明

- **幂等键**：同一 key + 同一 payload 返回原消息；同一 key + 不同 payload 返回 409。后者是 H-41 的有意收紧，避免“换内容重放”被静默吞掉。e2e 的旧断言属于整改前语义，已更新。
- **MFA enroll/confirm**：两者都改变认证边界，都会撤销既有会话；测试必须重新登录后才能继续读 `/api/me` 或 confirm。
- **PG 外键**：`users` 是 M011 的兼容视图，只供旧 SQL 读写；新表外键必须落 `identity_user`。这是 schema registry 已写明的约束，H-05 建表时漏了。

## 验证边界

- 本地验证：`cargo fmt --check`；`cargo metadata --locked`（桌面与 security 两套锁）；`cargo test -p wxlike-admin --test d3_rbac_mfa`（14 passed）；`cargo clippy -p wxlike-client-core -p wxlike-video -p wxlike-admin --all-targets`；`RUSTDOCFLAGS="-D warnings" cargo doc -p wxlike-video --no-deps`；`sourceaudit provenance check`（fail=0）；`python3 -m py_compile tests/e2e/run.py`。
- 本地 e2e：仓库里 13:40 的旧二进制对 smoke 套件（同 payload 重放）通过；旧二进制尚未合入 H-41 的冲突分支，因此“异 payload 409”只能由 CI 镜像验证。
- 未在本地验证：PostgreSQL 19 实际迁移（本机无 `psql`）；Redis 集成用例（本机无 Redis）；Flutter/Dart 测试（本机无工具链）。这些仍以 CI 为唯一真实证据。
- 下一道门：修复提交推送后，`ci` 与 `security` 两个 workflow 的全部非可选 job 必须为 success，`gate` 才会通过。
