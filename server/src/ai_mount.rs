//! 把识物/OCR/转写（`crates/vision`）与文本翻译（`crates/translation`）装进
//! 运行中的 server —— G07 / 第四轮审计「测而未接」孤岛接线。
//!
//! 两个域的编排层早已完备（consent 闸门、限流、超时取消、结果 TTL、一键删除），
//! 缺的只是生产入口。本模块是**组合根装配**，口径与 discover_mount 一致：
//!
//! 1. **鉴权只有一条路**。`Authorization: Bearer` → `Platform::lookup_token`；
//!    actor 一律来自令牌，不信请求体自报。
//! 2. **没有供应商凭据就诚实拒绝**。仓内没有 vision/translation 的沙箱或
//!    fixture provider 实现（域测试里的 Stub 只在 crate 测试内），生产环境也
//!    没有 `WXLIKE_VISION_PROVIDER_CREDENTIAL` / `WXLIKE_TRANSLATION_PROVIDER_CREDENTIAL`
//!    对应的真实供应商适配器。因此装配 `provider: None`：请求经 consent/限流
//!    校验后由**域服务**如实报 `DEPENDENCY_UNAVAILABLE`（→ HTTP 503），
//!    绝不返回样例识别结果或样例译文。真实供应商适配器落地后只需在这里注入。
//! 3. **用户内容不落盘不落日志**。图片/音频/原文只在内存中过一遍；审计摘要
//!    只有 actor/kind/字节数（域内 `audit_summary`）。结果有 TTL 与删除入口。
//!
//! ADR-28：本模块不碰 SQLite；会话查询在同步 helper `actor_of` 里完成，
//! async handler 不持任何锁跨 await。域服务的 run 本身是 async（供应商调用
//! 面），直接在 async 上下文 await，不需要 spawn_blocking。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use wxlike_platform::{Authed, PlatformState};
use wxlike_translation::{TranslationConfig, TranslationProvider, TranslationRequest, TranslationService};
use wxlike_vision::ports::{AudioInput, ImageInput};
use wxlike_vision::service::VisionInput;
use wxlike_vision::{CancelToken, JobKind, VisionConfig, VisionOutput, VisionProvider, VisionRequest, VisionService};

/// 真实视觉供应商凭据（登记于 integration-requests/G07/G07-02.yaml）。
/// 当前没有对应的供应商适配器：配置了也不会伪造结果，服务保持诚实拒绝。
pub const VISION_CREDENTIAL_ENV: &str = "WXLIKE_VISION_PROVIDER_CREDENTIAL";
/// 真实翻译供应商凭据。同上。
pub const TRANSLATION_CREDENTIAL_ENV: &str = "WXLIKE_TRANSLATION_PROVIDER_CREDENTIAL";
/// 识物/OCR/转写请求体上限：域内单图/单音频 20 MiB，base64 膨胀 4/3 再留 JSON 余量。
const RECOGNITION_BODY_LIMIT: usize = 32 * 1024 * 1024;

/// 未配置的视觉供应商占位类型。装配时传 `None`，域服务在真正调用前就报
/// `DEPENDENCY_UNAVAILABLE`；即便将来误注入本类型，三个方法也只会诚实报
/// `Unavailable`，不会产出任何结果。
pub struct UnconfiguredVisionProvider;

#[async_trait::async_trait]
impl VisionProvider for UnconfiguredVisionProvider {
    async fn identify(&self, _image: &ImageInput) -> std::result::Result<wxlike_vision::IdentifyResult, wxlike_vision::ProviderError> {
        Err(wxlike_vision::ProviderError::Unavailable)
    }
    async fn ocr(&self, _image: &ImageInput, _languages: &[String]) -> std::result::Result<wxlike_vision::OcrResult, wxlike_vision::ProviderError> {
        Err(wxlike_vision::ProviderError::Unavailable)
    }
    async fn transcribe(&self, _audio: &AudioInput) -> std::result::Result<wxlike_vision::Transcript, wxlike_vision::ProviderError> {
        Err(wxlike_vision::ProviderError::Unavailable)
    }
}

/// 未配置的翻译供应商占位类型（同上，绝不产出样例译文）。
pub struct UnconfiguredTranslationProvider;

#[async_trait::async_trait]
impl TranslationProvider for UnconfiguredTranslationProvider {
    async fn translate(
        &self, _text: &str, _source: Option<&str>, _target: &str,
    ) -> std::result::Result<wxlike_translation::Translation, wxlike_translation::ProviderError> {
        Err(wxlike_translation::ProviderError::Unavailable)
    }
}

/// 挂载句柄：路由进 `build_app`；域句柄供后台清理与测试取证。
pub struct AiMount {
    pub router: Router,
    pub vision: Arc<VisionService<UnconfiguredVisionProvider>>,
    pub translation: Arc<TranslationService<UnconfiguredTranslationProvider>>,
}

impl AiMount {
    /// 过期结果清理（G07-02 job：vision.jobs.purge）。
    pub fn spawn_purge_worker(&self, every: Duration) -> tokio::task::JoinHandle<()> {
        let vision = Arc::clone(&self.vision);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                vision.purge_expired(wxlike_core::types::now_ms() as i64);
            }
        })
    }
}

/// 装配视觉/翻译路由。生产默认：两个域都不配置供应商（诚实拒绝面）。
pub fn assemble(platform: &PlatformState) -> AiMount {
    if crate::non_empty_env(VISION_CREDENTIAL_ENV).is_some() || crate::non_empty_env(TRANSLATION_CREDENTIAL_ENV).is_some() {
        // 凭据存在但没有真实供应商适配器：不假装能用，如实留警告。
        tracing::warn!(
            "检测到 vision/translation 供应商凭据，但组合根尚无真实供应商适配器；\
             识别/翻译面保持 DEPENDENCY_UNAVAILABLE（绝不返回样例结果）"
        );
    }
    let vision = Arc::new(VisionService::new(VisionConfig::default(), None::<UnconfiguredVisionProvider>));
    let translation = Arc::new(TranslationService::new(TranslationConfig::default(), None::<UnconfiguredTranslationProvider>));
    let state = AiState { platform: platform.clone(), vision: Arc::clone(&vision), translation: Arc::clone(&translation) };
    let router = routes().with_state(state);
    AiMount { router, vision, translation }
}

fn routes() -> Router<AiState> {
    Router::new()
        .route("/api/v1/recognition/consent", post(vision_consent).delete(vision_consent_revoke))
        .route("/api/v1/recognition/jobs", post(vision_run).layer(DefaultBodyLimit::max(RECOGNITION_BODY_LIMIT)))
        .route("/api/v1/recognition/jobs/{job_id}", get(vision_job))
        .route("/api/v1/recognition/data", delete(vision_delete_data))
        .route("/api/v1/translation/consent", post(translation_consent).delete(translation_consent_revoke))
        .route("/api/v1/translation", post(translation_run))
        .route("/api/v1/translation/data", delete(translation_delete_data))
}

#[derive(Clone)]
struct AiState {
    platform: PlatformState,
    vision: Arc<VisionService<UnconfiguredVisionProvider>>,
    translation: Arc<TranslationService<UnconfiguredTranslationProvider>>,
}

/// 从请求头解析可信 actor（平台 sessions 表）。失败 → None → 401。
/// 同步函数：连接锁在这里拿、这里放，async handler 不持锁跨 await。
fn actor_of(platform: &PlatformState, headers: &HeaderMap) -> Option<String> {
    let token = Authed::bearer_from(headers)?;
    match platform.lookup_token(token) {
        Ok(Some(user_id)) => Some(user_id),
        _ => None,
    }
}

fn err_response(status: StatusCode, label: &str) -> Response {
    (status, Json(serde_json::json!({"error": label}))).into_response()
}

/// 域错误码（两域同一套字符串）→ HTTP 状态码。供应商未配置 = 503，
/// 与「服务暂时不可用」的语义一致；客户端可据稳定分类决定重试/提示。
fn status_of(code: &str) -> StatusCode {
    match code {
        "CONSENT_REQUIRED" => StatusCode::FORBIDDEN,
        "NOT_FOUND" => StatusCode::NOT_FOUND,
        "RATE_LIMITED" => StatusCode::TOO_MANY_REQUESTS,
        "INVALID_ARGUMENT" => StatusCode::BAD_REQUEST,
        "UNAUTHORIZED" => StatusCode::UNAUTHORIZED,
        "TIMEOUT" => StatusCode::GATEWAY_TIMEOUT,
        "CANCELLED" => StatusCode::CONFLICT,
        "DEPENDENCY_UNAVAILABLE" => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn coded_error(code: &str) -> Response {
    err_response(status_of(code), code)
}

fn parse_json(body: &Bytes) -> Option<serde_json::Value> {
    if body.is_empty() {
        return Some(serde_json::json!({}));
    }
    serde_json::from_slice(body).ok()
}

fn now_ms() -> i64 {
    wxlike_core::types::now_ms() as i64
}

// ---------------------------------------------------------------------------
// 识物 / OCR / 转写（G07-02）
// ---------------------------------------------------------------------------

/// `POST /api/v1/recognition/consent`：上传前显式授权（域内 TTL 到期自动失效）。
async fn vision_consent(State(state): State<AiState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    let Some(payload) = parse_json(&body) else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    match state.vision.grant_consent(&actor, now_ms(), payload["ttl_ms"].as_i64()) {
        Ok(expires_at_ms) => (StatusCode::OK, Json(serde_json::json!({"expires_at_ms": expires_at_ms}))).into_response(),
        Err(err) => coded_error(err.code().as_str()),
    }
}

/// `DELETE /api/v1/recognition/consent`：撤回上传授权。
async fn vision_consent_revoke(State(state): State<AiState>, headers: HeaderMap) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    state.vision.revoke_consent(&actor);
    (StatusCode::OK, Json(serde_json::json!({"revoked": true}))).into_response()
}

/// `POST /api/v1/recognition/jobs`：识物/OCR/转写。
/// `{"kind": "identify"|"ocr"|"transcribe", "image_base64"|"audio_base64",
///   "mime", "languages": [...]}`。无供应商 → 503 DEPENDENCY_UNAVAILABLE。
async fn vision_run(State(state): State<AiState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    let Some(payload) = parse_json(&body) else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    let request = match vision_request(&payload) {
        Ok(request) => request,
        Err(err) => return coded_error(err.code().as_str()),
    };
    // 审计摘要只有 actor/kind/字节数；用户内容不进日志（域内口径）。
    tracing::debug!("{}", wxlike_vision::service::audit_summary(&actor, request.kind, request.input.byte_len()));
    match state.vision.run(&actor, request, &CancelToken::new(), now_ms()).await {
        Ok(result) => (StatusCode::OK, Json(serde_json::json!({
            "job_id": result.job_id, "kind": result.kind.as_str(), "output": output_json(&result.output),
        }))).into_response(),
        Err(err) => coded_error(err.code().as_str()),
    }
}

fn vision_request(payload: &serde_json::Value) -> wxlike_vision::error::Result<VisionRequest> {
    use base64::Engine as _;
    use wxlike_vision::{VisionError, VisionErrorCode as Code};
    let kind = match payload["kind"].as_str() {
        Some("identify") => JobKind::Identify,
        Some("ocr") => JobKind::Ocr,
        Some("transcribe") => JobKind::Transcribe,
        _ => return Err(VisionError::new(Code::InvalidArgument, "job kind")),
    };
    let mime = payload["mime"].as_str().unwrap_or_default().to_string();
    let field = if kind == JobKind::Transcribe { "audio_base64" } else { "image_base64" };
    let encoded = payload[field].as_str().ok_or_else(|| VisionError::new(Code::InvalidArgument, "media base64 field"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)
        .map_err(|_| VisionError::new(Code::InvalidArgument, "media base64"))?;
    let input = if kind == JobKind::Transcribe {
        VisionInput::Audio(AudioInput { bytes, mime })
    } else {
        VisionInput::Image(ImageInput { bytes, mime })
    };
    let languages = payload["languages"].as_array()
        .map(|values| values.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(VisionRequest { kind, input, languages })
}

fn output_json(output: &VisionOutput) -> serde_json::Value {
    match output {
        VisionOutput::Identify(result) => serde_json::json!({
            "kind": "identify", "labels": result.labels, "provider": result.provider,
        }),
        VisionOutput::Ocr(result) => serde_json::json!({
            "kind": "ocr", "text": result.text, "language": result.language,
            "confidence": result.confidence, "blocks": result.blocks, "provider": result.provider,
        }),
        VisionOutput::Transcribe(result) => serde_json::json!({
            "kind": "transcribe", "text": result.text, "language": result.language,
            "confidence": result.confidence, "provider": result.provider,
        }),
    }
}

/// `GET /api/v1/recognition/jobs/{job_id}`：读结果（仅本人、TTL 内）。
async fn vision_job(State(state): State<AiState>, headers: HeaderMap, Path(job_id): Path<String>) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    match state.vision.job(&actor, &job_id, now_ms()) {
        Ok(view) => (StatusCode::OK, Json(serde_json::json!({
            "job_id": view.job_id, "kind": view.kind.as_str(),
            "output": output_json(&view.output), "expires_at_ms": view.expires_at_ms,
        }))).into_response(),
        Err(err) => coded_error(err.code().as_str()),
    }
}

/// `DELETE /api/v1/recognition/data`：一键删除本人结果与授权。
async fn vision_delete_data(State(state): State<AiState>, headers: HeaderMap) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    let removed = state.vision.delete_actor_data(&actor);
    (StatusCode::OK, Json(serde_json::json!({"removed": removed}))).into_response()
}

// ---------------------------------------------------------------------------
// 文本翻译（G07-02）
// ---------------------------------------------------------------------------

/// `POST /api/v1/translation/consent`：送供应商前的显式授权。
async fn translation_consent(State(state): State<AiState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    let Some(payload) = parse_json(&body) else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    match state.translation.grant_consent(&actor, now_ms(), payload["ttl_ms"].as_i64()) {
        Ok(expires_at_ms) => (StatusCode::OK, Json(serde_json::json!({"expires_at_ms": expires_at_ms}))).into_response(),
        Err(err) => coded_error(err.code().as_str()),
    }
}

/// `DELETE /api/v1/translation/consent`：撤回授权。
async fn translation_consent_revoke(State(state): State<AiState>, headers: HeaderMap) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    state.translation.revoke_consent(&actor);
    (StatusCode::OK, Json(serde_json::json!({"revoked": true}))).into_response()
}

/// `POST /api/v1/translation`：`{"text", "source": 可选, "target"}`。
/// 无供应商 → 503 DEPENDENCY_UNAVAILABLE；译文从不持久化。
async fn translation_run(State(state): State<AiState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    let Some(payload) = parse_json(&body) else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    let (Some(text), Some(target)) = (payload["text"].as_str(), payload["target"].as_str()) else {
        return err_response(StatusCode::BAD_REQUEST, "BAD_INPUT");
    };
    let request = TranslationRequest {
        text: text.to_owned(),
        source: payload["source"].as_str().map(str::to_owned),
        target: target.to_owned(),
    };
    match state.translation.run(&actor, request, &CancelToken::new(), now_ms()).await {
        Ok(translation) => (StatusCode::OK, Json(serde_json::json!({
            "text": translation.text, "source_language": translation.source_language,
            "target_language": translation.target_language,
            "confidence": translation.confidence, "provider": translation.provider,
        }))).into_response(),
        Err(err) => coded_error(err.code().as_str()),
    }
}

/// `DELETE /api/v1/translation/data`：删除本人授权与限流痕迹。
async fn translation_delete_data(State(state): State<AiState>, headers: HeaderMap) -> Response {
    let Some(actor) = actor_of(&state.platform, &headers) else {
        return err_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED");
    };
    let removed = state.translation.delete_actor_data(&actor);
    (StatusCode::OK, Json(serde_json::json!({"removed": removed}))).into_response()
}