//! 浏览器平台：跨 tab 单实例、生命周期、能力声明。
//!
//! **跨 tab 互斥不是"约定只开一个窗口"**。两个 tab 各跑一个核心时：同一个
//! 游标被消费两次（事件重复交付）、同一个 outbox 被补发两次（服务端靠幂等键
//! 去重，但请求翻倍）。
//!
//! 实现是 `localStorage` 上的**带过期的租约**：
//!
//! 1. 读现有租约，没过期且不是自己 → `AlreadyRunning`；
//! 2. 写入自己的租约（`tab` + `expires_at_ms`）；
//! 3. **读回确认**：两个 tab 同时写时后写者胜，读回来不是自己的就认输。
//!
//! 为什么不是 Web Locks：`navigator.locks` 的锁在回调返回的 promise 落地时
//! 释放，要长期持锁得自己拿住 resolve 函数，跨 tab 调试成本高；而租约的
//! 语义（"谁在最近 N 秒内续过约"）恰好就是我们要的——**页面崩了租约自然
//! 过期**，不需要任何清理逻辑。
//!
//! 代价写在明处：这不是强互斥。极端情况下（两个 tab 在同一个 tick 里写，
//! 且读回时序被浏览器打乱）可能短暂双活。所以：
//!
//! * 核心侧仍然靠幂等键保证"双活也不重复发消息"；
//! * 页面胶水必须每 [`LEASE_RENEW_MS`] 调一次 [`WebPlatform::renew_lease`]。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use wxlike_client_core::ports::{
    Clock, InstanceLock, Lifecycle, Platform, PlatformCaps, PlatformError, Release,
};

use crate::clock::WebClock;

/// 租约键。
const LEASE_KEY: &str = "wxlike.core.lease";
/// 租约有效期（毫秒）：页面崩溃后最多这么久别的 tab 才能接手。
pub const LEASE_MS: u64 = 15_000;
/// 续约间隔建议值（毫秒）：比有效期短一半，留一次重试的余量。
pub const LEASE_RENEW_MS: u64 = 5_000;
/// 生命周期轮询间隔（毫秒）。
const LIFECYCLE_POLL_MS: u64 = 200;
/// 生命周期队列上限：它是"状态"不是"流水"。
const LIFECYCLE_QUEUE: usize = 8;

/// 浏览器平台。
pub struct WebPlatform {
    caps: PlatformCaps,
    lifecycle: Rc<RefCell<VecDeque<Lifecycle>>>,
    tab_id: Rc<RefCell<Option<String>>>,
}

impl WebPlatform {
    /// 构造：有 WebSocket、有事务型存储；没有后台任务与推送。
    #[must_use]
    pub fn new() -> Self {
        Self {
            caps: PlatformCaps {
                websocket: true,
                background_tasks: false,
                secure_storage: true,
                push: false,
                transactional_store: true,
            },
            lifecycle: Rc::new(RefCell::new(VecDeque::new())),
            tab_id: Rc::new(RefCell::new(None)),
        }
    }

    /// 宿主（页面胶水）投递生命周期事件。
    ///
    /// 页面侧要挂三个监听：`visibilitychange`、`freeze`、`resume`。
    pub fn notify_lifecycle(&self, event: Lifecycle) {
        let mut queue = self.lifecycle.borrow_mut();
        if queue.len() >= LIFECYCLE_QUEUE {
            queue.pop_front();
        }
        queue.push_back(event);
    }

    /// 续约。页面胶水每 [`LEASE_RENEW_MS`] 调一次。
    ///
    /// 返回 `false` 表示租约已经不属于自己（另一个 tab 接手了）：这时应当
    /// 主动降级成只读，而不是继续跑两个核心。
    pub fn renew_lease(&self) -> bool {
        let Some(tab_id) = self.tab_id.borrow().clone() else {
            return false;
        };
        let Ok(storage) = local_storage() else {
            return false;
        };
        write_lease(&storage, &tab_id).is_ok()
    }

    fn storage() -> Result<web_sys::Storage, PlatformError> {
        local_storage().map_err(PlatformError::Unsupported)
    }
}

impl Default for WebPlatform {
    fn default() -> Self {
        Self::new()
    }
}

fn local_storage() -> Result<web_sys::Storage, String> {
    let window = web_sys::window().ok_or_else(|| "没有 window（非浏览器环境）".to_string())?;
    window
        .local_storage()
        .map_err(|err| format!("localStorage 不可用：{err:?}"))?
        .ok_or_else(|| "localStorage 被禁用（私有模式？）".to_string())
}

fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

/// 生成 tab id：时间戳 + 自增，够用且不依赖随机数。
fn new_tab_id() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let sequence = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("tab-{}-{sequence}", now_ms())
}

fn write_lease(storage: &web_sys::Storage, tab_id: &str) -> Result<(), String> {
    let lease = format!("{tab_id}|{}", now_ms().saturating_add(LEASE_MS as i64));
    storage
        .set_item(LEASE_KEY, &lease)
        .map_err(|err| format!("写租约失败：{err:?}"))
}

/// 读当前租约，返回（持有者, 到期时间）。
fn read_lease(storage: &web_sys::Storage) -> Option<(String, i64)> {
    let raw = storage.get_item(LEASE_KEY).ok().flatten()?;
    let (owner, expires) = raw.split_once('|')?;
    let expires = expires.parse::<i64>().ok()?;
    Some((owner.to_string(), expires))
}

/// 释放：只删自己的租约。
struct LeaseRelease {
    storage: web_sys::Storage,
    tab_id: String,
}

impl Release for LeaseRelease {
    fn release(&mut self) {
        if let Some((owner, _)) = read_lease(&self.storage) {
            if owner == self.tab_id {
                let _ = self.storage.remove_item(LEASE_KEY);
            }
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Platform for WebPlatform {
    fn caps(&self) -> PlatformCaps {
        self.caps
    }

    async fn acquire_instance_lock(&self) -> Result<InstanceLock, PlatformError> {
        let storage = Self::storage()?;
        let tab_id = new_tab_id();
        if let Some((owner, expires)) = read_lease(&storage) {
            if owner != tab_id && expires > now_ms() {
                return Err(PlatformError::AlreadyRunning);
            }
        }
        write_lease(&storage, &tab_id).map_err(PlatformError::Unsupported)?;
        // 读回确认：同时写时后写者胜
        match read_lease(&storage) {
            Some((owner, _)) if owner == tab_id => {
                *self.tab_id.borrow_mut() = Some(tab_id.clone());
                Ok(InstanceLock::new(Box::new(LeaseRelease {
                    storage,
                    tab_id,
                })))
            }
            _ => Err(PlatformError::AlreadyRunning),
        }
    }

    async fn next_lifecycle(&self) -> Option<Lifecycle> {
        loop {
            if let Some(event) = self.lifecycle.borrow_mut().pop_front() {
                return Some(event);
            }
            WebClock::new().sleep(LIFECYCLE_POLL_MS).await;
        }
    }
}
