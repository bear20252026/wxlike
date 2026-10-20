//! wxlike-core: message types, E2EE scaffolding, sqlite storage.

pub mod crypto;
pub mod e2ee;
pub mod msgmeta;
pub mod outbox;
pub mod store;
pub mod types;

pub use store::{Conversation, Store, StoredMessage};
pub use types::{
    custom_kind, json_bytes, now_ms, Envelope, MsgBody, MsgId, MsgKind, Receipt, NO_MSG_ID,
    PROTOCOL_VERSION,
};
