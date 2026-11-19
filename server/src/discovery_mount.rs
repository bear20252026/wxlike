//! 把发现域（`crates/features/discovery`）装进运行中的 server —— G07 /
//! 第四轮审计「测而未接」孤岛接线。
//!
//! 域实现与测试早已完备（qr 扫码 / geo 附近 / matching 摇一摇），但没有任何
//! 生产入口依赖它。本模块是**组合根装配**，口径与 discover_mount / commerce_mount
//! 一致：
//!
//! 1. **鉴权只有一条路**。`Authorization: Bearer` → `Platform::lookup_token`，
//!    读的是平台 `sessions` 表；actor 一律来自令牌，不信请求体自报。
//! 2. **位置默认关闭**。发布位置前必须显式 opt-in（域内强制）；撤回授权立即
//!    删除可发现状态；`nearby` 只回粗网格与距离档，永不回精确坐标。
//! 3. **扫码不自动执行**。解码结果只是"待确认动作"；支付码一次性 nonce 由域内
//!    NonceStore 拦截；外链是否放行由 `WXLIKE_SCAN_ALLOWED_HOSTS` 白名单决定
//!    （不配 = 一律 BLOCKED_EXTERNAL）。
//! 4. **签名密钥没有公开默认值**。`WXLIKE_QR_SIGNING_KEY` /
//!    `WXLIKE_PAYMENT_CONFIRM_KEY` 生产必配；缺省用本次启动的随机密钥
//!    （重启即失效，只允许开发/CI 走到这里，与 commerce_mount 同口径）。
//!
//! 状态是进程内的（域实现即内存态，重启清零，与 G07-03 契约的 TTL 语义一致）；
//! SQLite 化由 G07 迁移提案另行推进，本挂载不建影子表。
//!
//! ADR-28：本模块不碰 SQLite；会话查询在同步 helper `actor_of` 里完成，
//! async handler 不持任何锁跨 await；CPU 密集的二维码解码/渲染整段进
//! `spawn_blocking`，不占 tokio worker。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use wxlike_discovery::clock::SystemClock;
use wxlike_discovery::crypto::MemoryNonceStore;
use wxlike_discovery::geo::{
    DistanceBand, GeoConfig, GeoService, NearbyPeer, PresenceView, Visibility,
};
use wxlike_discovery::matching::{MatchOutcome, MotionSample, ShakeMatcher};
use wxlike_discovery::qr::{DeeplinkPolicy, QrCodec, QrScanner, QrTarget, ScanAction};
use wxlike_discovery::DiscoveryError;
use wxlike_platform::PlatformState;

/// 二维码 payload 签名密钥。生产必配；多实例必须配成同一个值。
pub const QR_SECRET_ENV: &str = "WXLIKE_QR_SIGNING_KEY";
/// 支付确认令牌签名密钥。生产必配；与二维码签名密钥职责分离。
pub const PAYMENT_CONFIRM_ENV: &str = "WXLIKE_PAYMENT_CONFIRM_KEY";
/// 扫码外链域名白名单（逗号分隔）。不配 = 不放行任何外部域。
pub const SCAN_HOSTS_ENV: &str = "WXLIKE_SCAN_ALLOWED_HOSTS";
/// 缺省签名密钥的随机字节数（与 commerce_mount 同口径）。
const SIGNING_KEY_BYTES: usize = 32;

/// 挂载句柄：路由进 `build_app`；后台清理与集成测试取证用这些域句柄。
pub struct DiscoveryMount {
    pub router: Router,
    pub scanner: Arc<QrScanner<Arc<MemoryNonceStore>>>,
    pub codec: Arc<QrCodec>,
    pub geo: Arc<GeoService<SystemClock>>,
    pub shake: Arc<ShakeMatcher<SystemClock>>,
    nonces: Arc<MemoryNonceStore>,
}

impl DiscoveryMount {
    /// 过期 nonce 清理（G07-01 job：discovery.nonce.prune）。
    pub fn prune_nonces(&self) {
        self.nonces.prune(wxlike_core::types::now_ms() as i64);
    }
    /// nonce 账本句柄：后台清理循环与集成测试取证用。
    pub fn nonces_handle(&self) -> Arc<MemoryNonceStore> {
        Arc::clone(&self.nonces)
    }
}

/// 装配发现域路由。返回值由 `start_stack` 拆成路由 + 句柄。
pub fn assemble(platform: &PlatformState) -> anyhow::Result<DiscoveryMount> {
    let qr_key = signing_key(QR_SECRET_ENV);
    let nonces = Arc::new(MemoryNonceStore::default());
    let codec = Arc::new(QrCodec::new(qr_key.clone()));
    let scanner = Arc::new(QrScanner::new(
        QrCodec::new(qr_key),
        signing_key(PAYMENT_CONFIRM_ENV),
        deeplink_policy()?,
        Arc::clone(&nonces),
    ));
    let geo = Arc::new(GeoService::new(SystemClock, geo_config_from_env()));
    let shake = Arc::new(ShakeMatcher::new(SystemClock));
    let state = DiscoveryState {
        platform: platform.clone(),
        scanner: Arc::clone(&scanner),
        codec: Arc::clone(&codec),
        geo: Arc::clone(&geo),
        shake: Arc::clone(&shake),
    };
    let router = routes().with_state(state);
    Ok(DiscoveryMount {
        router,
        scanner,
        codec,
        geo,
        shake,
        nonces,
    })
}

fn routes() -> Router<DiscoveryState> {
    Router::new()
        .route("/api/v1/scan", post(scan_payload))
        .route("/api/v1/scan/confirm", post(scan_confirm))
        .route("/api/v1/qr", post(qr_encode))
        .route("/api/v1/nearby/opt-in", post(geo_opt_in))
        .route(
            "/api/v1/nearby/presence",
            post(geo_publish).get(geo_presence).delete(geo_revoke),
        )
        .route("/api/v1/nearby", post(geo_nearby))
        .route("/api/v1/nearby/block", post(geo_block))
        .route("/api/v1/nearby/report", post(geo_report))
        .route("/api/v1/shake/start", post(shake_start))
        .route("/api/v1/shake/{ticket_id}/join", post(shake_join))
        .route("/api/v1/shake/{ticket_id}", get(shake_ticket))
        .route("/api/v1/shake/{ticket_id}/cancel", delete(shake_cancel))
        .route(
            "/api/v1/shake/proposals/{proposal_id}/confirm",
            post(shake_confirm),
        )
}

#[derive(Clone)]
struct DiscoveryState {
    platform: PlatformState,
    scanner: Arc<QrScanner<Arc<MemoryNonceStore>>>,
    codec: Arc<QrCodec>,
    geo: Arc<GeoService<SystemClock>>,
    shake: Arc<ShakeMatcher<SystemClock>>,
}

/// 签名密钥：配了就用；没配用本次启动的随机密钥并留警告（只允许开发/CI）。
fn signing_key(env: &str) -> Vec<u8> {
    match crate::non_empty_env(env) {
        Some(secret) => secret.into_bytes(),
        None => {
            tracing::warn!(
                "{env} 未设置：发现域用本次启动的随机密钥签名，\
                 重启即失效、多实例互不认（只允许开发/CI 走到这里）"
            );
            wxlike_platform::random_hex(SIGNING_KEY_BYTES).into_bytes()
        }
    }
}

/// 外链白名单：不配 = 最严策略（外部域一律 BLOCKED_EXTERNAL，纯文本拒绝）。
fn deeplink_policy() -> anyhow::Result<DeeplinkPolicy> {
    let Some(raw) = crate::non_empty_env(SCAN_HOSTS_ENV) else {
        return Ok(DeeplinkPolicy::strict());
    };
    let hosts: Vec<String> = raw
        .split(',')
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .collect();
    DeeplinkPolicy::with_hosts(hosts, false)
        .map_err(|err| anyhow::anyhow!("{SCAN_HOSTS_ENV} 配置非法: {err}"))
}

/// 附近位置的 TTL 配置（G07-03：WXLIKE_GEO_DEFAULT_TTL_MS / WXLIKE_GEO_MAX_TTL_MS）。
fn geo_config_from_env() -> GeoConfig {
    let mut config = GeoConfig::default();
    if let Some(v) = crate::non_empty_env("WXLIKE_GEO_DEFAULT_TTL_MS").and_then(|v| v.parse().ok())
    {
        config.default_ttl_ms = v;
    }
    if let Some(v) = crate::non_empty_env("WXLIKE_GEO_MAX_TTL_MS").and_then(|v| v.parse().ok()) {
        config.max_ttl_ms = v;
    }
    config
}

use crate::mount_common::{actor_of, authed_json, err_response, parse_json};

/// 域错误 → HTTP 状态码 + 稳定分类。只回分类字符串，不回显内部细节。
fn classify(err: &DiscoveryError) -> (StatusCode, &'static str) {
    use DiscoveryError as E;
    let status = match err {
        E::InvalidArgument(_) => StatusCode::BAD_REQUEST,
        E::Unauthorized => StatusCode::UNAUTHORIZED,
        E::Forbidden | E::BlockedExternal(_) => StatusCode::FORBIDDEN,
        E::NotFound => StatusCode::NOT_FOUND,
        E::Conflict(_) | E::Cancelled => StatusCode::CONFLICT,
        E::Expired => StatusCode::GONE,
        E::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        E::DependencyUnavailable => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, err.code())
}

fn discovery_reply(result: wxlike_discovery::Result<serde_json::Value>) -> Response {
    match result {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(err) => {
            let (status, label) = classify(&err);
            err_response(status, label)
        }
    }
}

/// 阻塞任务的统一收尾：JoinError 如实报 INTERNAL，域结果走稳定分类。
fn join_reply(
    ran: std::result::Result<wxlike_discovery::Result<serde_json::Value>, tokio::task::JoinError>,
    what: &str,
) -> Response {
    match ran {
        Ok(result) => discovery_reply(result),
        Err(err) => {
            tracing::error!(%err, "discovery {what} 阻塞任务异常结束");
            err_response(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL")
        }
    }
}

// ---------------------------------------------------------------------------
// 扫码 / 二维码（G07-01）
// ---------------------------------------------------------------------------

/// `POST /api/v1/scan`：扫码入口。JSON `{"text": "..."}` 或
/// `{"image_base64": "..."}`。解码结果只是待确认动作，不自动执行。
async fn scan_payload(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (actor, payload) = match authed_json(&state.platform, &headers, &body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let scanner = Arc::clone(&state.scanner);
    let now_ms = wxlike_core::types::now_ms() as i64;
    // CPU 密集的解码/验签整段进阻塞池，不占 async worker。
    let ran =
        tokio::task::spawn_blocking(move || scan_dispatch(&scanner, &actor, &payload, now_ms))
            .await;
    match ran {
        Ok(Ok(result)) => match scan_json(&result) {
            Ok(value) => (StatusCode::OK, Json(value)).into_response(),
            Err(err) => {
                tracing::error!(%err, "discovery 扫码结果序列化失败");
                err_response(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL")
            }
        },
        Ok(Err(err)) => {
            let (status, label) = classify(&err);
            err_response(status, label)
        }
        Err(err) => {
            tracing::error!(%err, "discovery 扫码阻塞任务异常结束");
            err_response(StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL")
        }
    }
}

fn scan_dispatch(
    scanner: &QrScanner<Arc<MemoryNonceStore>>,
    actor: &str,
    payload: &serde_json::Value,
    now_ms: i64,
) -> wxlike_discovery::Result<wxlike_discovery::qr::ScanResult> {
    if let Some(text) = payload["text"].as_str() {
        return scanner.scan_text(actor, text, now_ms);
    }
    if let Some(encoded) = payload["image_base64"].as_str() {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| DiscoveryError::InvalidArgument("image base64"))?;
        return scanner.scan_image(actor, &bytes, now_ms);
    }
    Err(DiscoveryError::InvalidArgument("scan payload"))
}

fn scan_json(
    result: &wxlike_discovery::qr::ScanResult,
) -> std::result::Result<serde_json::Value, serde_json::Error> {
    let action = match &result.action {
        ScanAction::Signed(target) => {
            serde_json::json!({"type": "signed", "target": serde_json::to_value(target)?})
        }
        ScanAction::Payment {
            intent_id,
            amount_minor,
            currency,
            payee_id,
            confirm_token,
            confirm_expires_at_ms,
        } => {
            serde_json::json!({
                "type": "payment", "intent_id": intent_id, "amount_minor": amount_minor,
                "currency": currency, "payee_id": payee_id,
                "confirm_token": confirm_token, "confirm_expires_at_ms": confirm_expires_at_ms,
            })
        }
        ScanAction::External { url, host } => {
            serde_json::json!({"type": "external", "url": url, "host": host})
        }
        ScanAction::Text { text } => serde_json::json!({"type": "text", "text": text}),
    };
    Ok(serde_json::json!({
        "action": action,
        "requires_confirmation": result.requires_confirmation,
        "elapsed_ms": result.stats.elapsed_ms,
    }))
}

/// `POST /api/v1/scan/confirm`：用户确认后的二次入口。`{"url": ...}` 走外链
/// 白名单复核；支付字段组校验确认令牌与金额/收款人绑定。
async fn scan_confirm(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (actor, payload) = match authed_json(&state.platform, &headers, &body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let scanner = Arc::clone(&state.scanner);
    let now_ms = wxlike_core::types::now_ms() as i64;
    let ran =
        tokio::task::spawn_blocking(move || confirm_dispatch(&scanner, &actor, &payload, now_ms))
            .await;
    join_reply(ran, "扫码确认")
}

fn confirm_dispatch(
    scanner: &QrScanner<Arc<MemoryNonceStore>>,
    actor: &str,
    payload: &serde_json::Value,
    now_ms: i64,
) -> wxlike_discovery::Result<serde_json::Value> {
    if let Some(url) = payload["url"].as_str() {
        return Ok(serde_json::json!({"url": scanner.execute_external(url)?}));
    }
    let approval = scanner.approve_payment(
        actor,
        payload["intent_id"].as_str().unwrap_or_default(),
        payload["amount_minor"].as_i64().unwrap_or_default(),
        payload["currency"].as_str().unwrap_or_default(),
        payload["payee_id"].as_str().unwrap_or_default(),
        payload["confirm_token"].as_str().unwrap_or_default(),
        now_ms,
    )?;
    Ok(serde_json::json!({
        "intent_id": approval.intent_id, "amount_minor": approval.amount_minor,
        "currency": approval.currency, "payee_id": approval.payee_id,
        "approved_at_ms": approval.approved_at_ms,
    }))
}

/// `POST /api/v1/qr`：给会话用户签发业务二维码（好友/群/支付/小程序分型）。
/// 请求体 `{"target": {...}, "ttl_ms": 可选, "png": 可选 bool}`；nonce 服务端生成。
async fn qr_encode(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if actor_of(&state.platform, &headers).is_none() {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    }
    let Some(payload) = parse_json(&body) else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    let Ok(target) = serde_json::from_value::<QrTarget>(payload["target"].clone()) else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    let expires_at_ms = payload["ttl_ms"]
        .as_i64()
        .map(|ttl| wxlike_core::types::now_ms() as i64 + ttl.max(1));
    let want_png = payload["png"].as_bool().unwrap_or(false);
    let codec = Arc::clone(&state.codec);
    let nonce = wxlike_discovery::id::new_id("qrnonce");
    let ran = tokio::task::spawn_blocking(move || {
        encode_qr(&codec, &target, &nonce, expires_at_ms, want_png)
    })
    .await;
    join_reply(ran, "二维码签发")
}

fn encode_qr(
    codec: &QrCodec,
    target: &QrTarget,
    nonce: &str,
    expires_at_ms: Option<i64>,
    want_png: bool,
) -> wxlike_discovery::Result<serde_json::Value> {
    let wire = codec.encode(target, nonce, expires_at_ms)?;
    let mut value = serde_json::json!({"wire": wire, "expires_at_ms": expires_at_ms});
    if want_png {
        use base64::Engine as _;
        let png = wxlike_discovery::qr::render_png(&wire)?;
        value["png_base64"] =
            serde_json::json!(base64::engine::general_purpose::STANDARD.encode(png));
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// 位置 / 附近（G07-03）
// ---------------------------------------------------------------------------

/// `POST /api/v1/nearby/opt-in`：`{"enabled": bool}`。位置默认关闭，显式授权。
async fn geo_opt_in(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (actor, payload) = match authed_json(&state.platform, &headers, &body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(enabled) = payload["enabled"].as_bool() else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    discovery_reply(
        state
            .geo
            .set_opt_in(&actor, enabled)
            .map(|()| serde_json::json!({"enabled": enabled})),
    )
}

/// `POST /api/v1/nearby/presence`：发布粗网格位置。
/// `{"lat", "lng", "visibility": "everyone"|"friends", "ttl_ms": 可选}`。
async fn geo_publish(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (actor, payload) = match authed_json(&state.platform, &headers, &body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let (Some(lat), Some(lng)) = (payload["lat"].as_f64(), payload["lng"].as_f64()) else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    let visibility = match payload["visibility"].as_str().unwrap_or("friends") {
        "friends" => Visibility::Friends,
        "everyone" => Visibility::Everyone,
        _ => return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT"),
    };
    discovery_reply(
        state
            .geo
            .publish(&actor, lat, lng, visibility, payload["ttl_ms"].as_i64())
            .map(presence_json),
    )
}

/// `GET /api/v1/nearby/presence`：读自己的可发现状态；没有发布过 → 404。
async fn geo_presence(State(state): State<DiscoveryState>, headers: HeaderMap) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    match state.geo.presence(&actor) {
        Some(view) => (StatusCode::OK, Json(presence_json(view))).into_response(),
        None => err_response(StatusCode::NOT_FOUND, "NOT_FOUND"),
    }
}

/// `DELETE /api/v1/nearby/presence`：撤回授权，立即删除可发现状态。
async fn geo_revoke(State(state): State<DiscoveryState>, headers: HeaderMap) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    discovery_reply(
        state
            .geo
            .revoke(&actor)
            .map(|()| serde_json::json!({"revoked": true})),
    )
}

/// `POST /api/v1/nearby`：附近的人。`{"lat", "lng", "radius_m"}`；
/// 只回粗网格与距离档，永不回精确坐标/精确距离。
async fn geo_nearby(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (actor, payload) = match authed_json(&state.platform, &headers, &body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let (Some(lat), Some(lng), Some(radius_m)) = (
        payload["lat"].as_f64(),
        payload["lng"].as_f64(),
        payload["radius_m"].as_f64(),
    ) else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    discovery_reply(
        state.geo.nearby(&actor, lat, lng, radius_m).map(
            |peers| serde_json::json!({"peers": peers.iter().map(peer_json).collect::<Vec<_>>()}),
        ),
    )
}

/// `POST /api/v1/nearby/block`：拉黑（双向从附近结果中过滤）。`{"target"}`。
async fn geo_block(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (actor, payload) = match authed_json(&state.platform, &headers, &body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(target) = payload["target"].as_str() else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    discovery_reply(
        state
            .geo
            .block(&actor, target)
            .map(|()| serde_json::json!({"blocked": true})),
    )
}

/// `POST /api/v1/nearby/report`：骚扰举报（可追溯）。`{"target", "reason"}`。
async fn geo_report(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (actor, payload) = match authed_json(&state.platform, &headers, &body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let (Some(target), Some(reason)) = (payload["target"].as_str(), payload["reason"].as_str())
    else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    discovery_reply(state.geo.report(&actor, target, reason).map(|record| serde_json::json!({
        "target": record.target, "reason": record.reason, "reported_at_ms": record.reported_at_ms,
    })))
}

fn presence_json(view: PresenceView) -> serde_json::Value {
    serde_json::json!({
        "cell": {"lat_idx": view.cell.lat_idx, "lng_idx": view.cell.lng_idx},
        "visibility": visibility_str(view.visibility),
        "expires_at_ms": view.expires_at_ms,
        "updated_at_ms": view.updated_at_ms,
    })
}

fn visibility_str(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Friends => "friends",
        Visibility::Everyone => "everyone",
    }
}

fn peer_json(peer: &NearbyPeer) -> serde_json::Value {
    serde_json::json!({
        "user_id": peer.user_id,
        "cell": {"lat_idx": peer.cell.lat_idx, "lng_idx": peer.cell.lng_idx},
        "distance_band": band_str(peer.distance_band),
    })
}

fn band_str(band: DistanceBand) -> &'static str {
    match band {
        DistanceBand::Under500m => "under_500m",
        DistanceBand::Under1Km => "under_1km",
        DistanceBand::Under3Km => "under_3km",
        DistanceBand::Under10Km => "under_10km",
    }
}

// ---------------------------------------------------------------------------
// 摇一摇（G07-03）
// ---------------------------------------------------------------------------

/// `POST /api/v1/shake/start`：`{"magnitude_g", "sampled_at_ms": 可选}`。
/// 有时限窗口；域内校验加速度与采样新鲜度。
async fn shake_start(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (actor, payload) = match authed_json(&state.platform, &headers, &body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(magnitude_g) = payload["magnitude_g"].as_f64() else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    let sampled_at_ms = payload["sampled_at_ms"]
        .as_i64()
        .unwrap_or_else(|| wxlike_core::types::now_ms() as i64);
    discovery_reply(state.shake.start(&actor, MotionSample { magnitude_g, sampled_at_ms }).map(|ticket| {
        serde_json::json!({"ticket_id": ticket.id, "expires_at_ms": ticket.expires_at_ms})
    }))
}

/// `POST /api/v1/shake/{ticket_id}/join`：加入仍在时限内的窗口（单对方）。
async fn shake_join(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    Path(ticket_id): Path<String>,
) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    discovery_reply(
        state
            .shake
            .join(&actor, &ticket_id)
            .map(|id| serde_json::json!({"proposal_id": id})),
    )
}

/// `GET /api/v1/shake/{ticket_id}`：读窗口状态（过期 → 404）。
async fn shake_ticket(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    Path(ticket_id): Path<String>,
) -> Response {
    if actor_of(&state.platform, &headers).is_none() {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    }
    match state.shake.ticket(&ticket_id) {
        Some(ticket) => (StatusCode::OK, Json(serde_json::json!({
            "ticket_id": ticket.id, "user_id": ticket.user_id, "expires_at_ms": ticket.expires_at_ms,
        }))).into_response(),
        None => err_response(StatusCode::NOT_FOUND, "NOT_FOUND"),
    }
}

/// `DELETE /api/v1/shake/{ticket_id}/cancel`：取消窗口（参与者限定）。
async fn shake_cancel(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    Path(ticket_id): Path<String>,
) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    discovery_reply(
        state
            .shake
            .cancel(&actor, &ticket_id)
            .map(|()| serde_json::json!({"cancelled": true})),
    )
}

/// `POST /api/v1/shake/proposals/{proposal_id}/confirm`：双边确认才产生匹配。
async fn shake_confirm(
    State(state): State<DiscoveryState>,
    headers: HeaderMap,
    Path(proposal_id): Path<String>,
) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    discovery_reply(state.shake.confirm(&actor, &proposal_id).map(|outcome| match outcome {
        MatchOutcome::Pending { needs_host, needs_guest } => serde_json::json!({
            "state": "pending", "needs_host": needs_host, "needs_guest": needs_guest,
        }),
        MatchOutcome::Matched(record) => serde_json::json!({
            "state": "matched", "match_id": record.id, "users": record.users, "matched_at_ms": record.matched_at_ms,
        }),
    }))
}
