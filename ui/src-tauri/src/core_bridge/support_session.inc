// support 碎片 4/6：会话流程与可判定撤销（open_session、ensure_local、
// with_local、session_is_live、sign_out_confirmed）。由 support.inc 按序 include。

// ---------------------------------------------------------------------------
// 会话流程（登录 → 本地库 → hub → 泵）
// ---------------------------------------------------------------------------

/// 打开完整会话：本地库 + TOFU + hub 连接 + 双泵。登录与恢复共用。
fn open_session<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
) -> Result<Value, BridgeError> {
    let me = state.me()?;
    let user_id = me["user_id"].as_str().unwrap_or_default().to_string();
    let name = me["name"].as_str().unwrap_or_default().to_string();
    let avatar = me["avatar"].as_str().map(str::to_string);
    if user_id.is_empty() {
        return Err(BridgeError::Other("服务端未返回 user_id".into()));
    }
    // 本地库：登录即打开（消息镜像 + 出队队列 + kv 都在这一个文件）。
    // `WXLIKE_DATA_DIR` 覆盖：便携安装与 e2e 测试（同一进程多账号隔离）
    // 都需要显式指定数据目录，而不是各平台 AppData 约定路径。
    let data_dir = match std::env::var("WXLIKE_DATA_DIR") {
        Ok(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
        _ => app
            .path()
            .app_data_dir()
            .map_err(|e| BridgeError::Other(format!("取数据目录失败：{e}")))?,
    };
    if !state.local_ready() {
        state.local_open(&data_dir.join("messages.db"))?;
    }
    // 会话凭据落 kv：重启后 restore 用。
    if let Some(token) = state.auth_token() {
        let _ = with_local(state, |store| {
            store.kv_put("auth_token", &token)?;
            store.kv_put(
                "account",
                &json!({"user_id": user_id, "name": name, "avatar": avatar}).to_string(),
            )
        });
    }
    connect_hub_best_effort(app, state, &data_dir);
    let account = json!({
        "user_id": user_id,
        "name": name,
        "avatar": avatar,
        "device_id": "desktop-tauri",
        "session_id": short_hash(state.auth_token().as_deref().unwrap_or("")),
    });
    *core.account.lock().unwrap_or_else(PoisonError::into_inner) = Some(account.clone());
    let projection = json!({ "phase": "authenticated", "account": account });
    emit_core(app, "session.state", &user_id, projection.clone());
    Ok(projection)
}

/// hub 连接（尽力而为）：`/endpoints` 给出 QUIC 端口，TOFU 固定证书后
/// 连接并起双泵；连不上不阻断登录——消息进 outbox，重连后由泵补发。
///
/// 死锁备忘：`state.http()` 的临时 MutexGuard 不能活在 if-let 条件里，
/// 否则后面 `auth_token()` 再锁同一把锁就是自己跟自己死锁（审计 R5 实锤）。
fn connect_hub_best_effort<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    data_dir: &std::path::Path,
) {
    let endpoints_quic = state
        .http()
        .expect_json("GET", "/endpoints", None)
        .ok()
        .and_then(|v| v["quic"].as_str().map(str::to_string));
    let Some(quic) = endpoints_quic else {
        return;
    };
    let pins = data_dir.join("tofu-pins.json");
    if state
        .connect_hub(&quic, &state.auth_token().unwrap_or_default(), &pins)
        .is_ok()
    {
        state.start_outbox_pump(app.clone());
        // 收包泵自带代次守卫：重复登录/重连不会叠出双泵。
        let _ = state.start_recv_pump(app.clone());
    }
}

fn short_hash(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    hex::encode(&digest[..6])
}

fn short_hash_full(input: &str) -> String {
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// 确保本地库已打开（restore/凭据读取的前置条件；重启后第一个被调用的
/// action 往往是 restore，此时还没人开过库）。
fn ensure_local<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
) -> Result<(), BridgeError> {
    if state.local_ready() {
        return Ok(());
    }
    let data_dir = match std::env::var("WXLIKE_DATA_DIR") {
        Ok(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
        _ => app
            .path()
            .app_data_dir()
            .map_err(|e| BridgeError::Other(format!("取数据目录失败：{e}")))?,
    };
    state
        .local_open(&data_dir.join("messages.db"))
        .map_err(BridgeError::Other)
}

fn with_local<T>(
    state: &AppState,
    f: impl FnOnce(&LocalStore) -> anyhow::Result<T>,
) -> Result<T, BridgeError> {
    let slot = state.local_arc();
    let guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
    match guard.as_ref() {
        Some(store) => f(store).map_err(|e| BridgeError::Other(format!("本地库操作失败：{e:#}"))),
        None => Err(BridgeError::Other("本地库未打开（先登录）".into())),
    }
}

// ---------------------------------------------------------------------------
// 可判定的撤销（#293 台账 1/2/3 条共用）
// ---------------------------------------------------------------------------

/// 必填字符串参数：缺失或空白**一律报错**。
///
/// 台账第 3 条的成因就是 `params.get(key).and_then(Value::as_str)
/// .unwrap_or_default()`：参数没给时拿到空串，`remove("")` 必然落空，
/// 返回值却照样是 `cancelled: true`。缺参数是调用方的错，要如实拒绝，
/// 不许造一个"看着像参数"的值往下传。
fn required_str(params: &Value, key: &str) -> Result<String, BridgeError> {
    let raw = params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::Validation(format!("缺少参数 {key}")))?;
    let value = raw.trim();
    if value.is_empty() {
        return Err(BridgeError::Validation(format!("参数 {key} 不能为空")));
    }
    Ok(value.to_string())
}

/// 拿一张凭据真打一次需要鉴权的端点：`true` = 服务端仍然认它。
///
/// 这是"撤销到底生效没有"的唯一判据：`/api/logout` 回 2xx 只说明服务端收到了
/// 请求，不说明那行会话真的删了。用独立的 `Http` 探，避免动当前会话的凭据槽。
fn session_is_live(state: &AppState, token: &str) -> bool {
    let mut probe = Http::new(state.http_base());
    probe.token = Some(token.to_string());
    probe.expect_json("GET", "/api/me", None).is_ok()
}

/// 服务端登出 + 回查确证：只有"服务端确实不再认这张凭据"才算成功。
///
/// 撤销没生效时**不清本地**：把一张仍然生效的凭据从本地抹掉，等于让人
/// 连重试撤销的机会都没有（只剩"以为退出了"的假象）。
///
/// 这里走 `raw_json` 而不是 `AppState::logout()`：后者在 2xx 时顺手把本地
/// token 清了，而"2xx"恰恰是本项目要怀疑的那一步（收了请求 ≠ 撤了会话）。
/// 清本地由 [`sign_out_confirmed`] 在确证之后统一做。
fn revoke_session_on_server(state: &AppState, token: &str) -> Result<(), BridgeError> {
    if let Err(err) = state.raw_json("POST", "/api/logout", None) {
        // 401 = 服务端早就不认这张票了，等同已撤销（否则人会被锁在退不出的会话里）。
        if !err.contains("(401)") {
            return Err(BridgeError::from(format!("服务端登出失败：{err}")));
        }
    }
    if session_is_live(state, token) {
        return Err(BridgeError::Dependency(
            "服务端登出没有生效：旧凭据仍能调 /api/me，本地会话保持不变".into(),
        ));
    }
    Ok(())
}

/// 当前会话凭据 + 服务端登出确证；没有凭据时如实报错（没什么可撤销）。
fn sign_out_confirmed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
) -> Result<(), BridgeError> {
    match state.auth_token() {
        Some(token) => revoke_session_on_server(state, &token)?,
        None => {
            return Err(BridgeError::Unauth(
                "当前没有已登录的会话，没有凭据可撤销".into(),
            ))
        }
    }
    state.http().token = None;
    core.account
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    // kv 里那份凭据也要抹掉，否则 restore 会把这张已撤销的票再捡回来。
    let _ = with_local(state, |s| {
        s.kv_delete("auth_token")?;
        s.kv_delete("account")
    });
    emit_core(
        app,
        "session.state",
        "session",
        json!({ "phase": "signed_out" }),
    );
    Ok(())
}

