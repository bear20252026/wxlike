//! Shared platform-capability contract for file picking, screen capture,
//! microphone recording and downloads.
//!
//! This module defines **only** the request/result/permission vocabulary and the
//! safety state machine. The real OS bridges belong to G10 (desktop) and G12
//! (mobile); they consume the same fixtures, so a web client can never claim a
//! capability the platform does not have.
//!
//! Hard rules encoded here:
//! * web screen capture and file picking need an explicit user gesture;
//! * web downloads never accept an arbitrary filesystem path;
//! * a recording session is registered while it runs, so stopping or canceling
//!   it clears the microphone indicator;
//! * raw camera frames and microphone audio are never uploaded implicitly —
//!   `Recorded` and `Captured` carry an `upload_allowed` flag that starts false.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{MediaError, Result};
use crate::ids;
use crate::{GIB, MIB};

pub const SCREEN_MAX_BYTES: u64 = 50 * MIB;
pub const PICK_MAX_BYTES: u64 = GIB;
pub const AUDIO_MAX_MS: u32 = 60 * 60 * 1000;
pub const AUDIO_SESSION_PREFIX: &str = "mic";
pub const WEB_CACHE_FRACTION_DEN: u64 = 10;
pub const DESKTOP_CACHE_FRACTION_DEN: u64 = 5;
pub const MOBILE_CACHE_FRACTION_DEN: u64 = 8;
pub const DERIVED_SHARE_DEN: u64 = 2;
pub const STAGING_SHARE_DEN: u64 = 10;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlatformKind {
    Web,
    Desktop,
    Mobile,
}

impl PlatformKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Desktop => "desktop",
            Self::Mobile => "mobile",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "web" => Ok(Self::Web),
            "desktop" => Ok(Self::Desktop),
            "mobile" => Ok(Self::Mobile),
            other => Err(MediaError::bad(format!("unknown platform {other:?}"))),
        }
    }

    pub fn capabilities(self) -> PlatformCapabilities {
        match self {
            Self::Web => PlatformCapabilities {
                pick_file: true,
                capture_screen: true,
                record_audio: true,
                save_download: true,
                requires_user_gesture: true,
                allows_arbitrary_save_path: false,
            },
            Self::Desktop => PlatformCapabilities {
                pick_file: true,
                capture_screen: true,
                record_audio: true,
                save_download: true,
                requires_user_gesture: false,
                allows_arbitrary_save_path: false,
            },
            Self::Mobile => PlatformCapabilities {
                pick_file: true,
                capture_screen: true,
                record_audio: true,
                save_download: true,
                requires_user_gesture: true,
                allows_arbitrary_save_path: false,
            },
        }
    }

    fn cache_fraction_den(self) -> u64 {
        match self {
            Self::Web => WEB_CACHE_FRACTION_DEN,
            Self::Desktop => DESKTOP_CACHE_FRACTION_DEN,
            Self::Mobile => MOBILE_CACHE_FRACTION_DEN,
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct PlatformCapabilities {
    pub pick_file: bool,
    pub capture_screen: bool,
    pub record_audio: bool,
    pub save_download: bool,
    pub requires_user_gesture: bool,
    pub allows_arbitrary_save_path: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "capability", rename_all = "snake_case")]
pub enum PlatformRequestKind {
    PickFile {
        #[serde(default)]
        filters: Vec<String>,
        #[serde(default)]
        max_bytes: u64,
    },
    CaptureScreen {
        #[serde(default)]
        interactive: bool,
    },
    RecordAudio {
        #[serde(default)]
        max_ms: u32,
    },
    SaveDownload {
        suggested_name: String,
        #[serde(default)]
        bytes: u64,
    },
    StopAudio {
        session_id: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PlatformRequest {
    pub request_id: String,
    #[serde(default)]
    pub user_gesture: bool,
    #[serde(flatten)]
    pub kind: PlatformRequestKind,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    User,
    Timeout,
    Replaced,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PlatformResult {
    Picked {
        files: Vec<PickedFile>,
    },
    Captured {
        asset_id: String,
        bytes: u64,
        mime: String,
        upload_allowed: bool,
    },
    Recorded {
        session_id: String,
        max_ms: u32,
        upload_allowed: bool,
    },
    Stopped {
        session_id: String,
    },
    Saved {
        destination: SaveDestination,
    },
    Canceled {
        reason: CancelReason,
    },
    PermissionDenied {
        capability: String,
    },
    DeviceDisconnected {
        capability: String,
    },
    Unsupported {
        capability: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PickedFile {
    pub name: String,
    pub bytes: u64,
    pub mime: String,
    pub asset_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SaveDestination {
    BrowserDownload { suggested_name: String },
    Dialog { path_token: String },
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct CacheBudget {
    pub platform: PlatformKind,
    pub total_bytes: u64,
    pub derived_bytes: u64,
    pub download_bytes: u64,
    pub upload_staging_bytes: u64,
}

impl CacheBudget {
    /// The Rust policy owns the split; clients may not pass their own numbers.
    pub fn for_device(platform: PlatformKind, available_bytes: u64) -> Self {
        let total = available_bytes / platform.cache_fraction_den();
        Self {
            platform,
            total_bytes: total,
            derived_bytes: total / DERIVED_SHARE_DEN,
            upload_staging_bytes: total / STAGING_SHARE_DEN,
            download_bytes: total
                .saturating_sub(total / DERIVED_SHARE_DEN)
                .saturating_sub(total / STAGING_SHARE_DEN),
        }
    }

    pub fn fits(&self) -> bool {
        self.derived_bytes + self.download_bytes + self.upload_staging_bytes <= self.total_bytes
    }
}

struct AudioSession {
    user_id: String,
    max_ms: u32,
}

/// Contract-level state machine. It does not touch a microphone; it records
/// which sessions are open so the UI indicator can be driven from one truth.
pub struct PlatformBroker {
    sessions: Mutex<HashMap<String, AudioSession>>,
}

impl Default for PlatformBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformBroker {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, AudioSession>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn authorize(
        &self,
        platform: PlatformKind,
        user_id: &str,
        request: &PlatformRequest,
    ) -> Result<PlatformResult> {
        if request.request_id.is_empty() || request.request_id.len() > ids::MAX_NAME_CHARS {
            return Err(MediaError::bad("request id is required"));
        }
        let caps = platform.capabilities();
        let capability = capability_name(&request.kind);
        if !supported(&caps, &request.kind) {
            return Ok(PlatformResult::Unsupported { capability });
        }
        if caps.requires_user_gesture && !request.user_gesture {
            return Ok(PlatformResult::PermissionDenied { capability });
        }
        match &request.kind {
            PlatformRequestKind::PickFile { max_bytes, .. } => {
                let cap = pick_cap(*max_bytes);
                if cap == 0 {
                    return Err(MediaError::PayloadTooLarge);
                }
                Ok(PlatformResult::Picked { files: Vec::new() })
            }
            PlatformRequestKind::CaptureScreen { interactive } => {
                self.capture(platform, *interactive)
            }
            PlatformRequestKind::RecordAudio { max_ms } => self.start_audio(user_id, *max_ms),
            PlatformRequestKind::SaveDownload { suggested_name, .. } => {
                Ok(save_result(platform, suggested_name)?)
            }
            PlatformRequestKind::StopAudio { session_id } => self.stop_audio(user_id, session_id),
        }
    }

    fn capture(&self, platform: PlatformKind, interactive: bool) -> Result<PlatformResult> {
        if platform == PlatformKind::Web && !interactive {
            return Ok(PlatformResult::PermissionDenied {
                capability: "capture_screen".into(),
            });
        }
        Ok(PlatformResult::Captured {
            asset_id: ids::opaque_id("screen"),
            bytes: 0,
            mime: "image/png".into(),
            upload_allowed: false,
        })
    }

    fn start_audio(&self, user_id: &str, max_ms: u32) -> Result<PlatformResult> {
        let cap = if max_ms == 0 {
            AUDIO_MAX_MS
        } else {
            max_ms.min(AUDIO_MAX_MS)
        };
        let session_id = ids::opaque_id(AUDIO_SESSION_PREFIX);
        self.sessions().insert(
            session_id.clone(),
            AudioSession {
                user_id: user_id.to_string(),
                max_ms: cap,
            },
        );
        Ok(PlatformResult::Recorded {
            session_id,
            max_ms: cap,
            upload_allowed: false,
        })
    }

    fn stop_audio(&self, user_id: &str, session_id: &str) -> Result<PlatformResult> {
        let removed = self.sessions().remove(session_id);
        match removed {
            Some(session) if session.user_id == user_id && session.max_ms > 0 => {
                Ok(PlatformResult::Stopped {
                    session_id: session_id.to_string(),
                })
            }
            _ => Err(MediaError::NotFound),
        }
    }

    /// Whether any microphone session is currently open. The UI indicator is
    /// required to follow this value, so a stop/cancel always clears it.
    pub fn indicator_active(&self) -> bool {
        !self.sessions().is_empty()
    }

    pub fn cancel_audio(&self, user_id: &str, session_id: &str) -> Result<PlatformResult> {
        let removed = self.sessions().remove(session_id);
        match removed {
            Some(session) if session.user_id == user_id => Ok(PlatformResult::Canceled {
                reason: CancelReason::User,
            }),
            _ => Err(MediaError::NotFound),
        }
    }
}

fn capability_name(kind: &PlatformRequestKind) -> String {
    match kind {
        PlatformRequestKind::PickFile { .. } => "pick_file",
        PlatformRequestKind::CaptureScreen { .. } => "capture_screen",
        PlatformRequestKind::RecordAudio { .. } => "record_audio",
        PlatformRequestKind::SaveDownload { .. } => "save_download",
        PlatformRequestKind::StopAudio { .. } => "stop_audio",
    }
    .into()
}

fn supported(caps: &PlatformCapabilities, kind: &PlatformRequestKind) -> bool {
    match kind {
        PlatformRequestKind::PickFile { .. } => caps.pick_file,
        PlatformRequestKind::CaptureScreen { .. } => caps.capture_screen,
        PlatformRequestKind::RecordAudio { .. } | PlatformRequestKind::StopAudio { .. } => {
            caps.record_audio
        }
        PlatformRequestKind::SaveDownload { .. } => caps.save_download,
    }
}

fn pick_cap(requested: u64) -> u64 {
    if requested == 0 {
        PICK_MAX_BYTES
    } else {
        requested.min(PICK_MAX_BYTES)
    }
}

fn save_result(platform: PlatformKind, suggested_name: &str) -> Result<PlatformResult> {
    let name = ids::validate_name(suggested_name)?;
    if name.contains('/') {
        return Err(MediaError::bad("name must not be a path"));
    }
    let destination = match platform {
        PlatformKind::Web => SaveDestination::BrowserDownload {
            suggested_name: name,
        },
        _ => SaveDestination::Dialog {
            path_token: ids::opaque_id("save"),
        },
    };
    Ok(PlatformResult::Saved { destination })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(kind: PlatformRequestKind) -> PlatformRequest {
        PlatformRequest {
            request_id: "req-1".into(),
            user_gesture: true,
            kind,
        }
    }

    #[test]
    fn web_cannot_capture_silently() {
        let broker = PlatformBroker::new();
        let silent = PlatformRequest {
            user_gesture: false,
            ..request(PlatformRequestKind::CaptureScreen { interactive: true })
        };
        assert!(matches!(
            broker.authorize(PlatformKind::Web, "u1", &silent).unwrap(),
            PlatformResult::PermissionDenied { .. }
        ));
        let no_picker = request(PlatformRequestKind::CaptureScreen { interactive: false });
        assert!(matches!(
            broker
                .authorize(PlatformKind::Web, "u1", &no_picker)
                .unwrap(),
            PlatformResult::PermissionDenied { .. }
        ));
    }

    #[test]
    fn mic_indicator_clears_on_stop_and_cancel() {
        let broker = PlatformBroker::new();
        let started = broker
            .authorize(
                PlatformKind::Desktop,
                "u1",
                &request(PlatformRequestKind::RecordAudio { max_ms: 1000 }),
            )
            .unwrap();
        let PlatformResult::Recorded {
            session_id,
            upload_allowed,
            ..
        } = started
        else {
            panic!("expected a recording");
        };
        assert!(!upload_allowed);
        assert!(broker.indicator_active());
        broker
            .authorize(
                PlatformKind::Desktop,
                "u1",
                &request(PlatformRequestKind::StopAudio {
                    session_id: session_id.clone(),
                }),
            )
            .unwrap();
        assert!(!broker.indicator_active());

        let again = broker
            .authorize(
                PlatformKind::Desktop,
                "u1",
                &request(PlatformRequestKind::RecordAudio { max_ms: 0 }),
            )
            .unwrap();
        let PlatformResult::Recorded { session_id, .. } = again else {
            panic!("expected a recording");
        };
        broker.cancel_audio("u1", &session_id).unwrap();
        assert!(!broker.indicator_active());
    }

    #[test]
    fn web_save_never_returns_a_path() {
        let broker = PlatformBroker::new();
        let saved = broker
            .authorize(
                PlatformKind::Web,
                "u1",
                &request(PlatformRequestKind::SaveDownload {
                    suggested_name: "report.pdf".into(),
                    bytes: 10,
                }),
            )
            .unwrap();
        assert!(matches!(
            saved,
            PlatformResult::Saved {
                destination: SaveDestination::BrowserDownload { .. }
            }
        ));
    }

    #[test]
    fn cache_budget_split_is_bounded() {
        let budget = CacheBudget::for_device(PlatformKind::Desktop, 10 * GIB);
        assert!(budget.fits());
        assert!(budget.total_bytes <= 10 * GIB);
    }
}
