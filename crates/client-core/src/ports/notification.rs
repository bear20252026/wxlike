//! 通知端口。
//!
//! 核心只在"应用不在前台"时发通知：前台已经有未读角标了，再弹一次系统通知
//! 就是重复打扰。判断前台与否交给实现（[`NotificationSink::is_foreground`]），
//! 核心不猜。

use wxlike_types::{ConvId, EventId};

use super::MaybeSend;

/// 一条通知。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// 标题（会话名）。
    pub title: String,
    /// 正文（消息摘要）。
    pub body: String,
    /// 相关会话，点击时跳转用。
    pub conv: Option<ConvId>,
    /// 相关事件，用于去重（同一事件不该弹两次）。
    pub event_id: Option<EventId>,
}

/// 通知失败。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationError {
    /// 用户没授权 / 系统关闭了通知。
    Denied,
    /// 通道不可用。
    Unavailable(String),
}

impl std::fmt::Display for NotificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotificationError::Denied => f.write_str("通知权限被拒绝"),
            NotificationError::Unavailable(detail) => write!(f, "通知不可用：{detail}"),
        }
    }
}

impl std::error::Error for NotificationError {}

/// 通知端口。
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait NotificationSink: MaybeSend {
    /// 是否在前台。
    fn is_foreground(&self) -> bool;

    /// 发一条通知。前台时实现可以直接丢弃（核心已经判断过，这里是兜底）。
    ///
    /// # Errors
    /// 见 [`NotificationError`]。
    async fn notify(&self, notification: Notification) -> Result<(), NotificationError>;
}
