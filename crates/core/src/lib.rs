//! wxlike-core: message types, E2EE scaffolding, sqlite storage.

pub mod crypto;
pub mod store;
pub mod types;

pub use types::{Envelope, MsgBody, MsgKind};
