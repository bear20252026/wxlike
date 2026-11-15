// ---------------------------------------------------------------------------
// 视频/直播域（D6 逐域退役）：控制面经认证桥，播放面保持匿名。
//
// 服务端把两面分开（crates/video/src/lib.rs，issue #7）：播放面（meta/清单/
// 分片/mp4/缩略图）匿名可用——<video> 与 HLS 播放器不会带 Authorization；控制面
// （上传/作业/直播）才认调用者。本桥只退役**控制面里读旧会话的那部分**：
// meta/stats/view/feed 与 live 六条。上传（multipart 二进制）与播放 URL 不经过
// 这里——前者在消费侧显式阻塞（无流式提供者），后者是匿名播放面，按设计保留。
//
// 凭据口径与钱包/通话域相同：authed_json 是唯一认证出口，Bearer 不出 Rust。
// live.start 回的 ingest_token 是**推流凭据**（给推流端用），不是会话 Bearer。
// ---------------------------------------------------------------------------

fn video_id_param(req: &CoreRequest) -> Result<String, BridgeError> {
    param_path_id(&req.params, "video_id")
}

fn live_channel_param(req: &CoreRequest) -> Result<String, BridgeError> {
    param_path_id(&req.params, "channel")
}

/// meta：服务端播放面匿名可读，形状原样透传（VideoMeta）。
fn act_video_meta<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = video_id_param(req)?;
    authed_json(state, "GET", &format!("/api/video/{id}"), None, None)
}

/// stats：{views, viewers, ...} 原样透传。
fn act_video_stats<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = video_id_param(req)?;
    authed_json(state, "GET", &format!("/api/video/{id}/stats"), None, None)
}

/// view：{viewer, watched_ms}。viewer 是客户端自报标识（服务端点播面不鉴权，
/// 见 crates/video/src/stats.rs），桥只校验非空，不把它当可信身份。
fn act_video_view<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = video_id_param(req)?;
    let params = &req.params;
    let viewer = param_str(params, "viewer")?;
    let watched = params
        .get("watched_ms")
        .and_then(Value::as_i64)
        .ok_or_else(|| missing_param("watched_ms"))?;
    let body = json!({ "viewer": viewer, "watched_ms": watched.max(1) });
    authed_json(
        state,
        "POST",
        &format!("/api/video/{id}/view"),
        Some(&body),
        req.idempotency_key.as_deref(),
    )
}

/// feed：mode=hot|follow，cursor/limit 可选；信封 {items,next_cursor,has_more} 透传。
fn act_video_feed<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let params = &req.params;
    let mode = match params.get("mode").and_then(Value::as_str).unwrap_or("hot") {
        "follow" => "follow",
        _ => "hot",
    };
    let mut path = format!("/api/videos/feed?mode={mode}");
    if let Some(cursor) = params.get("cursor").filter(|v| !v.is_null()) {
        path.push_str(&format!("&cursor={cursor}"));
    }
    if let Some(limit) = params.get("limit").and_then(Value::as_i64) {
        path.push_str(&format!("&limit={limit}"));
    }
    authed_json(state, "GET", &path, None, None)
}

/// live.state：LiveState 原样透传。
fn act_live_state<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let channel = live_channel_param(req)?;
    authed_json(state, "GET", &format!("/api/live/{channel}"), None, None)
}

/// live.start：{recording} → {session, ingest_url, ingest_token}。
fn act_live_start<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let channel = live_channel_param(req)?;
    let recording = req
        .params
        .get("recording")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let body = json!({ "recording": recording });
    authed_json(
        state,
        "POST",
        &format!("/api/live/{channel}/start"),
        Some(&body),
        req.idempotency_key.as_deref(),
    )
}

/// live.stop：204 → {}。
fn act_live_stop<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let channel = live_channel_param(req)?;
    authed_json(
        state,
        "POST",
        &format!("/api/live/{channel}/stop"),
        None,
        req.idempotency_key.as_deref(),
    )
}

/// live.join：{viewers}。
fn act_live_join<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let channel = live_channel_param(req)?;
    authed_json(
        state,
        "POST",
        &format!("/api/live/{channel}/join"),
        None,
        req.idempotency_key.as_deref(),
    )
}

/// live.leave：{viewers}。
fn act_live_leave<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let channel = live_channel_param(req)?;
    authed_json(
        state,
        "POST",
        &format!("/api/live/{channel}/leave"),
        None,
        req.idempotency_key.as_deref(),
    )
}

/// live.danmaku：{text} → {delivered}。
fn act_live_danmaku<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let channel = live_channel_param(req)?;
    let text = param_str(&req.params, "text")?;
    let body = json!({ "text": text });
    authed_json(
        state,
        "POST",
        &format!("/api/live/{channel}/danmaku"),
        Some(&body),
        req.idempotency_key.as_deref(),
    )
}