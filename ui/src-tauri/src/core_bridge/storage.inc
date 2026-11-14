fn act_secrets_status<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let lock = core.lock_state();
    Ok(json!({
        "locked": lock.locked,
        "os_backed": false,
        "passphrase_migration": if lock.passcode_sha.is_some() { "migrated" } else { "available" },
        "note": "桌面锁口令存本地库（sha256）；OS 密钥库由 G02 提供",
    }))
}

fn act_secrets_lock<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    core.lock_state().locked = true;
    Ok(json!({ "locked": true }))
}

fn act_secrets_migrate<R: tauri::Runtime>(
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
    let current = str_param("current_password");
    let next = str_param("new_password");
    let expected = core.lock_state().passcode_sha.clone();
    if let Some(expected_sha) = expected {
        if short_hash_full(&current) != expected_sha {
            return Err(BridgeError::Validation("当前口令不正确".into()));
        }
    }
    if next.len() < 8 {
        return Err(BridgeError::Validation("新口令至少 8 位".into()));
    }
    core.lock_state().passcode_sha = Some(short_hash_full(&next));
    Ok(json!({ "migrated": true, "os_backed": false }))
}

fn act_storage_status<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let bytes = with_local(state, |s| {
        Ok(std::fs::metadata(s.path()).map(|m| m.len()).unwrap_or(0))
    })
    .unwrap_or(0);
    Ok(json!({ "cache_bytes": 0, "message_bytes": bytes, "media_bytes": 0 }))
}

fn act_storage_cache_clear<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    Ok(json!({ "freed_bytes": 0, "kept": ["未发送 outbox", "草稿", "密钥"] }))
}

fn act_backup_create<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let pending = outbox_pending(state);
    if pending > 0 {
        return Err(BridgeError::Conflict(
            "还有未发送消息，先发完或清空再备份".into(),
        ));
    }
    // WAL checkpoint 后原文件即完整快照（secure_delete 也不留正文残余）。
    with_local(state, |s| {
        let _ = s.lock().execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        Ok(())
    })?;
    let src = with_local(state, |s| Ok(s.path().to_path_buf()))?;
    let bytes =
        std::fs::read(&src).map_err(|e| BridgeError::Other(format!("读本地库失败：{e}")))?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(BridgeError::Dependency(
            "备份超过内联上限（64MB），大库备份走文件导出（G02）".into(),
        ));
    }
    let digest = hex::encode(Sha256::digest(&bytes));
    let info = json!({
        "file_name": format!("wxlike-backup-{}.db", wxlike_core::types::now_ms()),
        "size": bytes.len(),
        "digest": digest,
        "created_at_ms": iso_ms(wxlike_core::types::now_ms() as i64),
    });
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(json!({ "info": info, "base64": b64 }))
}

fn act_backup_verify<R: tauri::Runtime>(
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
    use base64::Engine as _;
    let raw = str_param("base64");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&raw)
        .map_err(|_| BridgeError::Validation("文件不是有效的备份".into()))?;
    let digest = hex::encode(Sha256::digest(&bytes));
    if !bytes.starts_with(b"SQLite format 3") {
        return Ok(json!({ "valid": false, "reason": "备份格式不匹配", "digest": digest }));
    }
    Ok(json!({ "valid": true, "digest": digest }))
}

fn act_backup_import<R: tauri::Runtime>(
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
    if str_param("confirmation") != "IMPORT" {
        return Err(BridgeError::Confirmation(
            "导入会覆盖本地数据，需要再次确认".into(),
        ));
    }
    if outbox_pending(state) > 0 {
        return Err(BridgeError::Conflict("还有未发送消息，导入前先清空".into()));
    }
    use base64::Engine as _;
    let raw = str_param("base64");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&raw)
        .map_err(|_| BridgeError::Validation("备份文件损坏".into()))?;
    if !bytes.starts_with(b"SQLite format 3") {
        return Err(BridgeError::Validation("备份与当前账号不匹配".into()));
    }
    let path = with_local(state, |s| Ok(s.path().to_path_buf()))?;
    let tmp = path.with_extension("import-tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| BridgeError::Other(format!("写导入文件失败：{e}")))?;
    std::fs::rename(&tmp, &path).map_err(|e| BridgeError::Other(format!("替换本地库失败：{e}")))?;
    // 重开本地库（含出队队列与 kv）。
    state.reopen_local(&path)?;
    emit_core(app, "search.invalidated", "me", json!({}));
    Ok(json!({ "imported": true }))
}

fn act_app_info<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    Ok(json!({
        "name": "wxlike",
        "version": env!("CARGO_PKG_VERSION"),
        "commit": option_env!("WXLIKE_GIT_SHA").unwrap_or("unknown"),
        "built_at": iso_ms(option_env!("WXLIKE_BUILD_TIME").and_then(|t| t.parse().ok()).unwrap_or(0)),
        "licenses": [
            { "name": "wxlike", "license": "AGPL-3.0-only" },
            { "name": "tauri", "license": "MIT/Apache-2.0" },
            { "name": "quinn (QUIC)", "license": "MIT/Apache-2.0" },
        ],
    }))
}

// ---------------------------------------------------------------------------
// 钱包（#293）：认证传输 + 域处理器
//
// 这七条 action 是"核心是唯一会话归属地"的第一块竖切：React 侧的钱包/商务页
// 过去直接 `fetch(baseUrl() + "/api/wallet/…")`，而 `setBaseUrl`/`saveSession`
// 没有任何调用点，所以那些请求**根本没带凭据**（#293 的缺陷本体）。改由桥代持
// 会话之后，webview 不再发 HTTP，token 也不出 Rust。
// ---------------------------------------------------------------------------

