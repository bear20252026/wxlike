//! 把小程序域（G08）装进运行中的 server —— D6 逐域退役的最后一域。
//!
//! 与 media/discover 不同：miniapps crate 是**客户端**打包/安装/目录逻辑
//! （CatalogSource 内存替身、按设备的 InstallLedger、DataWiper），没有服务端
//! HTTP 提供者。本模块新建一个诚实的服务端 mini 域：
//!
//! 1. **鉴权只有一条路**。`Authorization: Bearer` → `Platform::lookup_token`，
//!    与平台 `Authed` 同一张 sessions 表。
//! 2. **目录由管理后台配置**（落盘 JSON，默认空）。空目录就是空数组，不拿
//!    静态假数据填充。每张应用卡带真实 app_id/origins/latest release。
//! 3. **授权与安装记录按用户落盘**。grants/installs 只读写当前 actor 自己的。
//! 4. **install 与包体下载诚实阻塞**。install 依赖"下载签名包 → sha256 校验
//!    → 提交"的二进制链路，没有 JSON 流式核心提供者；包体下载同样是二进制。
//!    两者经服务端如实回 501（BLOCKED），不伪造安装成功。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wxlike_platform::{Authed, PlatformState};

/// 小程序发布记录（前端 MiniAppRelease 契约形状）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MiniRelease {
    pub version: String,
    pub manifest_digest: String,
    pub signature: String,
    pub package_url: String,
    pub released_at: i64,
    #[serde(default)]
    pub revoked: bool,
}

/// 小程序目录卡（前端 MiniAppRecord 契约形状）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MiniApp {
    pub app_id: String,
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// 可信 origin（宿主 sandbox 检查依据）。
    pub origins: Vec<String>,
    /// 全部发布版本（newest first）；latest 取第一条未撤销的。
    pub releases: Vec<MiniRelease>,
}

/// 一条授权（前端 MiniGrant 契约形状）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MiniGrant {
    pub app_id: String,
    pub scopes: Vec<String>,
    pub granted_at: i64,
    #[serde(default)]
    pub expires_at: Option<i64>,
}

/// 落盘状态：目录（全局）+ 按用户的授权/安装。
#[derive(Default, Serialize, Deserialize)]
struct MiniState {
    /// app_id → 应用卡。管理后台配置；默认空。
    apps: BTreeMap<String, MiniApp>,
    /// user_id → (app_id → 授权)。
    grants: BTreeMap<String, BTreeMap<String, MiniGrant>>,
    /// user_id → (app_id → 已安装版本)。
    installs: BTreeMap<String, BTreeMap<String, String>>,
}

/// 服务端 mini 域句柄：内存状态 + 落盘路径（每次写后原子落盘）。
pub struct MiniService {
    state: Mutex<MiniState>,
    path: PathBuf,
}

impl MiniService {
    /// 打开（或新建）落盘状态。目录不可用是依赖问题，如实报错。
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        let state = if path.exists() {
            let bytes = std::fs::read(&path)?;
            serde_json::from_slice(&bytes).unwrap_or_default()
        } else {
            MiniState::default()
        };
        Ok(Self {
            state: Mutex::new(state),
            path,
        })
    }

    fn persist(&self, state: &MiniState) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(state)?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MiniState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 管理后台配置入口：上架/更新一张应用卡（落盘）。生产由管理面调用，
    /// 集成测试用它预置目录后验证授权/卸载往返。
    pub fn upsert_app(&self, app: MiniApp) -> anyhow::Result<()> {
        let mut svc = self.lock();
        svc.apps.insert(app.app_id.clone(), app);
        self.persist(&svc)
    }
}

pub struct AppState {
    pub service: Arc<MiniService>,
    pub platform: PlatformState,
}

/// 会话校验：bearer → 平台 sessions 表换可信 actor。
fn actor(state: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    let token = Authed::bearer_from(headers)
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "missing session"))?;
    state
        .platform
        .lookup_token(token)
        .ok()
        .flatten()
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "session rejected"))
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

/// 应用卡 → 前端形状（latest 取第一条未撤销发布；installed_version 由调用方补）。
fn app_view(app: &MiniApp, installed: Option<&str>) -> Value {
    let latest = app.releases.iter().find(|r| !r.revoked);
    json!({
        "app_id": app.app_id,
        "name": app.name,
        "icon": app.icon,
        "description": app.description,
        "origins": app.origins,
        "latest": latest,
        "installed_version": installed,
    })
}

/// GET /api/miniapps：目录（含当前用户的 installed_version）。
async fn list_apps(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult {
    let user = actor(&state, &headers)?;
    let svc = state.service.lock();
    let installs = svc.installs.get(&user);
    let items: Vec<Value> = svc
        .apps
        .values()
        .map(|app| {
            app_view(
                app,
                installs
                    .and_then(|m| m.get(&app.app_id))
                    .map(String::as_str),
            )
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

/// GET /api/miniapps/{id}/releases：某应用的全部发布版本。
async fn list_releases(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    actor(&state, &headers)?;
    let svc = state.service.lock();
    let app = svc
        .apps
        .get(&id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "app not found"))?;
    Ok(Json(json!({ "items": app.releases })))
}

#[derive(Deserialize)]
struct GrantQuery {
    app_id: Option<String>,
}

/// GET /api/mini-grants[?app_id=]：当前用户的授权。
async fn list_grants(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<GrantQuery>,
) -> ApiResult {
    let user = actor(&state, &headers)?;
    let svc = state.service.lock();
    let empty = BTreeMap::new();
    let grants = svc.grants.get(&user).unwrap_or(&empty);
    let items: Vec<Value> = grants
        .values()
        .filter(|g| {
            query
                .app_id
                .as_deref()
                .map(|id| g.app_id == id)
                .unwrap_or(true)
        })
        .map(|g| serde_json::to_value(g).unwrap_or(json!({})))
        .collect();
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
struct GrantBody {
    app_id: String,
    scopes: Vec<String>,
}

/// POST /api/mini-grants：授予/更新当前用户对某应用的授权。
/// body 用 Option 提取：先鉴权（匿名一律 401），再校验 body 存在，避免
/// Json 提取器在鉴权之前因缺 body 报 415/400。
async fn create_grant(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Option<Json<GrantBody>>,
) -> ApiResult {
    let user = actor(&state, &headers)?;
    let Json(body) =
        body.ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "request body is required"))?;
    if body.app_id.trim().is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "app_id is required"));
    }
    let now = wxlike_core::types::now_ms() as i64;
    let grant = MiniGrant {
        app_id: body.app_id.clone(),
        scopes: body.scopes,
        granted_at: now,
        expires_at: None,
    };
    let mut svc = state.service.lock();
    // 目录里没有的应用不许授权（不凭空给一个不存在的应用授权）。
    if !svc.apps.contains_key(&body.app_id) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "app not found"));
    }
    svc.grants
        .entry(user)
        .or_default()
        .insert(body.app_id, grant.clone());
    state
        .service
        .persist(&svc)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::to_value(grant).unwrap_or(json!({}))))
}

/// DELETE /api/mini-grants/{app_id}：撤回当前用户对某应用的授权。
async fn revoke_grant(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> ApiResult {
    let user = actor(&state, &headers)?;
    let mut svc = state.service.lock();
    if let Some(grants) = svc.grants.get_mut(&user) {
        grants.remove(&app_id);
    }
    state
        .service
        .persist(&svc)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({})))
}

/// DELETE /api/mini-installs/{app_id}：卸载（移除当前用户的安装记录）。
async fn uninstall(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(app_id): Path<String>,
) -> ApiResult {
    let user = actor(&state, &headers)?;
    let mut svc = state.service.lock();
    if let Some(installs) = svc.installs.get_mut(&user) {
        installs.remove(&app_id);
    }
    // 卸载同时撤权（与原契约一致：卸载即收回该应用的授权）。
    if let Some(grants) = svc.grants.get_mut(&user) {
        grants.remove(&app_id);
    }
    state
        .service
        .persist(&svc)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({})))
}

/// POST /api/mini-installs：安装。**诚实阻塞**——install 依赖"下载签名包 →
/// sha256 校验 → 提交"的二进制链路，没有 JSON 流式核心提供者，不伪造安装成功。
async fn install(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult {
    actor(&state, &headers)?;
    Err(ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "小程序安装依赖包体二进制下载与 sha256 校验，尚无流式核心提供者",
    ))
}

/// GET /api/miniapps/{id}/releases/{version}/package：包体下载。**诚实阻塞**
/// （二进制流，无 JSON 核心提供者）。
async fn download_package(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult {
    actor(&state, &headers)?;
    Err(ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "小程序包体下载（二进制流）尚无流式核心提供者",
    ))
}

/// 路由本体（与装配分离，便于测试预置目录后直接打）。
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/miniapps", get(list_apps))
        .route("/api/miniapps/{id}/releases", get(list_releases))
        .route(
            "/api/miniapps/{id}/releases/{version}/package",
            get(download_package),
        )
        .route("/api/mini-grants", get(list_grants).post(create_grant))
        .route("/api/mini-grants/{app_id}", delete(revoke_grant))
        .route("/api/mini-installs", post(install))
        .route("/api/mini-installs/{app_id}", delete(uninstall))
        .with_state(state)
}

/// 装配 mini 域路由。返回值由 `build_app` 直接 `merge`。
///
/// 两个面合并：本模块的落盘控制面（目录卡/授权/卸载，install 与包体下载诚实 501）
/// 与 [`crate::mini_runtime`] 的真实域面（审核目录搜索/包检视/验签/容器沙箱启动，
/// dispatch 进 wxlike-miniapps / wxlike-container / wxlike-dpage）。路径前缀不重叠
/// （控制面 `/api/miniapps*`、`/api/mini-grants*`、`/api/mini-installs*`；真实域
/// `/api/mini/*`），`merge` 不会撞路由。
pub fn assemble(platform: &PlatformState, db_path: Option<&str>) -> anyhow::Result<Router> {
    let path = crate::sidecar_dir(db_path, "mini").join("mini-state.json");
    let service = Arc::new(MiniService::open(path)?);
    let state = Arc::new(AppState {
        service,
        platform: platform.clone(),
    });
    let runtime = crate::mini_runtime::assemble(platform, db_path)?;
    Ok(router(state).merge(runtime))
}
