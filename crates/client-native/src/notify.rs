//! 通知：核心 → 宿主的一条通道。
//!
//! 核心不直接弹系统通知：那是宿主的事（Tauri 的 `Notification`、
//! Flutter 的 `flutter_local_notifications`）。这里做的是**排队 + 前台抑制**：
//! 核心只在后台时把通知投进通道，宿主取走并显示。
//!
//! 前台抑制必须在核心侧做：宿主不知道自己"该不该弹"（它只知道窗口在不在
//! 前台，但不知道用户当前是不是正在看那个会话）。两边都判断就会弹两次。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tokio::sync::broadcast;
use wxlike_client_core::ports::{Notification, NotificationError, NotificationSink};

/// 通道容量：通知是"最新比全量重要"，积压没意义。
const CHANNEL_CAPACITY: usize = 32;

/// 把通知投给宿主的实现。
pub struct ChannelNotifications {
    foreground: AtomicBool,
    sender: broadcast::Sender<Notification>,
    /// 最近若干条，便于诊断与测试断言。
    recent: Mutex<Vec<Notification>>,
}

impl ChannelNotifications {
    /// 构造（默认前台）。
    #[must_use]
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            foreground: AtomicBool::new(true),
            sender,
            recent: Mutex::new(Vec::new()),
        }
    }

    /// 宿主切换前台/后台时调用。
    pub fn set_foreground(&self, foreground: bool) {
        self.foreground.store(foreground, Ordering::SeqCst);
    }

    /// 订阅通知（宿主侧）。
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.sender.subscribe()
    }

    /// 最近投递过的通知（诊断用）。
    #[must_use]
    pub fn recent(&self) -> Vec<Notification> {
        self.recent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Default for ChannelNotifications {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl NotificationSink for ChannelNotifications {
    fn is_foreground(&self) -> bool {
        self.foreground.load(Ordering::SeqCst)
    }

    async fn notify(&self, notification: Notification) -> Result<(), NotificationError> {
        if self.is_foreground() {
            // 前台已经有角标/红点，再弹一次就是重复打扰
            return Ok(());
        }
        self.recent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(notification.clone());
        // 没有订阅者不算失败：宿主可能还没起来
        let _ = self.sender.send(notification);
        Ok(())
    }
}
