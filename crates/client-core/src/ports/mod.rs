//! 端口（ports）：核心需要的全部外部能力，一个不落。
//!
//! 每个端口只描述**核心要什么**，不描述平台怎么做：
//!
//! | 端口 | 核心要什么 | 原生实现 | Web 实现 |
//! |---|---|---|---|
//! | [`Transport`] | 发请求、收事件流 | ureq + WS | fetch + WebSocket |
//! | [`LocalStore`] | 落 outbox、存事件、存缓存 | SQLite 事务 | IndexedDB 事务 |
//! | [`Clock`] | 现在几点、睡一会 | `SystemTime` + tokio | `Date.now` + 定时器 |
//! | [`Secrets`] | 取密钥、放密钥 | 文件 / Keychain | WebCrypto + IDB |
//! | [`Platform`] | 单实例锁、生命周期、能力声明 | 文件锁 + 信号 | Web Locks + visibilitychange |
//! | [`NotificationSink`] | 后台时提醒 | 系统通知 | Notification API |
//!
//! 端口方法全部带 `&self`（内部可变性由实现决定）与取消/截止时间的入口参数：
//! 一个"没法取消、没法超时"的端口，在移动端就是内存泄漏与电量黑洞。

pub mod clock;
pub mod notification;
pub mod platform;
pub mod secrets;
pub mod store;
pub mod transport;

use std::sync::Arc;

pub use clock::Clock;
pub use notification::{Notification, NotificationError, NotificationSink};
pub use platform::{InstanceLock, Lifecycle, Platform, PlatformCaps, PlatformError, Release};
pub use secrets::{Secret, SecretError, Secrets};
pub use store::{LocalStore, OutboxRecord, StoreError};
pub use transport::{
    EventSource, HttpMethod, Transport, TransportError, TransportRequest, TransportResponse,
};

/// 条件线程安全约束。
///
/// 原生构建要求 `Send + Sync`（tokio 会在线程间搬 future）；wasm 构建**不要求**
/// ——浏览器的 JS 句柄本来就不能跨线程，硬要求只会逼出 `unsafe impl`。
/// 这不是"放宽标准"，是把标准放在它真正成立的地方。
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync + ?Sized> MaybeSend for T {}

/// wasm：无约束（见上面的说明）。
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSend for T {}

/// 核心运行所需的全部端口。
///
/// 一个结构体而不是六个参数：`Session::open` 的签名不该每加一个端口就变一次。
#[derive(Clone)]
pub struct Ports {
    /// 网络。
    pub transport: Arc<dyn Transport>,
    /// 本地真相。
    pub store: Arc<dyn LocalStore>,
    /// 时间。
    pub clock: Arc<dyn Clock>,
    /// 密钥。
    pub secrets: Arc<dyn Secrets>,
    /// 平台能力。
    pub platform: Arc<dyn Platform>,
    /// 通知。
    pub notifications: Arc<dyn NotificationSink>,
}

impl Ports {
    /// 组装。
    #[must_use]
    pub fn new(
        transport: Arc<dyn Transport>,
        store: Arc<dyn LocalStore>,
        clock: Arc<dyn Clock>,
        secrets: Arc<dyn Secrets>,
        platform: Arc<dyn Platform>,
        notifications: Arc<dyn NotificationSink>,
    ) -> Self {
        Self {
            transport,
            store,
            clock,
            secrets,
            platform,
            notifications,
        }
    }
}

impl std::fmt::Debug for Ports {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ports")
            .field("caps", &self.platform.caps())
            .finish_non_exhaustive()
    }
}
