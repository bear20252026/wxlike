//! wxlike-core: message types, E2EE, sqlite storage.

pub mod e2ee;
/// 共享错误基础设施（`#[doc(hidden)]`）：存储层共享变体 + 构造函数。
#[doc(hidden)]
pub mod error;
/// 数值约定（实现在零依赖的 `wxlike-limits`，这里只做转发）。
pub use wxlike_limits as limits;
pub mod msgmeta;
pub mod outbox;
pub mod retry;
pub mod store;
pub mod types;

pub use store::{Conversation, Store, StoredMessage};
pub use types::{
    custom_kind, json_bytes, now_ms, Envelope, MsgBody, MsgId, MsgKind, Receipt, NO_MSG_ID,
    PROTOCOL_VERSION,
};
