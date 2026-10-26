//! 运行时：会话、补发泵、事件订阅。
//!
//! 这一层是**唯一**知道"命令怎么变成请求、失败怎么重试、事件怎么落盘"的地方。
//! UI（React / Flutter）只做两件事：`dispatch` 与 `subscribe`——它不再自己
//! 持有消息列表的第二份真相，也不再自己决定重试。
//!
//! 三个子模块：
//!
//! * [`backoff`]：指数退避 + 抖动，全仓库一份；
//! * [`router`]：命令 id → HTTP 路径，全仓库一份；
//! * [`session`]：单实例会话 + 先落盘后回话 + 补发泵；
//! * [`subscription`]：先落盘后交付的事件流。

pub mod backoff;
pub mod router;
pub mod session;
pub mod subscription;

pub use backoff::Backoff;
pub use router::{composite, is_composite, route, Route, ROUTES};
pub use session::{is_write_action, PumpReport, Session, SessionError};
pub use subscription::EventSubscription;

/// catalog 里的全部命令 id（能力上报用，见 `client-web` 的 wasm 出口）。
#[must_use]
pub fn action_ids() -> Vec<&'static str> {
    wxlike_cmd::CATALOG.iter().map(|spec| spec.id).collect()
}
