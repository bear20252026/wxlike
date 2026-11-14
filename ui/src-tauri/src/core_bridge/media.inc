fn act_media_upload_begin<R: tauri::Runtime>(
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
    if core.account().is_none() {
        return Err(BridgeError::Unauth("尚未登录".into()));
    }
    let size = params.get("size").and_then(Value::as_u64).unwrap_or(0);
    if size == 0 || size > 1024 * 1024 * 1024 {
        return Err(BridgeError::Validation("文件大小超出预算".into()));
    }
    let upload_id = format!("up-{}", wxlike_core::types::now_ms());
    let mime = str_param("mime");
    core.uploads
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            upload_id.clone(),
            UploadSession {
                name: str_param("name"),
                mime: if mime.is_empty() {
                    "application/octet-stream".into()
                } else {
                    mime
                },
                size,
                buf: Vec::with_capacity(size.min(8 * 1024 * 1024) as usize),
                last_index: u64::MAX,
            },
        );
    Ok(json!({ "upload_id": upload_id, "chunk_size": 4 * 1024 * 1024 }))
}

fn act_media_upload_part<R: tauri::Runtime>(
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
    let upload_id = str_param("upload_id");
    let mut uploads = core.uploads.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(upload) = uploads.get_mut(&upload_id) else {
        return Err(BridgeError::Validation("上传会话不存在".into()));
    };
    let index = params.get("index").and_then(Value::as_u64).unwrap_or(0);
    // 分片必须有序：乱序/重放直接拒（同片重试由幂等键保证，不靠这里宽容）。
    if upload.last_index != u64::MAX && index != upload.last_index + 1 {
        return Err(BridgeError::Conflict("分片乱序".into()));
    }
    let data = params
        .get("data_base64")
        .and_then(Value::as_str)
        .map(|s| base64::engine::general_purpose::STANDARD.decode(s))
        .transpose()
        .map_err(|e| BridgeError::Validation(format!("分片 base64 解不开: {e}")))?
        .unwrap_or_default();
    let declared = params.get("length").and_then(Value::as_u64).unwrap_or(0);
    if !data.is_empty() && data.len() as u64 != declared {
        return Err(BridgeError::Validation("分片长度与声明不符".into()));
    }
    upload.buf.extend_from_slice(&data);
    upload.last_index = index;
    if upload.buf.len() as u64 > upload.size {
        return Err(BridgeError::Validation("上传超出声明大小".into()));
    }
    Ok(json!({ "received": upload.buf.len(), "size": upload.size }))
}

fn act_media_upload_complete<R: tauri::Runtime>(
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
    let upload_id = str_param("upload_id");
    let upload = core
        .uploads
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&upload_id)
        .ok_or_else(|| BridgeError::Validation("上传会话不存在".into()))?;
    if upload.buf.len() as u64 != upload.size {
        return Err(BridgeError::Conflict("上传未完成".into()));
    }
    let sha256 = hex::encode(Sha256::digest(&upload.buf));
    let conv = params.get("conv_id").and_then(Value::as_str);
    let reply = state.upload_media(upload.buf, &upload.mime, Some(&sha256), conv, false)?;
    Ok(json!({
        "media_id": reply["media_id"].clone(),
        "name": upload.name,
        "size": upload.size,
        "mime": upload.mime,
        "digest": sha256,
        "digest_algorithm": "SHA-256",
    }))
}

fn act_media_upload_cancel<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    _state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = app;
    // 台账第 3 条的旧形状：`unwrap_or_default()` 在没给 `upload_id` 时拿到空串，
    // `remove("")` 必然落空，返回值却照样是 `cancelled: true`。现在缺参数先报错；
    // "什么都没删掉"也要报错——删掉一张会话和没找到会话是两件不同的事。
    let upload_id = required_str(&req.params, "upload_id")?;
    let dropped = core
        .uploads
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&upload_id)
        .is_some();
    if !dropped {
        return Err(BridgeError::Validation(format!(
            "上传会话 {upload_id} 不存在（已完成、已取消，或 id 不对），没什么可取消"
        )));
    }
    Ok(media_cancel_receipt(&upload_id))
}

/// 取消上传的**诚实回执**：丢掉的只是本地分片记账，服务端那条上传还没开始。
///
/// 分片只在这张表里攒字节（`media.upload.part` 一条 HTTP 都不发），全部到齐才由
/// `media.upload.complete` 一次性 POST 上去。所以这里既没有可通知的服务端会话、
/// 也没有在途请求可中止——把这件事写成 `local_only` / `server_notified` 两个
/// 字段，而不是一个含糊的 `cancelled: true`（后者读起来像"上传被停了"，而上传
/// 根本还没发生过）。想真中止"已经发出去的上传"，前提是先有服务端可寻址的
/// 上传会话（待办，见本卡汇报）。
fn media_cancel_receipt(upload_id: &str) -> Value {
    json!({
        "upload_id": upload_id,
        "local_only": true,
        "server_notified": false,
        "local_bookkeeping_dropped": true,
        "reason": "只停了本地记账：分片全在内存，服务端没有这条上传的会话可通知，\
                   也没有在途请求可中止",
    })
}

