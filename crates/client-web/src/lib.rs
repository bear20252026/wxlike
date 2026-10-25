//! `wxlike-client-web`：客户端核心在**浏览器**里的落地（wasm32）。
//!
//! 分工与原生侧一致：业务逻辑全在 `wxlike-client-core`，这里只补平台能力。
//!
//! | 端口 | 这里的实现 | 浏览器特有的坑 |
//! |---|---|---|
//! | `Transport` | `fetch::FetchTransport` | 只有 `AbortSignal` 能真正取消请求 |
//! | `EventSource` | `ws::WebSocketEvents` | 回调式 API，要接成队列 |
//! | `LocalStore` | `idb::IdbStore` | 配额用尽 / 私有模式是两种不同的失败 |
//! | `Secrets` | `crypto::WebSecrets` | `localStorage` 不是安全存储（见文档） |
//! | `Clock` | `clock::WebClock` | 后台标签页定时器被节流 |
//! | `Platform` | `platform::WebPlatform` | 跨 tab 互斥只能靠 Web Locks |
//!
//! **没有"TS 业务同步引擎"**：浏览器里也不写第二套同步逻辑。`ui/` 只负责
//! 渲染与事件绑定，所有 dispatch / 重试 / 落盘都回到这个 crate 背后的核心。
//!
//! 纯策略（配额判定、冻结恢复、缓存敏感度）在 [`policy`]，与平台无关，
//! 所以在原生上也能跑单测——浏览器行为由 `tests/browser.rs` 覆盖。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod policy;

#[cfg(target_arch = "wasm32")]
pub mod clock;
#[cfg(target_arch = "wasm32")]
pub mod crypto;
#[cfg(target_arch = "wasm32")]
pub mod fetch;
#[cfg(target_arch = "wasm32")]
pub mod idb;
#[cfg(target_arch = "wasm32")]
pub mod platform;
#[cfg(target_arch = "wasm32")]
pub mod ws;

#[cfg(target_arch = "wasm32")]
pub use clock::WebClock;
#[cfg(target_arch = "wasm32")]
pub use crypto::{WebCacheCipher, WebSecrets};
#[cfg(target_arch = "wasm32")]
pub use fetch::FetchTransport;
#[cfg(target_arch = "wasm32")]
pub use idb::IdbStore;
#[cfg(target_arch = "wasm32")]
pub use platform::WebPlatform;
#[cfg(target_arch = "wasm32")]
pub use ws::WebSocketEvents;

#[cfg(target_arch = "wasm32")]
use std::sync::Arc;

/// 组装浏览器端端口。
///
/// 注意返回的是 `Ports`，不是"另一个核心"：核心只有一个
/// （`wxlike_client_core::runtime::Session`），这里只是把平台实现塞进去。
///
/// # Errors
/// IndexedDB 打不开（私有模式）时返回原因；调用方应进入
/// `policy::StorageMode` 的降级路径而不是白屏。
// wasm 是单线程：这些实现持有 `Rc` / JS 句柄，本来就不是 `Send + Sync`。
// 端口类型用 `Arc` 只是为了和原生侧同一个形状；在浏览器里 `Arc` 的原子操作
// 退化成普通读写，没有额外代价，也没有跨线程语义要维护。
#[allow(clippy::arc_with_non_send_sync)]
#[cfg(target_arch = "wasm32")]
pub async fn browser_ports(
    base_url: &str,
) -> Result<wxlike_client_core::ports::Ports, wxlike_client_core::ports::StoreError> {
    let store = idb::IdbStore::open().await?;
    let transport = fetch::FetchTransport::new(base_url);
    let secrets = crypto::WebSecrets::new()
        .map_err(|err| wxlike_client_core::ports::StoreError::Unavailable(err.to_string()))?;
    let notifications = crate::platform::WebPlatform::new();
    Ok(wxlike_client_core::ports::Ports::new(
        Arc::new(transport),
        Arc::new(store),
        Arc::new(clock::WebClock::new()),
        Arc::new(secrets),
        Arc::new(notifications),
        Arc::new(NoNotifications),
    ))
}

/// 浏览器端通知：走 `Notification` API 是宿主（页面）的事，核心只投递。
#[cfg(target_arch = "wasm32")]
struct NoNotifications;

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl wxlike_client_core::ports::NotificationSink for NoNotifications {
    fn is_foreground(&self) -> bool {
        true
    }

    async fn notify(
        &self,
        _notification: wxlike_client_core::ports::Notification,
    ) -> Result<(), wxlike_client_core::ports::NotificationError> {
        Ok(())
    }
}
