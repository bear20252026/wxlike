fn act_preferences_get<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    Ok(preferences_read(state))
}

fn act_preferences_set<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let patch = params.get("patch").cloned().unwrap_or(Value::Null);
    if !patch.is_object() {
        return Err(BridgeError::Validation("缺少设置内容".into()));
    }
    let mut prefs = preferences_read(state);
    if let Some(obj) = patch.as_object() {
        for (k, v) in obj {
            prefs[k] = v.clone();
        }
    }
    preferences_write(app, state, prefs)
}

fn default_preferences() -> Value {
    json!({
        "theme": "system",
        "compact": false,
        "notification_preview": true,
        "enter_to_send": true,
        "sync_range_days": 7,
        "auto_login": true,
    })
}

fn preferences_read(state: &AppState) -> Value {
    with_local(state, |s| {
        Ok(s.kv_get("preferences")
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .unwrap_or_else(default_preferences))
    })
    .unwrap_or_else(|_| default_preferences())
}

fn preferences_write<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    prefs: Value,
) -> Result<Value, BridgeError> {
    with_local(state, |s| s.kv_put("preferences", &prefs.to_string()))?;
    if let Some(user_id) = prefs_get_user(state) {
        emit_core(app, "preferences.updated", &user_id, prefs.clone());
    }
    Ok(prefs)
}

fn prefs_get_user(state: &AppState) -> Option<String> {
    with_local(state, |s| {
        Ok(s.kv_get("account")
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .and_then(|a| a["user_id"].as_str().map(str::to_string)))
    })
    .ok()
    .flatten()
}

// ---------------------------------------------------------------------------
// QR 登录
// ---------------------------------------------------------------------------

