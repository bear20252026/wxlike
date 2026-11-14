fn act_identity_session_projection<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    session_projection(state, core)
}

fn act_identity_session_login<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let username = str_param("username");
    let password = str_param("password");
    if username.trim().is_empty() || password.is_empty() {
        return Err(BridgeError::Validation("账号或口令为空".into()));
    }
    state.login(&username, &password)?;
    open_session(app, state, core)
}

fn act_identity_session_register<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let username = str_param("username");
    let password = str_param("password");
    let display_name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if username.trim().is_empty() || password.len() < 6 {
        return Err(BridgeError::Validation(
            "账号不能为空，口令至少 6 位".into(),
        ));
    }
    state.register(&username, &password)?;
    if !display_name.is_empty() {
        let _ = state.update_profile(Some(&display_name), None, None);
    }
    open_session(app, state, core)
}

fn act_identity_session_logout<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    // 台账第 2 条：以前这里 `let _ = state.logout();` 把服务端撤销的成败丢在地上，
    // 然后直接回 `{"revoked": true}`——token 在服务端照样能用。现在必须回查确证。
    sign_out_confirmed(app, state, core)?;
    Ok(json!({ "revoked": true, "server_revoked": true }))
}

fn act_identity_session_restore<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    ensure_local(app, state)?;
    let token = with_local(state, |s| Ok(s.kv_get("auth_token")))
        .ok()
        .flatten();
    match token {
        Some(token) if !token.is_empty() => {
            state.set_auth_token(&token);
            open_session(app, state, core)
        }
        _ => Err(BridgeError::Unauth("没有可恢复的会话".into())),
    }
}

fn act_security_lock<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let account = core.account();
    if account.is_none() {
        return Err(BridgeError::Unauth("尚未登录".into()));
    }
    core.lock_state().locked = true;
    let projection = json!({ "phase": "locked", "account": account, "lock_reason": "manual" });
    emit_core(app, "session.state", "session", projection.clone());
    Ok(projection)
}

fn act_security_unlock<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let account = core.account();
    if account.is_none() {
        return Err(BridgeError::Unauth("会话已过期".into()));
    }
    let password = str_param("password");
    let expected = core.lock_state().passcode_sha.clone();
    if let Some(expected_sha) = expected {
        if short_hash_full(&password) != expected_sha {
            return Err(BridgeError::Unauth("口令不正确".into()));
        }
    }
    core.lock_state().locked = false;
    let projection = json!({ "phase": "authenticated", "account": account });
    emit_core(app, "session.state", "session", projection.clone());
    Ok(projection)
}

fn act_identity_qr_login_create<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    qr_create(app, state, core)
}

fn act_identity_qr_login_cancel<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    // 台账第 1 条：只停本地轮询 ≠ 取消。手机侧在那张票的有效期内点"同意"照样能
    // 把这次已"取消"的登录做成。所以取消必须以"服务端状态确证是 REJECTED"为准。
    let ticket = qr_cancel_target(core, req)?;
    revoke_qr_ticket(state, &ticket)?;
    stop_qr_poll(core);
    let challenge = qr_challenge(&ticket, "REJECTED", &Value::Null);
    emit_core(app, "qr.state", &ticket, challenge.clone());
    Ok(json!({
        "cancelled": true,
        "server_revoked": true,
        "challenge_id": ticket,
        "state": "REJECTED",
    }))
}

/// 要撤销哪张票据：参数点名优先，其次桥内记着的那张，两处都没有就报错。
///
/// 不许对着"空票据"发撤销请求（那正是 `unwrap_or_default()` 造出来的形状）。
fn qr_cancel_target(core: &CoreState, req: &CoreRequest) -> Result<String, BridgeError> {
    for key in ["challenge_id", "ticket_id"] {
        if let Ok(id) = required_str(&req.params, key) {
            return Ok(id);
        }
    }
    core.qr_active
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| {
            BridgeError::Validation(
                "没有可取消的扫码票据：参数 challenge_id 缺失，桥内也没有进行中的票据".into(),
            )
        })
}

/// 服务端撤销一张票据并回查确证。
///
/// 服务端只有 `POST /api/qr/tickets/{id}/reject` 这一条撤销面（HTTP 臂在
/// `crates/platform/src/qr_login.rs` 的 `reject_ticket` → `decide`），而它要求
/// 调用者就是**扫码那一侧**的账号（域语义在
/// `crates/features/identity/src/qr_login.rs::decide`：票据必须处在 `scanned`
/// 且 `scanned_by == 调用者`）。回查用的 `GET /api/qr/tickets/{id}` 反过来又要
/// 求调用者是**创建者**——所以桌面这条会话只有"自己扫了自己建的票"这一种形状
/// 才有权撤销。桌面自建的票在"还没人扫"这个阶段没有任何可撤销端点，那种情况
/// 一律报错并保留本地轮询，绝不回 `cancelled: true`，因为停轮询拦不住手机点
/// "同意"。
///
/// 真正的服务端撤销需要补一个"创建者撤票"端点（已列待办，见本卡汇报）。
fn revoke_qr_ticket(state: &AppState, ticket: &str) -> Result<(), BridgeError> {
    let reject =
        state
            .http()
            .expect_json("POST", &format!("/api/qr/tickets/{ticket}/reject"), None);
    let now = qr_ticket_state(state, ticket);
    if now == "REJECTED" {
        return Ok(());
    }
    Err(revoke_failure(ticket, &now, reject))
}

/// 撤销没生效时按"服务端到底怎么答的"分档说实话（台账第 1 条）。
///
/// * 拿不到 HTTP 状态 ⇒ 请求压根没送到（链路问题）→ `OFFLINE`，可重试。
/// * 非 2xx ⇒ 服务端没挂这条路由（404）或不给这条会话撤（403/409）→
///   `NOT_IMPLEMENTED`：重新登录也变不出撤销面，手机侧仍然能批准。
/// * 2xx 但回查不是 `REJECTED` ⇒ 收了请求没做 → `DEPENDENCY_UNAVAILABLE`
///   （与登出"装了个成功其实没做"同一档）。
fn revoke_failure(ticket: &str, now: &str, reject: Result<Value, String>) -> BridgeError {
    let context = format!(
        "票据 {ticket} 当前状态 {now}：手机侧仍可批准这次登录，取消未生效（本地轮询也没停）"
    );
    let Err(err) = reject else {
        return BridgeError::Dependency(format!(
            "服务端收了撤销请求，但回查的票据状态不是 REJECTED；{context}"
        ));
    };
    match http_status(&err) {
        Some(status) => BridgeError::NotImpl(format!(
            "服务端撤销面不可用（HTTP {status}：{err}）——这条会话不是扫码那一侧的账号，\
             服务端也没有创建者撤票端点，需要手机侧操作；{context}"
        )),
        None => BridgeError::Offline(format!("撤销请求没送到服务端（{err}）；{context}")),
    }
}

/// `expect_json` 的非 2xx 错误尾巴是 `"… (NNN)"`。取不到 4xx/5xx 就说明响应
/// 根本没收到（那是链路问题，不是服务端拒绝）——两者对用户的意义完全不同：
/// 一个能重试，一个重试一万次也一样。
fn http_status(err: &str) -> Option<u16> {
    let open = err.rfind('(')? + 1;
    let close = open + err[open..].find(')')?;
    let raw: u16 = err[open..close].trim().parse().ok()?;
    (400..600).contains(&raw).then_some(raw)
}

/// 服务端记着的票据状态（查不到或缺字段都算"不可知"，不许兜成某个状态名）。
fn qr_ticket_state(state: &AppState, ticket: &str) -> String {
    state
        .http()
        .expect_json("GET", &format!("/api/qr/tickets/{ticket}"), None)
        .ok()
        .and_then(|v| v["state"].as_str().map(str::to_string))
        .unwrap_or_else(|| "UNKNOWN".into())
}

fn act_identity_qr_login_switch_account<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = app;
    sign_out_confirmed(app, state, core)?;
    stop_qr_poll(core);
    Ok(json!({ "revoked": true, "server_revoked": true }))
}

fn act_sync_range_get<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    if core.account().is_none() {
        return Err(BridgeError::Unauth("尚未登录".into()));
    }
    let prefs = preferences_read(state);
    let days = prefs["sync_range_days"].as_i64().unwrap_or(7);
    Ok(json!({
        "days": days,
        "incremental_default_days": 7,
        "can_view_further": days != 7,
    }))
}

fn act_sync_range_set<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    if core.account().is_none() {
        return Err(BridgeError::Unauth("尚未登录".into()));
    }
    let days = params.get("days").and_then(Value::as_i64).unwrap_or(-1);
    if ![0, 7, SYNC_RANGE_MONTH_DAYS, SYNC_RANGE_ALL_DAYS].contains(&days) {
        return Err(BridgeError::Validation("同步范围不合法".into()));
    }
    let mut prefs = preferences_read(state);
    prefs["sync_range_days"] = json!(days);
    preferences_write(app, state, prefs)
}

fn stop_qr_poll(core: &CoreState) {
    core.qr_poll_generation.fetch_add(1, Ordering::SeqCst);
    *core
        .qr_active
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
}

fn qr_create<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
) -> Result<Value, BridgeError> {
    // 已有未结束的票据：直接复用（fixture 同语义）。
    if let Some(active) = core
        .qr_active
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
    {
        // 同上：http 守卫先落局部，别活在 if-let 条件里。
        let existing = state
            .http()
            .expect_json("GET", &format!("/api/qr/tickets/{active}"), None);
        if let Ok(reply) = existing {
            let state_str = reply["state"].as_str().unwrap_or("CREATED");
            if !matches!(state_str, "APPROVED" | "EXPIRED" | "REJECTED") {
                return Ok(qr_challenge(&active, state_str, &reply));
            }
        }
    }
    let reply = state
        .http()
        .expect_json("POST", "/api/qr/tickets", Some(&json!({ "mode": "login" })))
        .map_err(|e| {
            if e.contains("404") {
                BridgeError::NotImpl("服务端未挂载 QR 登录路由（等待 G14 集成）".into())
            } else {
                BridgeError::from(e)
            }
        })?;
    let ticket = reply["ticket_id"].as_str().unwrap_or_default().to_string();
    if ticket.is_empty() {
        return Err(BridgeError::NotImpl(
            "服务端 QR 票据响应缺少 ticket_id".into(),
        ));
    }
    let challenge = qr_challenge(&ticket, "CREATED", &reply);
    // 轮询线程：票据状态变化 → qr.state 事件（代次守卫：新 create/取消让旧线程退场）。
    stop_qr_poll(core);
    let generation = core.qr_poll_generation.load(Ordering::SeqCst);
    *core
        .qr_active
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(ticket.clone());
    spawn_qr_poll(app.clone(), state, &ticket, generation, challenge.clone());
    Ok(challenge)
}
/// 轮询线程：票据状态变化 → `qr.state` 事件（代次守卫：新 create/取消让旧线程退场）。
fn spawn_qr_poll<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: &AppState,
    ticket: &str,
    generation: u64,
    challenge: Value,
) {
    let ticket_for_poll = ticket.to_string();
    std::thread::spawn(move || {
        // 走 emit_core（而不是自己拼一份信封）：补发日志必须是事件流的完整真相，
        // 第二条平行发射手路就是 #296 那类"目录/实现漂移"的温床。
        emit_core(&app, "qr.state", &ticket_for_poll, challenge);
        poll_qr_ticket(&app, &ticket_for_poll, generation);
    });
    // state 参数为对称签名保留（轮询线程内经 app.state() 取用）。
    let _ = state;
}

fn poll_qr_ticket<R: tauri::Runtime>(app: &tauri::AppHandle<R>, ticket: &str, generation: u64) {
    let mut last = String::new();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(QR_POLL_INTERVAL_MS));
        if app
            .state::<CoreState>()
            .qr_poll_generation
            .load(Ordering::SeqCst)
            != generation
        {
            break;
        }
        let state = app.state::<AppState>();
        let Ok(reply) = state
            .http()
            .expect_json("GET", &format!("/api/qr/tickets/{ticket}"), None)
        else {
            continue;
        };
        let now = reply["state"].as_str().unwrap_or_default().to_string();
        if now.is_empty() || now == last {
            continue;
        }
        last = now.clone();
        let payload = qr_challenge(ticket, &now, &reply);
        emit_core(app, "qr.state", ticket, payload);
        if matches!(now.as_str(), "APPROVED" | "EXPIRED" | "REJECTED") {
            break;
        }
    }
}

fn qr_challenge(ticket: &str, state: &str, reply: &Value) -> Value {
    json!({
        "challenge_id": ticket,
        "state": state,
        "payload": reply
            .get("payload")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("wxlike://login/{ticket}")),
        "expires_at_ms": iso_ms(
            reply.get("expires_at_ms")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| wxlike_core::types::now_ms() as i64 + 120_000)
        ),
        "account_hint": reply.get("account_hint").cloned().unwrap_or(Value::Null),
    })
}

