// ---------------------------------------------------------------------------
// 通话域（D6 逐域退役）：call.* 全部经认证桥，UI 侧不再自己发 HTTP。
//
// 与钱包域同口径：每条臂都走 [`authed_json`]——桥里唯一允许直连平台的出口，
// base/Bearer 由 `wxlike-client` 的 `Http` 统一挂（凭据只在 Rust 侧，没有任何
// action 会把 token 回给 webview）。服务端路由见 `crates/platform/src/call.rs`。
//
// 控制类端点（start/join/leave/mute/lock/record/hand/kick）服务端回 204 No
// Content：`reply_json` 把空体读成 `{}`，`authed_json` 只在非 2xx 时报错，所以
// 204 成功 → `Ok({})`。mute-all 回 `{affected}`、signal.post 回 `{dropped}`、
// signal.take 回 `{signals}`，都原样透传，不编字段。
// ---------------------------------------------------------------------------

/// 会话 id 进 URL 路径，先做安全校验（空串/`/`/`..` 一律拒）。
fn call_conv(req: &CoreRequest) -> Result<String, BridgeError> {
    param_path_id(&req.params, "conv")
}

fn act_call_state<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let conv = call_conv(req)?;
    // 服务端形状 {conv_id, live, host, locked, recording, participants} 原样透传；
    // 投影成 UI 的 CallState（conv/state/participants）由前端路由层负责。
    authed_json(state, "GET", &format!("/api/call/{conv}"), None, None)
}

fn act_call_start<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let conv = call_conv(req)?;
    // 空 body 合法（默认容量）；给了 capacity 才带。不替调用方编容量。
    let body = match req.params.get("capacity").filter(|v| !v.is_null()) {
        Some(cap) => json!({ "capacity": cap }),
        None => json!({}),
    };
    authed_json(
        state,
        "POST",
        &format!("/api/call/{conv}/start"),
        Some(&body),
        req.idempotency_key.as_deref(),
    )
}

/// join/leave 同形：无 body 的 POST，204 → {}。
fn act_call_join<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let conv = call_conv(req)?;
    authed_json(
        state,
        "POST",
        &format!("/api/call/{conv}/join"),
        None,
        req.idempotency_key.as_deref(),
    )
}

fn act_call_leave<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let conv = call_conv(req)?;
    authed_json(
        state,
        "POST",
        &format!("/api/call/{conv}/leave"),
        None,
        req.idempotency_key.as_deref(),
    )
}

/// mute：track/muted 必填；target 可选——**不兜默认**。旧前端单人静音不传
/// target，服务端按 422 拒绝；桥保持同一行为（透传缺省），不在这里替它补一个。
fn act_call_mute<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let conv = call_conv(req)?;
    let params = &req.params;
    let track = param_str(params, "track")?;
    let muted = params
        .get("muted")
        .and_then(Value::as_bool)
        .ok_or_else(|| missing_param("muted"))?;
    let mut body = json!({ "track": track, "muted": muted });
    if let Some(target) = params.get("target").and_then(Value::as_str) {
        body["target"] = json!(target);
    }
    authed_json(
        state,
        "POST",
        &format!("/api/call/{conv}/mute"),
        Some(&body),
        req.idempotency_key.as_deref(),
    )
}

/// mute-all：全员静音，不需要 target；服务端回 {affected}。
fn act_call_mute_all<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let conv = call_conv(req)?;
    let params = &req.params;
    let track = param_str(params, "track")?;
    let muted = params
        .get("muted")
        .and_then(Value::as_bool)
        .ok_or_else(|| missing_param("muted"))?;
    let body = json!({ "track": track, "muted": muted });
    authed_json(
        state,
        "POST",
        &format!("/api/call/{conv}/mute-all"),
        Some(&body),
        req.idempotency_key.as_deref(),
    )
}

/// lock/record/hand 同形：{value: bool}，204 → {}。
fn call_toggle_body(req: &CoreRequest) -> Result<Value, BridgeError> {
    let value = req
        .params
        .get("value")
        .and_then(Value::as_bool)
        .ok_or_else(|| missing_param("value"))?;
    Ok(json!({ "value": value }))
}

fn act_call_lock<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let conv = call_conv(req)?;
    let body = call_toggle_body(req)?;
    authed_json(
        state,
        "POST",
        &format!("/api/call/{conv}/lock"),
        Some(&body),
        req.idempotency_key.as_deref(),
    )
}

fn act_call_record<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let conv = call_conv(req)?;
    let body = call_toggle_body(req)?;
    authed_json(
        state,
        "POST",
        &format!("/api/call/{conv}/record"),
        Some(&body),
        req.idempotency_key.as_deref(),
    )
}

fn act_call_hand<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let conv = call_conv(req)?;
    let body = call_toggle_body(req)?;
    authed_json(
        state,
        "POST",
        &format!("/api/call/{conv}/hand"),
        Some(&body),
        req.idempotency_key.as_deref(),
    )
}

fn act_call_kick<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let conv = call_conv(req)?;
    let target = param_str(&req.params, "target")?;
    let body = json!({ "target": target });
    authed_json(
        state,
        "POST",
        &format!("/api/call/{conv}/kick"),
        Some(&body),
        req.idempotency_key.as_deref(),
    )
}

/// 投递一条信令（offer/answer/ICE/bye）。信封是不透明的 wxlike_rtc::signal::Signal，
/// 桥原样转发，绝不改写内容；`from` 由服务端改写，别信客户端。
fn act_call_signal_post<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let conv = call_conv(req)?;
    let signal = req
        .params
        .get("signal")
        .filter(|v| v.is_object())
        .ok_or_else(|| missing_param("signal"))?;
    authed_json(
        state,
        "POST",
        &format!("/api/call/{conv}/signal"),
        Some(signal),
        None,
    )
}

/// 取走发给我的信令（读即清，服务端队列有界）。回 {signals}。
fn act_call_signal_take<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let conv = call_conv(req)?;
    authed_json(state, "GET", &format!("/api/call/{conv}/signal"), None, None)
}