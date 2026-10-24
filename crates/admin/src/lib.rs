//! `wxlike-admin` — 运营后台 **adminx**（Zone D：D1 只读后台 / D2 审计检索与工单 /
//! D3 RBAC + MFA）。任务卡见 `docs/task-assignment.md` §5 Zone D，边界契约见
//! `docs/parallel-tracks.md` 轨道 A。
//!
//! 它是一个**独立进程**（`wxlike-admin`），与 `wxlike-server` 同时运行：
//!
//! ```text
//! 运维浏览器 ──HTTP──> adminx (:8090)
//!                         │
//!                         ├── 平台库 SQLite ──(只读 + query_only)──┐
//!                         │                                        ▼
//!                         └──写操作转发 x-admin-token──> wxlike-server (:8099)
//! ```
//!
//! 两条不可动摇的边界：
//!
//! 1. **adminx 不写平台库**。读面全部走只读连接；所有会改数据的动作都转成对
//!    平台管理端点（`/api/admin/*`）的一次 HTTP 调用。理由不是洁癖：平台进程
//!    里有内存态（审核模式、风控窗口、群成员缓存、通话房间），绕过它直接改库
//!    会让"运营在后台改了、服务其实没改"——本项目明令禁止这类静默降级。
//! 2. **业务规则不在这里**。谁能看什么、工单能不能这么流转，判定都在
//!    `wxlike-platform` / `wxlike-report`；adminx 只做展示、编排与准入。
//!
//! 凭据原语（argon2id、TOTP）直接借用 `wxlike-platform` 的公开函数，不重写一遍：
//! 重写就是两套参数、两套容差，而这类差异只在出事时才暴露。
//!
//! # 上手
//!
//! ```bash
//! # 1. 建第一个管理员（写操作要账号，读面只要令牌）
//! WXLIKE_ADMIN_TOKEN=… wxlike-admin account add --name ops --role superadmin
//! # 2. 起服务
//! WXLIKE_ADMIN_TOKEN=… wxlike-admin
//! # 3. 浏览器打开 http://127.0.0.1:8090
//! ```
//!
//! # 库用法（集成测试与嵌入式）
//!
//! ```no_run
//! # fn main() -> anyhow::Result<()> {
//! let cfg = wxlike_admin::Config {
//!     platform_db: std::path::PathBuf::from("/var/lib/wxlike/wxlike.db"),
//!     accounts_db: std::path::PathBuf::from("/var/lib/wxlike/adminx.db"),
//!     token: "从环境来的长随机串".into(),
//!     platform_token: "从环境来的长随机串".into(),
//!     platform_url: "http://127.0.0.1:8099".into(),
//!     bind: "127.0.0.1:8090".into(),
//!     report_sla_ms: 24 * 60 * 60 * 1000,
//!     session_ttl_ms: 8 * 60 * 60 * 1000,
//!     shutdown_grace: std::time::Duration::from_secs(10),
//! };
//! let app = wxlike_admin::App::open(&cfg)?.router();
//! # let _ = app; Ok(()) }
//! ```

pub mod accounts;
pub mod audit;
pub mod auth;
pub mod commerce;
pub mod config;
pub mod error;
pub mod g14;
pub mod http;
pub mod identity;
pub mod jobs;
pub mod publishing;
pub mod rbac;
pub mod read;
pub mod store;
pub mod upstream;
pub mod web;

pub use accounts::{AccountStore, OpRecord, OpStatus, Session};
pub use audit::AuditLog;
pub use auth::{Action, Resource, StepUp};
pub use config::{Config, ConfigError};
pub use error::{AdminError, Result};
pub use g14::G14;
pub use http::App;
pub use rbac::{Capability, Role, OPS};
pub use read::PlatformRead;
pub use store::Store;
pub use upstream::Upstream;

#[cfg(test)]
pub(crate) mod tmp;
