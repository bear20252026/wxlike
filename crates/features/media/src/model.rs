//! Shared media vocabulary. Domain values have one spelling on the wire, in
//! SQLite and in logs; the `parse` functions refuse unknown strings instead of
//! silently mapping them to a default.

use serde::{Deserialize, Serialize};

use crate::error::{MediaError, Result};

pub const MIB: u64 = 1024 * 1024;
pub const IMAGE_MAX_BYTES: u64 = 20 * MIB;
pub const STICKER_MAX_BYTES: u64 = 5 * MIB;
pub const VOICE_MAX_BYTES: u64 = 20 * MIB;
pub const VOICE_MAX_MS: u32 = 10 * 60 * 1000;
pub const DEFAULT_PAGE_LIMIT: u32 = 50;
pub const MAX_PAGE_LIMIT: u32 = 100;
pub const HEIGHT_360: u32 = 360;
pub const HEIGHT_720: u32 = 720;
pub const HEIGHT_1080: u32 = 1080;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Attachment,
    Avatar,
    Sticker,
    Voice,
    Video,
    File,
    Backup,
    Derived,
}

/// File-center bucket for an upload. Purpose wins over MIME sniffing so a
/// voice note stays a voice note even when stored as `audio/ogg`.
pub fn category_for(purpose: Purpose, mime: &str) -> &'static str {
    match purpose {
        Purpose::Voice => "voice",
        Purpose::Sticker => "sticker",
        Purpose::Video => "video",
        _ if mime.starts_with("image/") => "image",
        _ if mime.starts_with("audio/") => "audio",
        _ if mime.starts_with("video/") => "video",
        _ => "file",
    }
}

impl Purpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Attachment => "attachment",
            Self::Avatar => "avatar",
            Self::Sticker => "sticker",
            Self::Voice => "voice",
            Self::Video => "video",
            Self::File => "file",
            Self::Backup => "backup",
            Self::Derived => "derived",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "attachment" => Ok(Self::Attachment),
            "avatar" => Ok(Self::Avatar),
            "sticker" => Ok(Self::Sticker),
            "voice" => Ok(Self::Voice),
            "video" => Ok(Self::Video),
            "file" => Ok(Self::File),
            "backup" => Ok(Self::Backup),
            "derived" => Ok(Self::Derived),
            other => Err(MediaError::bad(format!("unknown purpose {other:?}"))),
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            Self::Avatar | Self::Attachment | Self::File | Self::Backup => "file",
            Self::Sticker => "sticker",
            Self::Voice => "voice",
            Self::Video => "video",
            Self::Derived => "derived",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RefType {
    Message,
    File,
    Post,
    Avatar,
    Backup,
    Derived,
}

impl RefType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::File => "file",
            Self::Post => "post",
            Self::Avatar => "avatar",
            Self::Backup => "backup",
            Self::Derived => "derived",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "message" => Ok(Self::Message),
            "file" => Ok(Self::File),
            "post" => Ok(Self::Post),
            "avatar" => Ok(Self::Avatar),
            "backup" => Ok(Self::Backup),
            "derived" => Ok(Self::Derived),
            other => Err(MediaError::bad(format!("unknown ref type {other:?}"))),
        }
    }
}

/// Who may read an object through a logical reference. Conversation/group
/// membership is resolved by an injected port (G02/G03); this crate never
/// guesses it from an id string.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Audience {
    User { id: String },
    Conversation { id: String },
    Group { id: String },
    Public,
}

impl Audience {
    pub fn key(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn requires_remote_check(&self) -> bool {
        matches!(self, Self::Conversation { .. } | Self::Group { .. })
    }

    pub fn id(&self) -> &str {
        match self {
            Self::User { id } | Self::Conversation { id } | Self::Group { id } => id,
            Self::Public => "",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ObjectRecord {
    pub object_id: String,
    pub owner: String,
    pub size: u64,
    pub digest: String,
    pub mime: String,
    pub status: String,
    pub access_epoch: i64,
    pub created_at: i64,
}

impl ObjectRecord {
    pub fn ready(&self) -> bool {
        self.status == "ready"
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FileItem {
    pub file_id: String,
    pub object_id: String,
    pub owner: String,
    pub name: String,
    pub folder: String,
    pub category: String,
    pub size: u64,
    pub digest: String,
    pub mime: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UploadStatus {
    Open,
    Completed,
    Canceled,
    Expired,
}

impl UploadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
            Self::Expired => "expired",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "open" => Ok(Self::Open),
            "completed" => Ok(Self::Completed),
            "canceled" => Ok(Self::Canceled),
            "expired" => Ok(Self::Expired),
            other => Err(MediaError::internal(format!(
                "unknown upload state {other:?}"
            ))),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Canceled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        // Arm order differs from `wxlike-video`'s state machine on purpose:
        // the two spellings overlap by design and must not drift silently.
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Pending => "pending",
            Self::Running => "running",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            other => Err(MediaError::internal(format!("unknown job state {other:?}"))),
        }
    }

    pub fn terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Canceled)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    Thumbnail,
    Waveform,
    Playback360,
    Playback720,
    Playback1080,
}

impl Preset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thumbnail => "thumbnail",
            Self::Waveform => "waveform",
            Self::Playback360 => "playback_360",
            Self::Playback720 => "playback_720",
            Self::Playback1080 => "playback_1080",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "thumbnail" => Ok(Self::Thumbnail),
            "waveform" => Ok(Self::Waveform),
            "playback_360" => Ok(Self::Playback360),
            "playback_720" => Ok(Self::Playback720),
            "playback_1080" => Ok(Self::Playback1080),
            other => Err(MediaError::bad(format!("unknown preset {other:?}"))),
        }
    }

    pub fn height(self) -> Option<u32> {
        match self {
            Self::Playback360 => Some(HEIGHT_360),
            Self::Playback720 => Some(HEIGHT_720),
            Self::Playback1080 => Some(HEIGHT_1080),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub kind: String,
    pub object_id: String,
    pub size: u64,
    pub digest: String,
    pub mime: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_enums_are_errors_not_defaults() {
        assert!(Purpose::parse("mystery").is_err());
        assert!(RefType::parse("mystery").is_err());
        assert!(JobState::parse("mystery").is_err());
        assert!(Preset::parse("mystery").is_err());
        assert_eq!(JobState::parse("running").unwrap(), JobState::Running);
    }

    #[test]
    fn audience_keys_are_stable_and_parseable() {
        let audience = Audience::Conversation { id: "d-a~b".into() };
        let key = audience.key();
        assert_eq!(
            serde_json::from_str::<Audience>(&key).unwrap(),
            Audience::Conversation { id: "d-a~b".into() }
        );
        assert!(audience.requires_remote_check());
        assert!(!Audience::Public.requires_remote_check());
    }

    #[test]
    fn limits_are_sane() {
        const { assert!(STICKER_MAX_BYTES < IMAGE_MAX_BYTES) };
        const { assert!(VOICE_MAX_BYTES <= IMAGE_MAX_BYTES) };
        const { assert!(DEFAULT_PAGE_LIMIT <= MAX_PAGE_LIMIT) };
        assert_eq!(Purpose::Voice.category(), "voice");
    }
}
