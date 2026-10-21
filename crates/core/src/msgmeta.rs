//! P2 rich message payload types (contract §3.6): voice notes, files,
//! stickers and video messages.
//!
//! Everything rides the existing `MsgBody::Custom` envelope — the wire never
//! grows a new variant — so the protocol, the hub, storage and dedup treat
//! these exactly like any other content frame. What this module owns is the
//! **payload schema**: typed serde structs, their validation, and the one
//! real algorithm (waveform downsampling for voice notes).
//!
//! Binary media never travels inside these payloads: every type references
//! stored media by SHA-256 id (WP-05), keeping the message gateway
//! byte-free. Video additionally references a video-service object by id
//! (ADR-21: video binaries never pass the message gateway).

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

/// `MsgBody::Custom` kinds for the P2 message types.
pub mod rich_kind {
    pub const VOICE: &str = "msg.voice";
    pub const FILE: &str = "msg.file";
    pub const STICKER: &str = "msg.sticker";
    pub const VIDEO: &str = "msg.video";

    /// All P2 kinds, for registry-style iteration (tests, UI dispatch).
    pub const ALL: [&str; 4] = [VOICE, FILE, STICKER, VIDEO];
}

/// Field limits shared by the payload validators.
pub mod limits {
    /// Voice waveform bucket count after downsampling (display resolution).
    pub const WAVEFORM_BARS: usize = 40;
    /// Sticker pack / id are short labels.
    pub const STICKER_LABEL_MAX: usize = 64;
    /// File display name.
    pub const FILE_NAME_MAX: usize = 255;
}

// ---------------------------------------------------------------------------
// voice note
// ---------------------------------------------------------------------------

/// A recorded voice note. `asr` is the **占位** transcription slot: always
/// `None` until a speech-to-text provider is wired (P4 转文字), but present in
/// the schema so clients can render the row today.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct VoicePayload {
    pub media_id: String,
    /// Duration in milliseconds (1 ms ..= 10 min).
    pub duration_ms: u32,
    /// Display waveform, `WAVEFORM_BARS` values in `0..=100` (relative loudness).
    pub waveform: Vec<u8>,
    /// Speech-to-text placeholder: `None` until a provider exists (P4).
    #[serde(default)]
    pub asr: Option<String>,
}

impl VoicePayload {
    pub fn validate(&self) -> Result<(), String> {
        if self.media_id.len() != crate::limits::SHA256_HEX_LEN
            || !self.media_id.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err("voice media_id must be a sha256 hex".into());
        }
        if self.duration_ms == 0 || self.duration_ms > 10 * 60 * 1000 {
            return Err("voice duration_ms out of range".into());
        }
        if self.waveform.len() != limits::WAVEFORM_BARS {
            return Err(format!(
                "waveform must have exactly {} bars",
                limits::WAVEFORM_BARS
            ));
        }
        if self.waveform.iter().any(|b| *b > 100) {
            return Err("waveform bars are relative loudness in 0..=100".into());
        }
        if let Some(asr) = &self.asr {
            if asr.len() > 2000 {
                return Err("asr text too long".into());
            }
        }
        Ok(())
    }

    /// Downsamples raw loudness samples (any length, values `0..=255`) to the
    /// display waveform.
    ///
    /// Peak-preserving per bucket (a burst inside a bucket must stay visible
    /// or a two-syllable word renders as one blob), then normalized to
    /// `0..=100` against the loudest bucket so quiet notes still show shape.
    /// Silence renders as a low flat line, never as division-by-zero NaNs.
    pub fn waveform_from_samples(samples: &[u8]) -> Vec<u8> {
        let bars = limits::WAVEFORM_BARS;
        if samples.is_empty() {
            return vec![0; bars];
        }
        let mut peaks = vec![0u8; bars];
        for (i, s) in samples.iter().enumerate() {
            let bucket = i * bars / samples.len();
            if *s > peaks[bucket] {
                peaks[bucket] = *s;
            }
        }
        let max = u32::from(*peaks.iter().max().unwrap_or(&0));
        if max == 0 {
            return peaks; // all-silence input: zeros, no scaling needed
        }
        peaks
            .iter()
            .map(|p| (u32::from(*p) * 100).div_ceil(max).max(1) as u8)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// file message
// ---------------------------------------------------------------------------

/// Any-type file (contract §3.6: 断点续传 lands with the P6 storage layer;
/// today the media store's HTTP Range support already serves partial reads).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FilePayload {
    pub media_id: String,
    /// Display name, no path components allowed (it is rendered, never joined).
    pub name: String,
    /// Exact byte size of the stored object; a mismatch means the reference
    /// is corrupt and clients must refuse to serve it.
    pub size: u64,
    #[serde(default)]
    pub sha256: String,
}

impl FilePayload {
    pub fn validate(&self) -> Result<(), String> {
        if self.media_id.len() != crate::limits::SHA256_HEX_LEN
            || !self.media_id.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err("file media_id must be a sha256 hex".into());
        }
        let name = self.name.trim();
        if name.is_empty() {
            return Err("file name must not be empty".into());
        }
        if name.chars().count() > limits::FILE_NAME_MAX {
            return Err("file name too long".into());
        }
        // rendered as text, never used as a path: refuse separators outright
        if name.contains(['/', '\\']) || name.contains("..") {
            return Err("file name must not contain path separators".into());
        }
        if self.size == 0 {
            return Err("file size must be positive".into());
        }
        if !self.sha256.is_empty()
            && (self.sha256.len() != crate::limits::SHA256_HEX_LEN
                || !self.sha256.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err("file sha256 must be empty or sha256 hex".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// sticker
// ---------------------------------------------------------------------------

/// A sticker: a small image in a named pack. The image itself is a media
/// object (`media_id`), so packs are just naming over content addressing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StickerPayload {
    pub media_id: String,
    pub pack: String,
    pub id: String,
}

impl StickerPayload {
    pub fn validate(&self) -> Result<(), String> {
        if self.media_id.len() != crate::limits::SHA256_HEX_LEN
            || !self.media_id.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err("sticker media_id must be a sha256 hex".into());
        }
        for label in [&self.pack, &self.id] {
            if label.is_empty() || label.chars().count() > limits::STICKER_LABEL_MAX {
                return Err("sticker pack/id must be 1..=64 chars".into());
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// video message
// ---------------------------------------------------------------------------

/// A video message: a reference into video-service (ADR-21). The HLS
/// playlist and thumbnail live behind video-service; the message only names
/// them, so a re-transcode never rewrites history.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct VideoPayload {
    /// video-service object id (sha256 of the source upload).
    pub video_id: String,
    /// Playlist URL path (relative to the server base), e.g.
    /// `/api/video/{id}/playlist.m3u8`.
    pub playlist_url: String,
    /// Progressive MP4 for native `<video>` playback + 倍速 (browsers cannot
    /// read HLS directly). Older senders may omit it; receivers fall back to
    /// the playlist (external player) then.
    #[serde(default)]
    pub play_url: String,
    /// Thumbnail media id (regular media store object).
    pub thumb_media_id: String,
    /// Duration in milliseconds, from ffprobe.
    pub duration_ms: u32,
    /// playback rates the sender's UI offers (e.g. [1.0, 1.5, 2.0]); the
    /// receiver is free to render its own set. Empty = sender default.
    #[serde(default)]
    pub speeds: Vec<f32>,
}

impl VideoPayload {
    /// Default speed ladder (倍速).
    pub const DEFAULT_SPEEDS: [f32; 4] = [0.5, 1.0, 1.5, 2.0];

    pub fn validate(&self) -> Result<(), String> {
        if self.video_id.len() != crate::limits::SHA256_HEX_LEN
            || !self.video_id.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err("video_id must be a sha256 hex".into());
        }
        if !self.playlist_url.starts_with("/api/video/")
            || !self.playlist_url.ends_with(".m3u8")
            || self.playlist_url.contains("..")
        {
            return Err("playlist_url must be a /api/video/{id}/playlist.m3u8 path".into());
        }
        if !self.play_url.is_empty()
            && (!self.play_url.starts_with("/api/video/")
                || !self.play_url.ends_with(".mp4")
                || self.play_url.contains(".."))
        {
            return Err("play_url must be a /api/video/{id}/play.mp4 path".into());
        }
        // empty is legal until the thumbnail is mirrored into the media store
        // (P2: it lives in video-service; P5 mirrors it for inline grids)
        if !self.thumb_media_id.is_empty()
            && (self.thumb_media_id.len() != crate::limits::SHA256_HEX_LEN
                || !self.thumb_media_id.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err("thumb_media_id must be empty or a sha256 hex".into());
        }
        if self.duration_ms == 0 {
            return Err("duration_ms must be positive".into());
        }
        if self.speeds.iter().any(|s| !(0.1..=4.0).contains(s)) {
            return Err("speeds are playback rates in 0.1..=4.0".into());
        }
        Ok(())
    }
}

/// sha256 hex of some bytes — the id every payload references by.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_payload_validation() {
        let media = sha256_hex(b"audio");
        let bars = limits::WAVEFORM_BARS;
        let ok = VoicePayload {
            media_id: media.clone(),
            duration_ms: 3200,
            waveform: vec![10; bars],
            asr: None,
        };
        assert!(ok.validate().is_ok());

        let mut bad = ok.clone();
        bad.duration_ms = 0;
        assert!(bad.validate().is_err());
        bad.duration_ms = 11 * 60 * 1000;
        assert!(bad.validate().is_err());

        bad = ok.clone();
        bad.waveform = vec![10; bars - 1];
        assert!(bad.validate().is_err());
        bad.waveform = vec![101; bars];
        assert!(bad.validate().is_err());

        bad = ok;
        bad.media_id = "../../etc/passwd".into();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn waveform_downsampling_is_peak_preserving_and_normalized() {
        // one loud spike inside a quiet sea must survive downsampling
        let mut samples = vec![5u8; 1000];
        samples[500] = 250;
        let bars = VoicePayload::waveform_from_samples(&samples);
        assert_eq!(bars.len(), limits::WAVEFORM_BARS);
        assert_eq!(*bars.iter().max().unwrap(), 100, "loudest bucket = 100");
        assert!(
            bars.iter().any(|b| *b >= 90),
            "the spike's bucket keeps its peak"
        );
        // every other bar is tiny but positive (shape, not flatline)
        assert!(bars.iter().filter(|b| **b <= 5).count() >= 30);

        // all-silence stays zeros
        assert_eq!(
            VoicePayload::waveform_from_samples(&[0; 999]),
            vec![0; limits::WAVEFORM_BARS]
        );
        // empty input is safe
        assert_eq!(
            VoicePayload::waveform_from_samples(&[]),
            vec![0; limits::WAVEFORM_BARS]
        );
        // fewer samples than bars still fills every bucket exactly once
        let short = VoicePayload::waveform_from_samples(&[200, 100, 50]);
        assert_eq!(short.len(), limits::WAVEFORM_BARS);
        assert_eq!(short.iter().filter(|b| **b == 100).count(), 1);
    }

    #[test]
    fn file_payload_rejects_path_shaped_names() {
        let media = sha256_hex(b"file");
        let ok = FilePayload {
            media_id: media.clone(),
            name: "季度报表.pdf".into(),
            size: 123,
            sha256: String::new(),
        };
        assert!(ok.validate().is_ok());

        for name in ["../etc/passwd", "a/b", "a\\b", "", "  "] {
            let bad = FilePayload {
                media_id: media.clone(),
                name: name.into(),
                size: 1,
                sha256: String::new(),
            };
            assert!(bad.validate().is_err(), "name {name:?} must be refused");
        }

        let bad = FilePayload {
            media_id: media.clone(),
            name: "f.bin".into(),
            size: 0,
            sha256: String::new(),
        };
        assert!(bad.validate().is_err(), "zero-size reference is corrupt");

        // sha256 present must be well-formed
        let bad = FilePayload {
            media_id: sha256_hex(b"x"),
            name: "f.bin".into(),
            size: 1,
            sha256: "zz".into(),
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn sticker_payload_validation() {
        let ok = StickerPayload {
            media_id: sha256_hex(b"sticker"),
            pack: "cats".into(),
            id: "happy-01".into(),
        };
        assert!(ok.validate().is_ok());
        let bad = StickerPayload {
            pack: String::new(),
            ..ok.clone()
        };
        assert!(bad.validate().is_err());
        let bad = StickerPayload {
            id: "x".repeat(65),
            ..ok
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn video_payload_validation() {
        let ok = VideoPayload {
            video_id: sha256_hex(b"video"),
            playlist_url: "/api/video/abc/playlist.m3u8".into(),
            play_url: "/api/video/abc/play.mp4".into(),
            thumb_media_id: sha256_hex(b"thumb"),
            duration_ms: 42_000,
            speeds: vec![0.5, 1.0, 2.0],
        };
        assert!(ok.validate().is_ok());

        let mut bad = ok.clone();
        bad.playlist_url = "https://evil.example/x.m3u8".into();
        assert!(
            bad.validate().is_err(),
            "playlist must stay on our api path"
        );
        bad.playlist_url = "/api/video/../admin/playlist.m3u8".into();
        assert!(bad.validate().is_err(), "traversal in the path is refused");
        bad = ok.clone();
        bad.play_url = "https://evil.example/x.mp4".into();
        assert!(
            bad.validate().is_err(),
            "play_url must stay on our api path"
        );
        bad = ok.clone();
        bad.speeds = vec![9.0];
        assert!(bad.validate().is_err());
        bad = ok;
        bad.duration_ms = 0;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn payloads_round_trip_through_json() {
        // the payload travels as JSON text inside a Custom frame; the shape
        // must survive a serde_json round trip unchanged
        let voice = VoicePayload {
            media_id: sha256_hex(b"audio"),
            duration_ms: 1500,
            waveform: VoicePayload::waveform_from_samples(&[10, 200, 30]),
            asr: None,
        };
        let json = serde_json::to_string(&voice).unwrap();
        let back: VoicePayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, voice);

        // asr is optional on the wire: old senders may omit it
        let minimal = serde_json::json!({
            "media_id": sha256_hex(b"audio"),
            "duration_ms": 1500,
            "waveform": vec![1; limits::WAVEFORM_BARS],
        });
        let parsed: VoicePayload = serde_json::from_value(minimal).unwrap();
        assert_eq!(parsed.asr, None);

        // default speed ladder is a sane constant
        assert!(VideoPayload::DEFAULT_SPEEDS.contains(&1.0));
    }
}
