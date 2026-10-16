use serde::{Deserialize, Serialize};

/// Wire envelope. Versioned so the protocol can evolve independently of storage.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Envelope {
    pub version: u8,
    pub conv_id: String,
    pub sender: String,
    /// Recipient user ids. Empty = conversation broadcast (delivery to all
    /// conversation members except the sender, once membership exists).
    #[serde(default)]
    pub to: Vec<String>,
    pub seq: u64,
    pub kind: MsgKind,
    pub body: MsgBody,
    /// millis since epoch, set by sender
    pub sent_at_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgKind {
    Text,
    Blob,
    System,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum MsgBody {
    Text(String),
    Bytes(Vec<u8>),
    Notice(String),
}

impl Envelope {
    /// Conversation broadcast (no explicit recipients).
    pub fn text(conv_id: impl Into<String>, sender: impl Into<String>, seq: u64, text: impl Into<String>) -> Self {
        Self::direct(conv_id, sender, Vec::new(), seq, text)
    }

    /// Directed message: `to` lists recipient user ids.
    pub fn direct(
        conv_id: impl Into<String>,
        sender: impl Into<String>,
        to: Vec<String>,
        seq: u64,
        text: impl Into<String>,
    ) -> Self {
        Self {
            version: 1,
            conv_id: conv_id.into(),
            sender: sender.into(),
            to,
            seq,
            kind: MsgKind::Text,
            body: MsgBody::Text(text.into()),
            sent_at_ms: now_ms(),
        }
    }

    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(bincode::serialize(self)?)
    }

    pub fn from_bytes(b: &[u8]) -> anyhow::Result<Self> {
        Ok(bincode::deserialize(b)?)
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrip() {
        let e = Envelope::text("conv1", "alice", 7, "hello");
        let b = e.to_bytes().unwrap();
        assert_eq!(Envelope::from_bytes(&b).unwrap(), e);
    }
}
