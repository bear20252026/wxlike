// ---------------------------------------------------------------------------
// 小程序域（D6 逐域退役）：目录/授权/卸载控制面经认证桥。
//
// 服务端是 G08（server/mini_mount，与平台同 sessions 鉴权）。前端旧路径
// /api/miniapps*、/api/mini-grants*、/api/mini-installs/{id} 由消费侧路由层
// 映射到这里的 mini.* action：
//   mini.list      → GET    /api/miniapps                 （{items} 信封透传）
//   mini.releases  → GET    /api/miniapps/{id}/releases   （{items} 信封透传）
//   mini.grants    → GET    /api/mini-grants[?app_id=]    （{items} 信封透传）
//   mini.grant     → POST   /api/mini-grants              （MiniGrant 透传）
//   mini.revoke    → DELETE /api/mini-grants/{app_id}     （204 → {}）
//   mini.uninstall → DELETE /api/mini-installs/{app_id}   （204 → {}）
//
// install（POST /api/mini-installs）依赖包体二进制下载 + sha256 校验，包体下载
// 同样是二进制：两者没有 JSON 流式核心提供者，在消费侧显式阻塞，不在本桥伪造。
// 凭据口径与其余已迁移域相同：authed_json 是唯一认证出口，Bearer 不出 Rust。
// ---------------------------------------------------------------------------

fn mini_id(req: &CoreRequest, field: &str) -> Result<String, BridgeError> {
    param_path_id(&req.params, field)
}

/// 小程序目录：→ {items:[MiniAppRecord]}（含当前用户 installed_version）。
fn act_mini_list<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    authed_json(state, "GET", "/api/miniapps", None, None)
}

/// 某应用的发布版本：app_id → {items:[MiniAppRelease]}。
fn act_mini_releases<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = mini_id(req, "app_id")?;
    authed_json(state, "GET", &format!("/api/miniapps/{id}/releases"), None, None)
}

/// 当前用户的授权：app_id 可选 → {items:[MiniGrant]}。
fn act_mini_grants<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let mut path = String::from("/api/mini-grants");
    if let Some(app_id) = req.params.get("app_id").and_then(Value::as_str) {
        if !app_id.is_empty() {
            path.push_str(&format!("?app_id={app_id}"));
        }
    }
    authed_json(state, "GET", &path, None, None)
}

/// 授予/更新授权：{app_id, scopes} → MiniGrant。
fn act_mini_grant<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let params = &req.params;
    let body = json!({
        "app_id": param_str(params, "app_id")?,
        "scopes": params.get("scopes").cloned().unwrap_or(json!([])),
    });
    authed_json(state, "POST", "/api/mini-grants", Some(&body), req.idempotency_key.as_deref())
}

/// 撤回授权：app_id → 204 → {}。
fn act_mini_revoke<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = mini_id(req, "app_id")?;
    authed_json(state, "DELETE", &format!("/api/mini-grants/{id}"), None, req.idempotency_key.as_deref())
}

/// 卸载：app_id → 204 → {}（同时撤权）。
fn act_mini_uninstall<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = mini_id(req, "app_id")?;
    authed_json(state, "DELETE", &format!("/api/mini-installs/{id}"), None, req.idempotency_key.as_deref())
}