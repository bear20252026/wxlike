//! wxlike-core: message types, E2EE scaffolding, sqlite storage.

pub mod crypto;
pub mod e2ee;
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
