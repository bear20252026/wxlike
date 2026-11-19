//! 把 G08 小程序**真实域**接进运行中的 server —— 第四轮审计「测而未接」收口。
//!
//! `mini_mount.rs` 是早先的服务端控制面（目录卡/授权/卸载的落盘投影，install 与
//! 包体下载诚实 501）。它没有 dispatch 进 `wxlike-miniapps`/`wxlike-container`/
//! `wxlike-dpage` 三个域 crate，所以审计称其为「影子」。本模块补上真实域面：
//!
//! 1. **目录搜索/推荐**走 `wxlike_miniapps::catalog::Catalog`（审核目录的唯一事实
//!    源是管理后台配置的 [`CatalogStore`]，空目录就是空数组，不拿静态假数据填充）。
//! 2. **包体检视**走 `wxlike_miniapps::packages::open` + `wxlike_dpage::peek`
//!    （形状/宿主兼容/逐资源摘要校验，不做信任判定）。
//! 3. **包体验签**走 `wxlike_miniapps::packages::verify` + `toolchain::load_trust`
//!    （根信任快照 `WXLIKE_MINI_TRUST`）。没配根密钥就诚实 501，不伪造签名能力。
//! 4. **沙箱启动**走 `wxlike_container::host::MiniHost` + `wxlike_dpage::Page`：
//!    解包 → 隔离档案 → Boa 宿主跑 onLoad/onShow 生命周期，CPU/内存预算兜底。
//!    只运行调用方显式提供、且 app_id 在审核目录里的页面包；**不**把它装进可信
//!    目录（签名安装是下面诚实阻塞的那一面）。
//! 5. **安装**诚实 501：依赖「签名包二进制上传 → 验签 → 原子提交」的流式核心
//!    提供者与离线根密钥，当前环境没有，不伪造安装成功（与 mini_mount 同口径）。
//!
//! 鉴权只有一条路：`Authorization: Bearer` → `Platform::lookup_token`，与平台
//! `Authed` 同一张 sessions 表（与 commerce/discover 挂载同口径）。
//!
//! ADR-28：域对象（`Catalog`/`MiniHost`/`Container`/`Installer`）含 `!Send` 的
//! trait 对象与 Boa `Context`，**整段在 `spawn_blocking` 闭包内构造并使用**，绝不
//! 存进 axum state、绝不跨 await 持锁。state 只放 `Send` 数据（目录快照、平台句柄、
//! 路径、宿主版本）。会话校验的 `lookup_token` 同样进阻塞池，async handler 不持锁。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use wxlike_container::host::profile::DeviceTarget;
use wxlike_container::host::{HostError, MiniHost};
use wxlike_dpage as dpage;
use wxlike_miniapps::catalog::recent::MemoryRecents;
use wxlike_miniapps::catalog::source::{
    CatalogEntry, Category, ReviewedCatalog, SearchQuery, DEFAULT_SEARCH_LIMIT,
};
use wxlike_miniapps::catalog::{Catalog, CatalogError};
use wxlike_miniapps::packages::{self, MiniError, PackageSignature};
use wxlike_miniapps::toolchain::{self, DEFAULT_HOST_VERSION};
use wxlike_platform::{Authed, PlatformState};

/// 根信任快照路径的环境变量（验签面用）。没配 = 验签诚实阻塞。
pub const TRUST_ENV: &str = "WXLIKE_MINI_TRUST";
/// 宿主版本的环境变量；缺省用工具链的 `DEFAULT_HOST_VERSION`。
pub const HOST_VERSION_ENV: &str = "WXLIKE_MINI_HOST_VERSION";

/// 审核目录的落盘存储（管理后台配置，默认空）。这是 `Catalog` 的唯一事实源，
/// 取代「影子」各自维护的应用表。集成测试预置它后验证搜索/推荐/启动往返。
pub struct CatalogStore {
    entries: Mutex<Vec<CatalogEntry>>,
    path: PathBuf,
}

impl CatalogStore {
    /// 打开（或新建）落盘目录。目录不可用是依赖问题，如实报错。
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        let entries = if path.exists() {
            let bytes = std::fs::read(&path)?;
            serde_json::from_slice(&bytes).unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(Self {
            entries: Mutex::new(entries),
            path,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Vec<CatalogEntry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 管理后台配置入口：上架/更新一张审核目录卡（落盘）。
    pub fn upsert_entry(&self, entry: CatalogEntry) -> anyhow::Result<()> {
        let mut entries = self.lock();
        entries.retain(|existing| existing.app_id != entry.app_id);
        entries.push(entry);
        self.persist(&entries)
    }

    /// 当前审核目录快照（供 `Catalog` 在阻塞池内消费）。
    pub fn snapshot(&self) -> Vec<CatalogEntry> {
        self.lock().clone()
    }

    fn persist(&self, entries: &[CatalogEntry]) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(entries)?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// 真实域面的共享状态：只放 `Send` 数据。域对象在阻塞池内按请求构造。
pub struct AppState {
    pub catalog: Arc<CatalogStore>,
    pub platform: PlatformState,
    pub host_version: String,
    pub trust_path: Option<PathBuf>,
}

struct RuntimeError {
    status: StatusCode,
    message: String,
}

impl RuntimeError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        RuntimeError {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for RuntimeError {
    fn into_response(self) -> axum::response::Response {
        let body = json!({ "error": self.message });
        (self.status, Json(body)).into_response()
    }
}

type ApiResult = Result<Json<Value>, RuntimeError>;

/// 会话校验：bearer → 平台 sessions 表换可信 actor。`lookup_token` 是同步 rusqlite，
/// 整段进阻塞池（ADR-28），async handler 不持连接锁。失败一律 401，不放行。
async fn actor(state: &AppState, headers: &HeaderMap) -> Result<String, RuntimeError> {
    let token = Authed::bearer_from(headers)
        .ok_or_else(|| RuntimeError::new(StatusCode::UNAUTHORIZED, "missing session"))?
        .to_string();
    let platform = state.platform.clone();
    let found = tokio::task::spawn_blocking(move || platform.lookup_token(&token))
        .await
        .map_err(|err| {
            tracing::error!(%err, "mini runtime 会话校验阻塞任务异常结束");
            RuntimeError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        })?;
    found
        .ok()
        .flatten()
        .ok_or_else(|| RuntimeError::new(StatusCode::UNAUTHORIZED, "session rejected"))
}

/// 把同步域工作整段搬进阻塞池（ADR-28）。闭包只捕获 `Send` 数据，域对象在闭包内
/// 构造并使用、用完即弃，绝不跨 await 持锁。
async fn run_blocking<F>(work: F) -> ApiResult
where
    F: FnOnce() -> Result<Value, RuntimeError> + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Ok(Json(value)),
        Ok(Err(err)) => Err(err),
        Err(err) => {
            tracing::error!(%err, "mini runtime 阻塞任务异常结束");
            Err(RuntimeError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error",
            ))
        }
    }
}

#[derive(Deserialize, Default)]
struct SearchParams {
    q: Option<String>,
    category: Option<String>,
    limit: Option<usize>,
}

fn build_query(params: SearchParams) -> Result<SearchQuery, RuntimeError> {
    let mut query = SearchQuery::new(params.q.unwrap_or_default());
    if let Some(raw) = params.category {
        let category: Category = serde_json::from_value(Value::String(raw))
            .map_err(|_| RuntimeError::new(StatusCode::BAD_REQUEST, "invalid category"))?;
        query.category = Some(category);
    }
    if let Some(limit) = params.limit {
        query.limit = limit;
    }
    Ok(query)
}

/// GET /api/mini/catalog/search：审核目录搜索（真实 `Catalog::search`）。
async fn catalog_search(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<SearchParams>,
) -> ApiResult {
    actor(&state, &headers).await?;
    let query = build_query(params)?;
    run_blocking(move || search_catalog(&state, &query)).await
}

#[derive(Deserialize, Default)]
struct RecommendParams {
    limit: Option<usize>,
}

/// GET /api/mini/catalog/recommend：审核目录推荐（真实 `Catalog::recommend`）。
async fn catalog_recommend(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<RecommendParams>,
) -> ApiResult {
    actor(&state, &headers).await?;
    let limit = params.limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
    run_blocking(move || recommend_catalog(&state, limit)).await
}

fn search_catalog(state: &AppState, query: &SearchQuery) -> Result<Value, RuntimeError> {
    let mut catalog = fresh_catalog(state);
    let hits = catalog.search(query).map_err(catalog_err)?;
    Ok(json!({ "items": hits }))
}

fn recommend_catalog(state: &AppState, limit: usize) -> Result<Value, RuntimeError> {
    let mut catalog = fresh_catalog(state);
    let hits = catalog.recommend(limit).map_err(catalog_err)?;
    Ok(json!({ "items": hits }))
}

/// 在阻塞池内用审核目录快照构造真实 `Catalog`（域对象含 `!Send` trait 对象，
/// 只在闭包内存在）。搜索/推荐是无状态读，最近/收藏的跨请求同步由 G02 适配器
/// 后续接入（见 integration-requests/G08/G08-04.yaml）。
fn fresh_catalog(state: &AppState) -> Catalog {
    Catalog::new(
        Box::new(ReviewedCatalog::new(state.catalog.snapshot())),
        Box::new(MemoryRecents::default()),
    )
}

#[derive(Deserialize)]
struct PackageBody {
    package_hex: String,
}

/// POST /api/mini/package/inspect：包体检视（`dpage::peek` + `miniapps::open`）。
async fn package_inspect(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Option<Json<PackageBody>>,
) -> ApiResult {
    actor(&state, &headers).await?;
    let Json(body) =
        body.ok_or_else(|| RuntimeError::new(StatusCode::BAD_REQUEST, "request body is required"))?;
    let blob = decode_package(&body.package_hex)?;
    run_blocking(move || inspect_package(&state, &blob)).await
}

fn inspect_package(state: &AppState, blob: &[u8]) -> Result<Value, RuntimeError> {
    let (dpage_version, dpage_id) = dpage::peek(blob)
        .map_err(|err| RuntimeError::new(StatusCode::BAD_REQUEST, err.to_string()))?;
    let inspected = packages::open(blob, &state.host_version).map_err(mini_err)?;
    let manifest = &inspected.manifest;
    Ok(json!({
        "app_id": manifest.app_id,
        "name": manifest.name,
        "version": manifest.version,
        "channel": manifest.channel.as_str(),
        "min_host": manifest.min_host,
        "entry": manifest.entry,
        "publisher": manifest.publisher,
        "capabilities": manifest.capabilities,
        "digest": inspected.digest,
        "files": inspected.files.len(),
        "dpage_version": dpage_version,
        "dpage_id": dpage_id,
        "signature": "unverified",
    }))
}

#[derive(Deserialize)]
struct VerifyBody {
    package_hex: String,
    key_id: String,
    sig: String,
}

/// POST /api/mini/package/verify：包体验签（`miniapps::verify` + 根信任快照）。
/// 没配 `WXLIKE_MINI_TRUST` 就诚实 501，不伪造签名能力。
async fn package_verify(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Option<Json<VerifyBody>>,
) -> ApiResult {
    actor(&state, &headers).await?;
    let Json(body) =
        body.ok_or_else(|| RuntimeError::new(StatusCode::BAD_REQUEST, "request body is required"))?;
    let blob = decode_package(&body.package_hex)?;
    let signature = PackageSignature {
        key_id: body.key_id,
        sig: body.sig,
    };
    run_blocking(move || verify_package(&state, &blob, &signature)).await
}

fn verify_package(
    state: &AppState,
    blob: &[u8],
    signature: &PackageSignature,
) -> Result<Value, RuntimeError> {
    let Some(path) = state.trust_path.as_ref() else {
        return Err(RuntimeError::new(
            StatusCode::NOT_IMPLEMENTED,
            "包体验签需要配置根信任快照（WXLIKE_MINI_TRUST），当前未配置",
        ));
    };
    let trust = toolchain::load_trust(path).map_err(mini_err)?;
    let now = packages::trust::now_seconds();
    let verified =
        packages::verify(blob, signature, &trust, now, &state.host_version).map_err(mini_err)?;
    let signer = verified.signer();
    Ok(json!({
        "app_id": verified.manifest.app_id,
        "version": verified.manifest.version,
        "digest": verified.digest,
        "publisher": signer.publisher,
        "key_id": signer.key_id,
        "verified": true,
    }))
}

#[derive(Deserialize)]
struct LaunchBody {
    package_hex: String,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    payload: Option<String>,
}

/// POST /api/mini/launch：沙箱启动一个审核目录内的页面包，跑真实宿主生命周期
/// （`dpage` 解包 → `miniapps::open` 取清单 → `container::MiniHost` 运行）。
async fn launch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Option<Json<LaunchBody>>,
) -> ApiResult {
    actor(&state, &headers).await?;
    let Json(body) =
        body.ok_or_else(|| RuntimeError::new(StatusCode::BAD_REQUEST, "request body is required"))?;
    let blob = decode_package(&body.package_hex)?;
    let target = parse_target(body.target.as_deref())?;
    let payload = body.payload.unwrap_or_else(|| "{}".to_string());
    run_blocking(move || launch_page(&state, &blob, target, &payload)).await
}

fn launch_page(
    state: &AppState,
    blob: &[u8],
    target: DeviceTarget,
    payload: &str,
) -> Result<Value, RuntimeError> {
    let inspected = packages::open(blob, &state.host_version).map_err(mini_err)?;
    let app_id = inspected.manifest.app_id.clone();
    // 只运行审核目录里的应用：目录是唯一事实源，不在目录里的包不许启动。
    if !state.catalog.snapshot().iter().any(|e| e.app_id == app_id) {
        return Err(RuntimeError::new(
            StatusCode::NOT_FOUND,
            "app is not in the reviewed catalog",
        ));
    }
    let version = inspected.manifest.version.clone();
    let digest = inspected.digest.clone();
    let page = page_from_inspected(blob, inspected)?;
    let mut host = MiniHost::launch_default(target, &app_id).map_err(host_err)?;
    let title = host.run(&page, payload).map_err(host_err)?;
    Ok(json!({
        "app_id": app_id,
        "version": version,
        "digest": digest,
        "title": title,
        "state": host.state().as_str(),
        "session_id": host.session_id(),
        "verified": false,
    }))
}

/// 把已检视的包体转成宿主可运行的 `dpage::Page`（内容 id 来自 stub）。
fn page_from_inspected(
    blob: &[u8],
    inspected: packages::InspectedPackage,
) -> Result<dpage::Page, RuntimeError> {
    let (_version, id) = dpage::peek(blob)
        .map_err(|err| RuntimeError::new(StatusCode::BAD_REQUEST, err.to_string()))?;
    let name = inspected.manifest.name.clone();
    let entry = inspected.manifest.entry.clone();
    Ok(dpage::Page {
        id,
        manifest: dpage::Manifest {
            name: name.clone(),
            main_js: entry,
            title: name,
        },
        files: inspected.files,
    })
}

/// POST /api/mini/install：安装。**诚实阻塞**——依赖签名包二进制上传与离线根密钥
/// 的流式核心提供者，当前环境没有，不伪造安装成功（与 mini_mount 同口径）。
async fn install(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult {
    actor(&state, &headers).await?;
    Err(RuntimeError::new(
        StatusCode::NOT_IMPLEMENTED,
        "小程序安装依赖签名包二进制上传与根信任密钥（WXLIKE_MINI_TRUST），尚无流式核心提供者",
    ))
}

fn decode_package(package_hex: &str) -> Result<Vec<u8>, RuntimeError> {
    hex::decode(package_hex.trim())
        .map_err(|_| RuntimeError::new(StatusCode::BAD_REQUEST, "package_hex must be hex"))
}

fn parse_target(raw: Option<&str>) -> Result<DeviceTarget, RuntimeError> {
    match raw.unwrap_or("desktop") {
        "desktop" => Ok(DeviceTarget::Desktop),
        "web" => Ok(DeviceTarget::Web),
        "ios" => Ok(DeviceTarget::Ios),
        "android" => Ok(DeviceTarget::Android),
        _ => Err(RuntimeError::new(StatusCode::BAD_REQUEST, "invalid target")),
    }
}

/// 目录错误 → HTTP 状态码 + 稳定分类（不回显内部报文）。
fn catalog_err(err: CatalogError) -> RuntimeError {
    use wxlike_miniapps::catalog::source::SourceError;
    let (status, label) = match err {
        CatalogError::BadQuery(_) | CatalogError::Source(SourceError::BadQuery(_)) => {
            (StatusCode::BAD_REQUEST, "bad catalog query")
        }
        CatalogError::Source(SourceError::NotFound(_)) => (StatusCode::NOT_FOUND, "app not found"),
        CatalogError::Source(SourceError::Unauthorized) => {
            (StatusCode::UNAUTHORIZED, "catalog unauthorized")
        }
        CatalogError::Source(SourceError::Offline) | CatalogError::Source(SourceError::Timeout) => {
            (StatusCode::BAD_GATEWAY, "catalog unavailable")
        }
        CatalogError::NotPublished(_, _) => (StatusCode::CONFLICT, "version not published"),
        CatalogError::Revoked(_) => (StatusCode::FORBIDDEN, "app withdrawn"),
        CatalogError::NotInstalled(_) => (StatusCode::NOT_FOUND, "app not installed"),
        CatalogError::CapabilityNotReviewed(_, _) => {
            (StatusCode::FORBIDDEN, "capability not reviewed")
        }
        CatalogError::Denied(_) => (StatusCode::FORBIDDEN, "operation denied"),
        CatalogError::Cancelled(_) => (StatusCode::CONFLICT, "operation cancelled"),
        CatalogError::Sync(_) => (StatusCode::BAD_GATEWAY, "recent sync unavailable"),
        CatalogError::DataWipe(_) => (StatusCode::INTERNAL_SERVER_ERROR, "data wipe failed"),
        CatalogError::Package(err) => return mini_err(err),
    };
    RuntimeError::new(status, label)
}

/// 包错误 → HTTP 状态码 + 稳定分类。5xx 全量落日志，响应体只回固定分类。
fn mini_err(err: MiniError) -> RuntimeError {
    let (status, label) = match &err {
        MiniError::BadSignature
        | MiniError::BadSigning
        | MiniError::UntrustedPublisher
        | MiniError::PublisherMismatch => (StatusCode::UNAUTHORIZED, "untrusted package"),
        MiniError::CertExpired | MiniError::Revoked => {
            (StatusCode::FORBIDDEN, "publisher revoked or expired")
        }
        MiniError::ResourceTooLarge(_, _, _) | MiniError::PackageTooLarge(_, _) => {
            (StatusCode::PAYLOAD_TOO_LARGE, "package too large")
        }
        MiniError::Downgrade(_, _, _)
        | MiniError::VersionConflict(_, _)
        | MiniError::WrongChannel(_, _) => (StatusCode::CONFLICT, "version conflict"),
        MiniError::HostTooOld(_, _) => (StatusCode::CONFLICT, "host too old"),
        MiniError::NotInstalled(_) => (StatusCode::NOT_FOUND, "app not installed"),
        MiniError::Io(_) | MiniError::Codec(_) | MiniError::InjectedFault(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
        _ => (StatusCode::BAD_REQUEST, "bad package"),
    };
    if status.is_server_error() {
        tracing::warn!(error = %err, "mini package error");
    }
    RuntimeError::new(status, label)
}

/// 宿主错误 → HTTP 状态码 + 稳定分类。
fn host_err(err: HostError) -> RuntimeError {
    let (status, label) = match &err {
        HostError::BadAppId | HostError::BadStorageKey => {
            (StatusCode::BAD_REQUEST, "invalid app or storage key")
        }
        HostError::InvalidState(_, _) => (StatusCode::CONFLICT, "invalid host state"),
        HostError::BudgetExceeded(_) => (StatusCode::FORBIDDEN, "budget exceeded"),
        HostError::Guest(_) => (StatusCode::UNPROCESSABLE_ENTITY, "guest error"),
        HostError::WasmVersionNotAllowed(_) => (StatusCode::FORBIDDEN, "wasm version not allowed"),
        HostError::Load(_) => (StatusCode::INTERNAL_SERVER_ERROR, "page load failed"),
    };
    RuntimeError::new(status, label)
}

/// 路由本体（与装配分离，便于测试预置目录后直接打）。
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/mini/catalog/search", get(catalog_search))
        .route("/api/mini/catalog/recommend", get(catalog_recommend))
        .route("/api/mini/package/inspect", post(package_inspect))
        .route("/api/mini/package/verify", post(package_verify))
        .route("/api/mini/launch", post(launch))
        .route("/api/mini/install", post(install))
        .with_state(state)
}

/// 装配真实域面路由。返回值由 `mini_mount::assemble` 直接 `merge`。
pub fn assemble(platform: &PlatformState, db_path: Option<&str>) -> anyhow::Result<Router> {
    let path = crate::sidecar_dir(db_path, "mini").join("mini-catalog.json");
    let catalog = Arc::new(CatalogStore::open(path)?);
    let host_version =
        crate::non_empty_env(HOST_VERSION_ENV).unwrap_or_else(|| DEFAULT_HOST_VERSION.to_string());
    let trust_path = crate::non_empty_env(TRUST_ENV).map(PathBuf::from);
    let state = Arc::new(AppState {
        catalog,
        platform: platform.clone(),
        host_version,
        trust_path,
    });
    Ok(router(state))
}
