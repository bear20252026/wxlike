//! 平台端口：单实例、生命周期、能力声明。
//!
//! 三件事，每件都对应一个真实事故：
//!
//! * **单实例**：浏览器开两个 tab、桌面双击两次图标，就有两个核心同时消费
//!   同一个游标、同时补发同一个 outbox——消息会重复发。锁必须由平台提供
//!   （Web Locks / 文件锁），不能靠"我们约定只开一个窗口"。
//! * **生命周期**：手机切后台后定时器会被冻、Web 页面会被 `freeze`。
//!   核心要知道"我现在可能不被调度"，才能把重连与补发安排到恢复之后。
//! * **能力声明**：某个端没有 WebSocket 就用长轮询，没有后台任务就在前台
//!   补发。**不支持要有替代路径**，不能只是把按钮置灰（G00-01 的验收条款）。

use super::MaybeSend;

/// 本端能力。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformCaps {
    /// 有长连接（WebSocket / QUIC）。
    pub websocket: bool,
    /// 进程/页面在后台时仍能跑任务。
    pub background_tasks: bool,
    /// 有系统级安全存储（Keychain / Keystore / WebCrypto + IDB）。
    pub secure_storage: bool,
    /// 有推送通道。
    pub push: bool,
    /// 有事务型本地库（IndexedDB / SQLite）。
    pub transactional_store: bool,
}

impl PlatformCaps {
    /// 最保守的一组（纯前端页面、私有模式）。
    #[must_use]
    pub fn minimal() -> Self {
        Self {
            websocket: false,
            background_tasks: false,
            secure_storage: false,
            push: false,
            transactional_store: false,
        }
    }

    /// 桌面/移动端的常见组合。
    #[must_use]
    pub fn native() -> Self {
        Self {
            websocket: true,
            background_tasks: true,
            secure_storage: true,
            push: true,
            transactional_store: true,
        }
    }
}

/// 生命周期事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    /// 回到前台 / 页面可见。
    Foreground,
    /// 进入后台（可能被冻）。
    Background,
    /// 被系统冻结（Web 的 `freeze` 事件、iOS 的挂起）。
    Frozen,
    /// 从冻结恢复。**必须重新校验游标与 outbox**：冻结期间可能断网、可能
    /// 被别的设备消费过。
    Resumed,
}

/// 平台错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlatformError {
    /// 另一个实例已经持有锁。
    AlreadyRunning,
    /// 平台不支持（例如私有模式下 Web Locks 不可用）。
    Unsupported(String),
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlatformError::AlreadyRunning => f.write_str("已有另一个实例在运行"),
            PlatformError::Unsupported(what) => write!(f, "平台不支持：{what}"),
        }
    }
}

impl std::error::Error for PlatformError {}

/// 释放动作。单独一个 trait 而不是 `Box<dyn FnMut()>`：后者没有 `Send`，
/// 原生侧拿不住（锁对象会被搬进后台线程）。
pub trait Release: MaybeSend {
    /// 释放。幂等。
    fn release(&mut self);
}

/// 单实例锁。**`Drop` 即释放**：忘了解锁的路径太多，靠显式调用一定会漏。
pub struct InstanceLock {
    release: Option<Box<dyn Release>>,
}

impl InstanceLock {
    /// 由平台实现构造。
    #[must_use]
    pub fn new(release: Box<dyn Release>) -> Self {
        Self {
            release: Some(release),
        }
    }

    /// 立刻释放（也可以交给 `Drop`）。
    pub fn unlock(&mut self) {
        if let Some(mut release) = self.release.take() {
            release.release();
        }
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        self.unlock();
    }
}

impl std::fmt::Debug for InstanceLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceLock")
            .field("held", &self.release.is_some())
            .finish()
    }
}

/// 平台端口。
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait Platform: MaybeSend {
    /// 能力声明。
    fn caps(&self) -> PlatformCaps;

    /// 抢单实例锁。
    ///
    /// # Errors
    /// 已有实例持锁时 [`PlatformError::AlreadyRunning`]；平台不支持时
    /// [`PlatformError::Unsupported`]（调用方应降级为"只读模式"并提示用户）。
    async fn acquire_instance_lock(&self) -> Result<InstanceLock, PlatformError>;

    /// 取下一个生命周期事件；返回 `None` 表示平台不再提供（退出中）。
    async fn next_lifecycle(&self) -> Option<Lifecycle>;
}
