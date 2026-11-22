//! 持久化端口（G00-04 S2）。
//!
//! 这里只放 **跨 crate 的 trait 与 DTO**，不放任何 SQL、不放 rusqlite/sqlx。
//! 这样 `wxlike-platform` 与 `wxlike-identity` 可以各自实现同一个端口，
//! 而调用方只依赖端口，不依赖某个数据库驱动。
//!
//! 第一批只有身份域的两个端口：
//!
//! - [`users::UsersRepo`]：注册、登录校验、资料读写、注销、密码/TOTP 写入。
//! - [`sessions::SessionsRepo`]：访问会话的增删查、按 sid/设备撤销、过期清理。
//!
//! 错误类型用关联类型 `Error`，不把平台错误或 feature 错误硬编码进端口。

pub mod sessions;
pub mod users;

pub use sessions::{NewSession, SessionRecord, SessionsRepo};
pub use users::{NewUser, UserCredentials, UserProfileRow, UsersRepo};
