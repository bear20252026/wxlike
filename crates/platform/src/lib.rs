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

mod articles;
mod audit_mw;
mod auth;
mod call;
mod communities;
mod content_index;
mod db;
mod devices;
mod error;
pub mod extract;
mod favorites;
mod friends;
mod groups;
mod groups_admin;
mod inbox;
mod keys;
mod live;
mod media;
mod moments;
mod notes;
mod notification;
mod page;
pub mod perms;
mod read;
mod reconcile;
mod recovery;
mod relations;
mod report;
mod risk_mw;
mod secret;
mod security;
mod sessions;
mod status;
mod stickers;
mod trust;
mod users;
mod validate;
mod video_feed;
mod video_refs;
mod wallet;

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use wxlike_core::{Envelope, MsgBody, MsgKind};

pub use crate::auth::{hash_password, random_hex, token_digest, verify_password, Authed};
pub use crate::call::CallRegistry;
pub use crate::db::Db;
pub use crate::devices::{DeviceReg, DeviceRow};
pub use crate::error::{ApiError, PlatformError, Result};
pub use crate::favorites::{Favorite, FavoriteInput, MAX_FAVORITE_PAGE};
pub use crate::groups_admin::GroupInvite;
pub use crate::media::{LocalDirStore, MediaStore, StoredObject, MAX_MEDIA_BYTES, THUMB_MAX_EDGE};
pub use crate::perms::Perm;
pub use crate::reconcile::{spawn_reconciler, Mismatch, ReconcileReport};
pub use crate::relations::{StrangerRequest, StrangerState};
pub use crate::security::{hash_argon2id, totp_generate_secret, totp_verify, verify_argon2id};
pub use crate::status::spawn_status_sweeper;
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
    /// 管理端点令牌（`x-admin-token`）。`None` = 管理面关闭：P6 的 adminx
    /// 接手前，对账/审核/举报处置只能由进程内调用，不能从公网打。
    pub admin_token: Option<String>,
    /// 举报工单 SLA（超时进看板）。
    pub report_sla_ms: u64,
    /// 审核模式（合同 §3.14 可配置）：默认先审后发。
    pub moderation_mode: wxlike_moderation::Mode,
    /// 直播推流凭据的 HMAC 密钥。默认值仅供开发；部署必须用
    /// `WXLIKE_LIVE_SECRET` 覆盖（ingest 节点与平台共享同一密钥）。
    pub live_ingest_secret: String,
    /// 视频转码回调的令牌（`x-wxlike-video-token`）。`None` = 回调接口关闭。
    ///
    /// 回调会写"这条视频现在能播了"的引用，敞开等于让人往你的消息里塞任意
    /// 播放地址；所以和 `admin_token` 一样：**没配就是关**，不是"没配就放行"。
    pub video_callback_token: Option<String>,
    /// 部署在**受信反向代理**后面时为 `true`（issue #19）。
    ///
    /// 默认 `false`：限流的"来源"只取连接的对端 IP，不读 `x-forwarded-for`。
    /// 直连部署下那个头是客户端随便填的，信它等于把限流桶的钥匙交出去。
    pub trust_proxy: bool,
    /// 媒体下载签名 URL 的 HMAC 密钥（issue #14）。
    ///
    /// 空 = 每个进程随机生成（单实例部署足够，重启后旧链接失效）。
    /// 多实例/负载均衡部署必须用 `WXLIKE_MEDIA_SECRET` 配成同一个值，
    /// 否则 A 实例签的链接在 B 实例上验不过。
    pub media_signing_key: String,
}

impl Default for PlatformOptions {
    fn default() -> Self {
        Self {
            session_ttl_ms: DEFAULT_SESSION_TTL_MS,
            pbkdf2_rounds: auth::DEFAULT_ROUNDS,
            max_media_bytes: media::MAX_MEDIA_BYTES,
            admin_token: None,
            report_sla_ms: wxlike_report::DEFAULT_SLA_MS,
            moderation_mode: wxlike_moderation::Mode::PreReview,
            // 空 = 未配置。装配层必须填（随机或来自 env），不允许存在"公开默认值"。
            live_ingest_secret: String::new(),
            video_callback_token: None,
            trust_proxy: false,
            media_signing_key: String::new(),
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

/// GET /api/audit/self：本人最近 100 条审计；无记录返回空 entries（不 4xx）。
async fn audit_self(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let rows = platform.audit_of(&authed.user_id, 100).unwrap_or_default();
    Ok(Json(serde_json::json!({
        "entries": rows.into_iter().map(|(at, action, target, detail)| {
            serde_json::json!({
                "at_ms": at, "action": action,
                "target": target, "detail": detail
            })
        }).collect::<Vec<_>>()
    })))
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

impl From<wxlike_message::MailError> for PlatformError {
    fn from(e: wxlike_message::MailError) -> Self {
        match e {
            wxlike_message::MailError::Storage(r) => Self::Sqlite(r),
            other => Self::Mail(other.to_string()),
        }
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
    /// P3 支柱 C 频道时间线的读扩散信箱（与主库同文件/同内存实例）。
    message_store: Option<wxlike_message::MessageStore>,
    /// P4 风控前置层（ADR-20 第一层）：进程内滑动窗口，不落库。
    risk: wxlike_risk::RiskEngine,
    /// P4 审核能力层（ADR-20 第二层）：模式可热切换。
    moderation: std::sync::Mutex<wxlike_moderation::Pipeline>,
    /// P4/P5 通话房间注册表（进程内，重启即散）。
    calls: CallRegistry,
}

/// 把"未配置"的选项归一化成可安全使用的值。
///
/// 只处理一件事，但它很关键：**推流密钥不能为空**。空串是合法的 HMAC key，
/// 签出来的 ingest token 谁都能伪造——与"公开默认值"是同一类洞，只是更隐蔽。
/// 二进制的"缺 env 就拒绝启动"是第一道闸；这里保证**任何** `Platform` 实例
/// （含测试与嵌入式用法）都不会拿空密钥签名。
/// 媒体签名密钥的字节数（HMAC-SHA256 的密钥长度）。
const MEDIA_SIGNING_KEY_BYTES: usize = 32;

fn normalize_options(mut options: PlatformOptions) -> PlatformOptions {
    if options.live_ingest_secret.trim().is_empty() {
        options.live_ingest_secret = auth::random_hex(32);
    }
    // 空密钥不能留：空 HMAC key 的签名谁都能算出来，等于没有签名。
    // 单实例随机生成够用（重启后旧链接失效）；多实例部署必须显式配置。
    if options.media_signing_key.trim().is_empty() {
        options.media_signing_key = auth::random_hex(MEDIA_SIGNING_KEY_BYTES);
    }
    options
}

impl Platform {
    pub fn in_memory() -> Result<Self> {
        Self::in_memory_with(PlatformOptions::default())
    }

    pub fn in_memory_with(options: PlatformOptions) -> Result<Self> {
        let options = normalize_options(options);
        let moderation_mode = options.moderation_mode;
        Ok(Self {
            db: Db::open(None)?,
            options,
            sink: std::sync::Mutex::new(None),
            group_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            // an in-memory platform must not write into the working directory
            media: std::sync::Mutex::new(Arc::new(media::LocalDirStore::new(temp_media_dir()))),
            message_store: Some(wxlike_message::MessageStore::open(None)?),
            calls: CallRegistry::default(),
            risk: wxlike_risk::RiskEngine::with_defaults(),
            moderation: std::sync::Mutex::new(wxlike_moderation::Pipeline::new(
                moderation_mode,
                wxlike_moderation::Moderator::with_builtin_rules(),
            )),
        })
    }

    /// `None` path opens in-memory; `Some(file)` persists under `WXLIKE_DB`.
    /// 优雅关闭前把 WAL 落盘并截断。
    ///
    /// 为什么需要显式做：进程被 SIGTERM 直接杀掉时，`-wal` 文件可能留着未
    /// 检查点的页；下次启动要重放，慢且容易在异常掉电时留下半截状态。
    /// 这里在关闭路径上主动 checkpoint，失败只记日志——关闭不该因为磁盘问题卡住。
    pub fn checkpoint_wal(&self) {
        let conn = self.db.lock();
        if let Err(err) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
            eprintln!("[wxlike] WAL checkpoint 失败（继续关闭）: {err}");
        }
    }

    pub fn open(path: Option<&str>) -> Result<Self> {
        Self::open_with(path, PlatformOptions::default())
    }

    pub fn open_with(path: Option<&str>, options: PlatformOptions) -> Result<Self> {
        let options = normalize_options(options);
        let moderation_mode = options.moderation_mode;
        Ok(Self {
            db: Db::open(path)?,
            options,
            sink: std::sync::Mutex::new(None),
            group_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            media: std::sync::Mutex::new(Arc::new(media::LocalDirStore::new(
                media::default_media_dir(),
            ))),
            message_store: Some(wxlike_message::MessageStore::open(path)?),
            calls: CallRegistry::default(),
            risk: wxlike_risk::RiskEngine::with_defaults(),
            moderation: std::sync::Mutex::new(wxlike_moderation::Pipeline::new(
                moderation_mode,
                wxlike_moderation::Moderator::with_builtin_rules(),
            )),
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

    /// 读扩散信箱（频道时间线 / 弹幕不进时间线的断言面）。
    pub fn message_store(&self) -> Option<&wxlike_message::MessageStore> {
        self.message_store.as_ref()
    }

    /// P4 风控引擎（server 的投递策略也会读它）。
    pub fn risk(&self) -> &wxlike_risk::RiskEngine {
        &self.risk
    }

    /// 审核管线。调用方拿锁后做一次判定即释放——判定是纯 CPU，不 await。
    pub fn moderation(&self) -> std::sync::MutexGuard<'_, wxlike_moderation::Pipeline> {
        self.moderation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 管理端点令牌校验：未配置时一律拒绝（P6 adminx 之前不暴露管理面）。
    pub fn admin_ok(&self, presented: Option<&str>) -> bool {
        match self.options.admin_token.as_deref() {
            Some(expected) if !expected.is_empty() => presented == Some(expected),
            _ => false,
        }
    }

    /// Issues a session token valid for `ttl_ms` (0 = already expired, used by
    /// tests to cover the expiry path without sleeping).
    pub fn create_session(&self, user_id: &str, ttl_ms: u64) -> Result<String> {
        self.create_session_for_device(user_id, ttl_ms, "", "", "")
    }

    /// 带设备信息的会话（P4 设备管理页：按设备下线、看最近活跃）。
    /// 设备字段为空时与老会话等价，只多一个 `sid` 供设备页寻址。
    pub fn create_session_for_device(
        &self,
        user_id: &str,
        ttl_ms: u64,
        device_id: &str,
        device_name: &str,
        ip: &str,
    ) -> Result<String> {
        let token = auth::random_hex(auth::TOKEN_BYTES);
        let sid = auth::random_hex(6);
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
            "INSERT INTO sessions(token, user_id, created_at, expires_at, sid, device_id, device_name, ip, last_seen)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                auth::token_digest(&token),
                user_id,
                now as i64,
                now.saturating_add(ttl_ms) as i64,
                sid,
                device_id,
                device_name,
                ip,
                now as i64
            ],
        )?;
        Ok(token)
    }

    /// Resolves a token to its user, dropping the row when it has expired.
    /// The column holds [`auth::token_digest`] output; the raw token never
    /// touches the database (AR-3/H-2).
    pub fn lookup_token(&self, token: &str) -> Result<Option<String>> {
        let token = &auth::token_digest(token);
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
        let token = auth::token_digest(token);
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
            .merge(moments::routes())
            .merge(wallet::routes())
            .merge(keys::routes())
            .merge(devices::routes())
            .merge(favorites::routes())
            .merge(relations::routes())
            .merge(groups_admin::routes())
            .merge(communities::routes())
            .merge(notification::routes())
            .merge(recovery::routes())
            .merge(sessions::routes())
            .merge(secret::routes())
            .merge(trust::routes())
            .merge(read::routes())
            .merge(report::routes())
            .merge(reconcile::routes())
            .merge(live::routes())
            .merge(call::routes())
            .merge(perms::routes())
            .merge(video_refs::routes())
            .merge(articles::routes())
            .merge(notes::routes())
            .merge(stickers::routes())
            .merge(video_feed::routes())
            .merge(content_index::routes())
            .merge(inbox::routes())
            .merge(status::routes())
            .route("/api/audit/self", get(audit_self))
            .with_state(state.clone())
            // 命令审计：带 `X-Wxlike-Command` 的请求各落一条（见 audit_mw）
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                audit_mw::command_audit,
            ))
            // AR-5（#29）：写接口风控的声明式闸门，挂在最外层——
            // handler 不再各自记得调 risk_gate（见 risk_mw）
            .layer(axum::middleware::from_fn_with_state(state, risk_mw::gate))
    }

    /// P3 审计日志（不变式 7）：独立审计表，只写面。检索/留存策略归
    /// P6 adminx；与运行日志（tracing → Loki）物理分离。
    pub fn audit(&self, actor: &str, action: &str, target: &str, detail: &str) {
        let r = self.db().lock().execute(
            "INSERT INTO audit_log(at_ms, actor, action, target, detail) VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                wxlike_core::types::now_ms() as i64,
                actor,
                action,
                target,
                detail
            ],
        );
        if let Err(err) = r {
            eprintln!("[platform] audit write failed: {err}");
        }
    }

    /// 审计读取面（自查自己的记录；全量检索归 P6 adminx + RBAC）。
    pub fn audit_of(
        &self,
        actor: &str,
        limit: usize,
    ) -> Result<Vec<(i64, String, String, String)>> {
        let limit = limit.clamp(1, 200) as i64;
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT at_ms, action, target, detail FROM audit_log
             WHERE actor = ?1 ORDER BY at_ms DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![actor, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
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
