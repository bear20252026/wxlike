# G02 集成请求（身份 / 关系 / 安全）

本目录是**请求**，不是执行结果。根清单、路由挂载、迁移编号、生成绑定一律由 G00
串行执行；G02 只提交"我需要什么"与"我改了什么"。

## 通用约定

| 项 | 值 |
|---|---|
| 契约 | `contracts/domains/identity/contract.json`（identity-v1）、`relations/contract.json`（relations-v1）、`security/contract.json`（security-v1） |
| 契约 fixture | 各域 `fixtures/*.json`，其他组按 fixture 并行开发 |
| 迁移草稿 | `schema/proposals/G02/*.sql` + `README.md`（含回滚兼容说明） |
| 新增 workspace 成员 | `crates/features/identity`、`crates/features/relations`、`crates/features/security` |
| 测试 | `cargo test -p wxlike-identity -p wxlike-relations -p wxlike-security` |
| 门禁 | `cargo clippy --all-targets -- -D warnings`、`cargo fmt --check`、`python scripts/quality-gate.py` |

## 已由 G02 临时完成、请 G00 复核的两处根级改动

1. 根 `Cargo.toml` 的 `members` 追加三个 `crates/features/*`。
   理由：不加入 workspace，CI 不会编译这三个 crate，等于没有门禁。G00 的
   "生成绑定"流程请把它们纳入生成清单，避免下次重生成时丢失。
2. `crates/core/src/e2ee.rs` 纯新增 `EncryptedEnvelope` 与 `seal_secret_blob` /
   `open_secret_blob`（该文件在 G02 的 exclusive roots 内），未改动任何既有签名。

## 尚未完成（不得据此标 ACCEPTED）

* 平台层 HTTP 路由与 handler 未接线：G02 只改 features 层，避免与并行组抢
  `crates/platform` 的文件；
* G02-06 短信供应商 sandbox 证据（需真实凭据）；
* G02-07 真实手机扫真实桌面码证据（需 G10/G12 页面合入）。

逐条状态见 `docs/full-platform-delivery/handoff/G02-delivery.md`。
