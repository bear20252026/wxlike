// ---------------------------------------------------------------------------
// 内容域（D6 逐域退役）：moments/favorites/report/stickers 控制面经认证桥。
//
// 服务端形状原样透传（信封/扁平 detail/只回 id 的写操作），投影回 api.ts 形状
// 由消费侧路由层负责（同钱包口径）。G05 契约面里服务端尚未对齐的部分
// （articles 列表/publisher 订阅/article watching）在消费侧显式阻塞，不伪造。
//
// 凭据口径与钱包/通话/视频域相同：authed_json 是唯一认证出口，Bearer 不出 Rust。
// ---------------------------------------------------------------------------

fn content_id(req: &CoreRequest, field: &str) -> Result<String, BridgeError> {
    param_path_id(&req.params, field)
}

/// 朋友圈发布：{text, media_ids, visibility} → {moment_id}。
fn act_moment_publish<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let params = &req.params;
    let body = json!({
        "text": params.get("text").and_then(Value::as_str).unwrap_or(""),
        "media_ids": params.get("media_ids").cloned().unwrap_or(json!([])),
        "visibility": params.get("visibility").and_then(Value::as_str).unwrap_or("public"),
    });
    authed_json(state, "POST", "/api/moments", Some(&body), req.idempotency_key.as_deref())
}

/// 朋友圈 feed：since/limit 可选 → {moments, next_cursor, has_more}。
fn act_moment_feed<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let params = &req.params;
    let mut path = String::from("/api/moments/feed");
    let mut sep = '?';
    if let Some(since) = params.get("since").and_then(Value::as_i64) {
        path.push_str(&format!("{sep}since={since}"));
        sep = '&';
    }
    if let Some(limit) = params.get("limit").and_then(Value::as_i64) {
        path.push_str(&format!("{sep}limit={limit}"));
    }
    authed_json(state, "GET", &path, None, None)
}

fn act_moment_detail<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = content_id(req, "moment_id")?;
    authed_json(state, "GET", &format!("/api/moments/{id}"), None, None)
}

fn act_moment_remove<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = content_id(req, "moment_id")?;
    authed_json(state, "DELETE", &format!("/api/moments/{id}"), None, req.idempotency_key.as_deref())
}

fn act_moment_like<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = content_id(req, "moment_id")?;
    authed_json(state, "POST", &format!("/api/moments/{id}/likes"), None, req.idempotency_key.as_deref())
}

fn act_moment_unlike<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = content_id(req, "moment_id")?;
    authed_json(state, "DELETE", &format!("/api/moments/{id}/likes"), None, req.idempotency_key.as_deref())
}

fn act_moment_comment<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = content_id(req, "moment_id")?;
    let params = &req.params;
    let text = param_str(params, "text")?;
    let body = json!({
        "text": text,
        "reply_to_comment_id": params.get("reply_to_comment_id").cloned().unwrap_or(Value::Null),
    });
    authed_json(state, "POST", &format!("/api/moments/{id}/comments"), Some(&body), req.idempotency_key.as_deref())
}

fn act_moment_comment_remove<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = content_id(req, "moment_id")?;
    let cid = content_id(req, "comment_id")?;
    authed_json(state, "DELETE", &format!("/api/moments/{id}/comments/{cid}"), None, req.idempotency_key.as_deref())
}

/// 举报：{target_kind, target_id, reason, detail} → {ticket_id, state}。
fn act_report<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let params = &req.params;
    let body = json!({
        "target_kind": params.get("target_kind").and_then(Value::as_str).unwrap_or("moment"),
        "target_id": param_str(params, "target_id")?,
        "reason": param_str(params, "reason")?,
        "detail": params.get("detail").and_then(Value::as_str).unwrap_or(""),
    });
    authed_json(state, "POST", "/api/reports", Some(&body), req.idempotency_key.as_deref())
}

/// 收藏列表：→ {favorites, next_cursor, has_more}。
fn act_favorite_list<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    authed_json(state, "GET", "/api/favorites", None, None)
}

fn act_favorite_one<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = content_id(req, "id")?;
    authed_json(state, "GET", &format!("/api/favorites/{id}"), None, None)
}

fn act_favorite_remove<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = content_id(req, "id")?;
    authed_json(state, "DELETE", &format!("/api/favorites/{id}"), None, req.idempotency_key.as_deref())
}

/// 收藏（服务端快照）：body 原样透传 → Favorite。
fn act_favorite_add<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let body = req.params.get("body").filter(|v| v.is_object()).ok_or_else(|| missing_param("body"))?;
    authed_json(state, "POST", "/api/favorites", Some(body), req.idempotency_key.as_deref())
}

/// 收藏备注编辑：服务端 inline PUT（snapshot_label）→ Favorite。
fn act_favorite_update<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = content_id(req, "id")?;
    let label = param_str(&req.params, "snapshot_label")?;
    let body = json!({ "snapshot_label": label });
    authed_json(state, "PUT", &format!("/api/favorites/{id}"), Some(&body), req.idempotency_key.as_deref())
}

/// 表情包列表：→ {packs}。
fn act_sticker_packs<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    authed_json(state, "GET", "/api/stickers/packs", None, None)
}

/// 媒体签名 URL（朋友圈图片 `<img src>` 依赖）：→ {url, expires_at}。
/// 只迁这一条媒体**读**面（认证签链接）；files 域的上传/下载二进制仍无提供者。
fn act_media_signed_url<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = content_id(req, "media_id")?;
    authed_json(state, "GET", &format!("/api/media/{id}/url"), None, None)
}