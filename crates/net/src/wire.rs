//! Control-plane frames sent on the client's control stream before envelopes.

use serde::{Deserialize, Serialize};
use wxlike_core::Envelope;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum WireFrame {
    /// First frame on a client's stream; registers `user_id` on the hub.
    Hello {
        user_id: String,
    },
    Msg(Envelope),
    /// Keepalive; also materializes the QUIC stream (lazy-open semantics).
    Ping,
}

impl WireFrame {
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(bincode::serialize(self)?)
    }

    pub fn from_bytes(b: &[u8]) -> anyhow::Result<Self> {
        Ok(bincode::deserialize(b)?)
    }
}
