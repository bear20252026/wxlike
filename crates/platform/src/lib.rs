//! `wxlike-platform` — the single business data plane: HTTP/JSON over SQLite.
//!
//! Design rules that shape this crate (see `docs/WORK_BREAKDOWN.md` §0.1):
//! all business logic lives here in Rust; the QUIC hub only routes and pushes.
//! Storage is one SQLite connection behind a mutex with parameterized SQL
//! everywhere; sessions are random 32-byte hex tokens with a TTL; passwords
//! use per-user salted PBKDF2-HMAC-SHA256.
//!
//! Each feature area is a module exposing `routes() -> Router<PlatformState>`,
//! merged by [`Platform::router`], so endpoints for accounts, friends, groups,
//! moments, media and wallet stay in separate files with one shared state.
//!
//! ```no_run
//! # fn main() -> anyhow::Result<()> {
//! let platform = wxlike_platform::Platform::in_memory()?.shared();
//! let app = wxlike_platform::Platform::router(platform.clone());
//! # let _ = app; Ok(()) }
//! ```

mod auth;
mod db;
mod error;
mod friends;
mod groups;
mod media;
mod users;
mod validate;
mod wallet;

use std::sync::Arc;

use axum::Router;
use rusqlite::{params, OptionalExtension as _};
use wxlike_core::{Envelope, MsgBody, MsgKind};

pub use crate::auth::{hash_password, random_hex, verify_password, Authed};
pub use crate::db::Db;
pub use crate::error::{ApiError, PlatformError, Result};
pub use crate::media::{LocalDirStore, MediaStore, StoredObject, MAX_MEDIA_BYTES, THUMB_MAX_EDGE};
pub use crate::users::{Credentials, Profile, ProfilePatch, PublicProfile};
pub use crate::wallet::{kind as wallet_kind, REDPACKET_TTL_MS};

/// Periodic refund of unclaimed red packets (WP-09). Started by `wxlike-server`;
/// tests call [`Platform::sweep_expired_redpackets`] directly so the expiry path
/// is deterministic instead of sleeping for a day.
pub fn spawn_redpacket_sweeper(
    state: PlatformState,
    every: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            match state.sweep_expired_redpackets() {
                Ok(0) => {}
                Ok(n) => println!("[platform] refunded {n} expired red packet(s)"),
                Err(err) => eprintln!("[platform] redpacket sweep failed: {err}"),
            }
        }
    })
}

/// A throwaway media root for an in-memory platform, so tests never touch the
/// caller's working directory.
fn temp_media_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-media-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Thirty days, as required by WP-01.
pub const DEFAULT_SESSION_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Clone, Debug)]
pub struct PlatformOptions {
    pub session_ttl_ms: u64,
    pub pbkdf2_rounds: u32,
    /// Upload cap enforced inside the handler. Defaults to the contract's
    /// 100 MB; tests lower it to exercise the same 413 path without allocating.
    pub max_media_bytes: usize,
}

impl Default for PlatformOptions {
    fn default() -> Self {
        Self {
            session_ttl_ms: DEFAULT_SESSION_TTL_MS,
            pbkdf2_rounds: auth::DEFAULT_ROUNDS,
            max_media_bytes: media::MAX_MEDIA_BYTES,
        }
    }
}

pub type PlatformState = Arc<Platform>;

/// Outbound system events (friend/group lifecycle) are pushed through the QUIC
/// hub so online peers see them end-to-end. The server wires a `HubHandle`
/// adapter in; tests wire a recorder. `None` = events are only persisted.
#[async_trait::async_trait]
pub trait EventSink: Send + Sync + 'static {
    async fn publish(&self, env: wxlike_core::Envelope) -> anyhow::Result<()>;
}

/// Deterministic 1:1 conversation id for a pair (order-independent).
pub fn direct_conv_id(a: &str, b: &str) -> String {
    let (mut x, mut y) = (a.to_string(), b.to_string());
    if x > y {
        std::mem::swap(&mut x, &mut y);
    }
    format!("d-{x}~{y}")
}

pub(crate) fn system_envelope(
    conv_id: &str,
    sender: &str,
    to: Vec<String>,
    text: &str,
) -> Envelope {
    Envelope {
        version: wxlike_core::PROTOCOL_VERSION,
        conv_id: conv_id.to_string(),
        sender: sender.to_string(),
        to,
        seq: 0,
        kind: MsgKind::System,
        body: MsgBody::Notice(text.to_string()),
        sent_at_ms: wxlike_core::types::now_ms(),
        // its own dedup id, so a retried event cannot double-notify
        msg_id: Envelope::new_msg_id(),
        quote_of: None,
        receipt: None,
    }
}

pub struct Platform {
    db: Db,
    options: PlatformOptions,
    sink: std::sync::Mutex<Option<Arc<dyn EventSink>>>,
    /// group_id -> member ids, invalidated on every membership change (WP-04).
    group_cache: std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>,
    /// swappable object store behind `/api/media` (WP-05).
    media: std::sync::Mutex<Arc<dyn media::MediaStore>>,
}

impl Platform {
    pub fn in_memory() -> Result<Self> {
        Self::in_memory_with(PlatformOptions::default())
    }

    pub fn in_memory_with(options: PlatformOptions) -> Result<Self> {
        Ok(Self {
            db: Db::open(None)?,
            options,
            sink: std::sync::Mutex::new(None),
            group_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            // an in-memory platform must not write into the working directory
            media: std::sync::Mutex::new(Arc::new(media::LocalDirStore::new(temp_media_dir()))),
        })
    }

    /// `None` path opens in-memory; `Some(file)` persists under `WXLIKE_DB`.
    pub fn open(path: Option<&str>) -> Result<Self> {
        Self::open_with(path, PlatformOptions::default())
    }

    pub fn open_with(path: Option<&str>, options: PlatformOptions) -> Result<Self> {
        Ok(Self {
            db: Db::open(path)?,
            options,
            sink: std::sync::Mutex::new(None),
            group_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            media: std::sync::Mutex::new(Arc::new(media::LocalDirStore::new(
                media::default_media_dir(),
            ))),
        })
    }

    pub fn shared(self) -> PlatformState {
        Arc::new(self)
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn options(&self) -> &PlatformOptions {
        &self.options
    }

    /// Issues a session token valid for `ttl_ms` (0 = already expired, used by
    /// tests to cover the expiry path without sleeping).
    pub fn create_session(&self, user_id: &str, ttl_ms: u64) -> Result<String> {
        let token = auth::random_hex(auth::TOKEN_BYTES);
        let now = wxlike_core::types::now_ms();
        let c = self.db.lock();
        let exists: Option<String> = c
            .query_row(
                "SELECT id FROM users WHERE id = ?1",
                params![user_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(PlatformError::NotFound);
        }
        c.execute(
            "INSERT INTO sessions(token, user_id, created_at, expires_at) VALUES(?1, ?2, ?3, ?4)",
            params![
                token,
                user_id,
                now as i64,
                now.saturating_add(ttl_ms) as i64
            ],
        )?;
        Ok(token)
    }

    /// Resolves a token to its user, dropping the row when it has expired.
    pub fn lookup_token(&self, token: &str) -> Result<Option<String>> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db.lock();
        let row: Option<(String, i64)> = c
            .query_row(
                "SELECT user_id, expires_at FROM sessions WHERE token = ?1",
                params![token],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?;
        match row {
            Some((user_id, expires)) if expires > now => Ok(Some(user_id)),
            Some(_) => {
                c.execute("DELETE FROM sessions WHERE token = ?1", params![token])?;
                Ok(None)
            }
            None => Ok(None),
        }
    }

    pub fn revoke_session(&self, token: &str) -> Result<()> {
        let c = self.db.lock();
        c.execute("DELETE FROM sessions WHERE token = ?1", params![token])?;
        Ok(())
    }

    /// Resolves a display name to a user id (used by friend/group endpoints).
    pub fn user_id_by_name(&self, name: &str) -> Result<Option<String>> {
        let c = self.db.lock();
        Ok(
            c.query_row("SELECT id FROM users WHERE name = ?1", params![name], |r| {
                r.get::<_, String>(0)
            })
            .optional()?,
        )
    }

    pub fn router(state: PlatformState) -> Router {
        Router::new()
            .merge(users::routes())
            .merge(friends::routes())
            .merge(groups::routes())
            .merge(media::routes())
            .merge(wallet::routes())
            .with_state(state)
    }

    /// Wires the hub (or a test recorder) that receives system envelopes.
    pub fn set_event_sink(&self, sink: Arc<dyn EventSink>) {
        let mut guard = self
            .sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(sink);
    }

    /// Best-effort push: a peer that cannot be reached must not roll back an
    /// already-committed relationship change, so delivery failures are logged
    /// and the caller's data stays committed.
    pub(crate) async fn emit(&self, env: Envelope) {
        let sink = {
            let guard = self
                .sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.clone()
        };
        if let Some(sink) = sink {
            if let Err(err) = sink.publish(env).await {
                eprintln!("[platform] system event publish failed: {err:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_roundtrip_and_rejection() {
        let (salt, hash) = hash_password("s3cret", 100_000).unwrap();
        assert_ne!(hash, "s3cret");
        assert!(verify_password("s3cret", &salt, &hash, 100_000).unwrap());
        assert!(!verify_password("s3creu", &salt, &hash, 100_000).unwrap());
        // salt is per-call random, so identical passwords differ on the wire
        let (salt2, hash2) = hash_password("s3cret", 100_000).unwrap();
        assert_ne!(hash, hash2);
        assert_ne!(salt, salt2);
    }

    #[test]
    fn verify_rejects_malformed_stored_digest() {
        assert!(!verify_password("x", "00", "nothex", 1).unwrap());
    }

    #[test]
    fn token_is_unique_per_issue() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            assert!(seen.insert(random_hex(32)));
        }
        assert_eq!(seen.len(), 200);
    }

    #[test]
    fn validation_limits() {
        assert!(validate::name("").is_err());
        assert!(validate::name("   ").is_err());
        assert!(validate::name(&"a".repeat(10_000)).is_err());
        assert!(validate::password("").is_err());
        assert!(validate::name("alice").is_ok());
        // WP-01 A4: injection-shaped input is refused outright
        assert!(validate::name("'; DROP TABLE users;--").is_err());
        assert!(validate::name("ali\nce").is_err());
        assert!(validate::id("deadbeef").is_ok());
        assert!(validate::id("../../etc/passwd").is_err());
    }
}
