//! `wxlike-client-native`：客户端核心在**原生平台**（桌面 Tauri / 移动 FFI）的落地。
//!
//! 只做"平台相关"的四件事，业务逻辑一律在 `wxlike-client-core`：
//!
//! | 端口 | 这里的实现 | 为什么必须平台相关 |
//! |---|---|---|
//! | [`LocalStore`](wxlike_client_core::ports::LocalStore) | [`SqliteStore`] | 需要真事务与 `synchronous=FULL` |
//! | [`Transport`](wxlike_client_core::ports::Transport) | [`HttpTransport`] | 复用 ureq + 现有鉴权头 |
//! | [`Clock`](wxlike_client_core::ports::Clock) | [`SystemClock`] | 系统时间 + tokio 定时器 |
//! | [`Secrets`](wxlike_client_core::ports::Secrets) | [`FileSecrets`] | 设备文件 / 后续换 Keychain |
//! | [`Platform`](wxlike_client_core::ports::Platform) | [`FileLockPlatform`] | 跨进程单实例锁 |
//! | [`NotificationSink`](wxlike_client_core::ports::NotificationSink) | [`ChannelNotifications`] | 交给宿主弹系统通知 |
//!
//! 边界：这一层**不实现业务规则**，也不持有 UI 状态。加一个功能先去
//! `wxlike-cmd` 加命令、去 `client-core` 加路由，别在这里开新分支。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod clock;
pub mod core;
pub mod notify;
pub mod platform;
pub mod secrets;
pub mod store;
pub mod transport;

pub use clock::SystemClock;
pub use core::{default_data_dir, NativeConfig, NativeCore, NativeError};
pub use notify::ChannelNotifications;
pub use platform::FileLockPlatform;
pub use secrets::{CacheCipher, FileSecrets, CACHE_KEY};
pub use store::SqliteStore;
pub use transport::HttpTransport;
