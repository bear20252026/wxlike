fn act_messaging_conversations_list<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    conversations_list(state, params)
}

fn act_messaging_messages_page<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    messages_page(state, params)
}

fn act_messaging_send<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    message_send(app, state, params)
}

fn act_messaging_retry<R: tauri::Runtime>(
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
    let msg_id = str_param("msg_id");
    if msg_id.is_empty() {
        return Err(BridgeError::Validation("消息不存在".into()));
    }
    let ob = state
        .outbox()
        .ok_or_else(|| BridgeError::Other("本地库未打开".into()))?;
    ob.requeue(&msg_id, wxlike_core::types::now_ms() as i64)
        .map_err(|e| BridgeError::Other(e.to_string()))?;
    Ok(json!({ "msg_id": msg_id, "state": "LOCAL_QUEUED" }))
}

fn act_messaging_recall<R: tauri::Runtime>(
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
    let conv_id = str_param("conv_id");
    let msg_id = str_param("msg_id");
    state.recall_msg(&conv_id, &msg_id)?;
    emit_core(
        app,
        "message.updated",
        &conv_id,
        json!({
            "msg_id": msg_id, "conv_id": conv_id,
            "body": { "kind": "system", "text": "你撤回了一条消息" },
            "state": "RECALLED", "recalled": true,
        }),
    );
    Ok(json!({ "recalled": true }))
}

fn act_messaging_forward<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    let msg_ids: Vec<String> = params
        .get("msg_ids")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let targets: Vec<String> = params
        .get("target_conv_ids")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if msg_ids.is_empty() || targets.is_empty() {
        return Err(BridgeError::Validation("转发参数不合法".into()));
    }
    let me = state.user_id()?;
    let mut created = 0usize;
    for target in &targets {
        let to = recipients_of(target, &me);
        for msg_id in &msg_ids {
            let item = json!({ "conv_id": target, "msg_id": msg_id });
            state.forward_message(target, to.clone(), &item)?;
            created += 1;
        }
    }
    Ok(json!({ "created": created }))
}

fn act_messaging_read<R: tauri::Runtime>(
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
    let conv_id = str_param("conv_id");
    state.http().mark_conv_read(&conv_id)?;
    emit_core(
        app,
        "conversation.upsert",
        &conv_id,
        json!({ "conv_id": conv_id, "unread": 0 }),
    );
    emit_core(app, "unread.updated", "me", json!({ "total": 0 }));
    Ok(json!({ "unread": 0 }))
}

fn act_messaging_conversation_pin<R: tauri::Runtime>(
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
    let conv_id = str_param("conv_id");
    let value = params
        .get("value")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let key = format!("conv_flags:{conv_id}");
    let mut flags = with_local(state, |s| {
        Ok(s.kv_get(&key)
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .unwrap_or(json!({})))
    })?;
    let field = if req.action_id.ends_with("pin") {
        "pinned"
    } else {
        "muted"
    };
    flags[field] = json!(value);
    with_local(state, |s| s.kv_put(&key, &flags.to_string()))?;
    let mut conv = conversation_summary(state, &conv_id)?;
    // 投影补齐另一侧标志：kv 里没写过的字段必须是 bool 而不是 null，
    // 否则契约里的 Conversation.pinned/muted（boolean）会被写成 null
    // （conversation-list.tsx 读 conv.pinned 时就是看这个字段）。
    conv["pinned"] = json!(flags["pinned"].as_bool().unwrap_or(false));
    conv["muted"] = json!(flags["muted"].as_bool().unwrap_or(false));
    emit_core(app, "conversation.upsert", &conv_id, conv.clone());
    Ok(conv)
}

fn act_messaging_conversation_read_all<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let convs = with_local(state, |s| s.conversations())?;
    for (conv, _, _) in convs {
        let unread = state
            .http()
            .conv_unread(&conv)
            .ok()
            .and_then(|v| v["unread"].as_i64())
            .unwrap_or(0);
        if unread > 0 {
            state.http().mark_conv_read(&conv)?;
            emit_core(
                app,
                "conversation.upsert",
                &conv,
                json!({ "conv_id": conv, "unread": 0 }),
            );
        }
    }
    emit_core(app, "unread.updated", "me", json!({ "total": 0 }));
    Ok(json!({ "total": 0 }))
}

fn act_messaging_conversation_ensure<R: tauri::Runtime>(
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
    let peer = str_param("user_id");
    let me = state.user_id()?;
    let conv_id = wxlike_client::direct_conv_id(&me, &peer);
    let mut conv = conversation_summary(state, &conv_id)?;
    conv["peer_user_id"] = json!(peer);
    emit_core(app, "conversation.upsert", &conv_id, conv.clone());
    Ok(conv)
}

fn act_messaging_draft_get<R: tauri::Runtime>(
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
    let conv_id = str_param("conv_id");
    let text = with_local(state, |s| {
        Ok(s.kv_get(&format!("draft:{conv_id}")).unwrap_or_default())
    })?;
    Ok(json!({ "text": text }))
}

fn act_messaging_draft_save<R: tauri::Runtime>(
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
    let conv_id = str_param("conv_id");
    let text = str_param("text");
    with_local(state, |s| {
        if text.is_empty() {
            s.kv_delete(&format!("draft:{conv_id}"))
        } else {
            s.kv_put(&format!("draft:{conv_id}"), &text)
        }
    })?;
    Ok(json!({ "saved": true }))
}

fn act_messaging_draft_clear<R: tauri::Runtime>(
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
    let conv_id = str_param("conv_id");
    with_local(state, |s| s.kv_delete(&format!("draft:{conv_id}")))?;
    Ok(json!({ "cleared": true }))
}

fn outbox_pending(state: &AppState) -> i64 {
    state
        .outbox()
        .and_then(|ob| ob.pending_count().ok())
        .map(|n| n as i64)
        .unwrap_or(0)
}

fn session_projection(state: &AppState, core: &CoreState) -> Result<Value, BridgeError> {
    // 有凭据 → 服务端确证；凭据缺失 → signed_out；锁优先于一切。
    // （本地库没开时读不到 kv：投影如实说 signed_out，restore 负责开库。）
    if let Some(account) = core.account() {
        let phase = if core.lock_state().locked {
            "locked"
        } else {
            "authenticated"
        };
        let mut v = json!({ "phase": phase, "account": account });
        if phase == "locked" {
            v["lock_reason"] = json!("manual");
        }
        return Ok(v);
    }
    let token = with_local(state, |s| Ok(s.kv_get("auth_token")))
        .ok()
        .flatten()
        .unwrap_or_default();
    if token.is_empty() {
        return Ok(json!({ "phase": "signed_out" }));
    }
    state.set_auth_token(&token);
    let Ok(me) = state.me() else {
        return Ok(json!({ "phase": "signed_out" }));
    };
    let account = json!({
        "user_id": me["user_id"].clone(),
        "name": me["name"].clone(),
        "avatar": me["avatar"].clone(),
        "device_id": "desktop-tauri",
        "session_id": short_hash(&token),
    });
    *core.account.lock().unwrap_or_else(PoisonError::into_inner) = Some(account.clone());
    Ok(json!({ "phase": "authenticated", "account": account }))
}

// ---------------------------------------------------------------------------
// 会话列表 / 消息页
// ---------------------------------------------------------------------------

fn conversations_list(state: &AppState, params: &Value) -> Result<Value, BridgeError> {
    let filter = params
        .get("filter")
        .and_then(Value::as_str)
        .unwrap_or("all");
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE) as usize;
    let offset = cursor_usize(params);

    let me = state.user_id()?;
    let mut rows = Vec::new();
    for (conv, max_seq, max_at) in with_local(state, |s| s.conversations())? {
        rows.push(conversation_summary_parts(
            state, &conv, &me, max_seq, max_at,
        ));
    }
    if filter == "unread" {
        rows.retain(|c| c["unread"].as_i64().unwrap_or(0) > 0);
    }
    if filter == "groups" {
        rows.retain(|c| c["kind"] == json!("group"));
    }
    if !query.is_empty() {
        rows.retain(|c| conversation_matches_query(c, &query));
    }
    rows.sort_by(|a, b| {
        b["pinned"]
            .as_bool()
            .cmp(&a["pinned"].as_bool())
            .then(b["last_at_ms"].as_str().cmp(&a["last_at_ms"].as_str()))
    });
    let total = rows.len();
    let items: Vec<Value> = rows.into_iter().skip(offset).take(limit).collect();
    let has_more = offset + items.len() < total;
    let next = if has_more {
        json!(offset + items.len())
    } else {
        Value::Null
    };
    Ok(json!({ "items": items, "next_cursor": next, "has_more": has_more }))
}

/// 会话卡片的标题或最后一条预览里（已小写）有没有这段查询词。
fn conversation_matches_query(conversation: &Value, query: &str) -> bool {
    ["title", "last_preview"].iter().any(|field| {
        conversation[*field]
            .as_str()
            .unwrap_or_default()
            .to_lowercase()
            .contains(query)
    })
}

/// 单个会话卡片（含最新一条消息的预览）。
fn conversation_summary(state: &AppState, conv_id: &str) -> Result<Value, BridgeError> {
    let me = state.user_id()?;
    let last = with_local(state, |s| {
        Ok(s.page(conv_id, 0, 1)
            .map(|mut rows| rows.pop())
            .unwrap_or(None))
    })?;
    match last {
        Some(row) => {
            let payload = row.to_json();
            let mut c = conversation_summary_parts(state, conv_id, &me, row.seq, row.sent_at_ms);
            c["last_preview"] = json!(preview_of(&payload));
            c["last_at_ms"] = json!(iso_ms(row.sent_at_ms));
            Ok(c)
        }
        None => Ok(conversation_summary_parts(
            state,
            conv_id,
            &me,
            0,
            wxlike_core::types::now_ms() as i64,
        )),
    }
}

/// 会话标题：直聊用 kv 缓存的对端名（缺失时拉一次 profile 并缓存）；
/// 群聊用 group_info 的名字（同样缓存）。
fn conv_title(state: &AppState, conv_id: &str, me: &str, kind: &str) -> String {
    match kind {
        "direct" => {
            let peer = peer_of(conv_id, me).unwrap_or_else(|| conv_id.to_string());
            cached_or_fetch(state, &format!("peer_name:{peer}"), || {
                state
                    .http()
                    .expect_json("GET", &format!("/api/profile/{peer}"), None)
                    .ok()
                    .and_then(|p| p["name"].as_str().map(str::to_string))
                    .unwrap_or_else(|| peer.clone())
            })
        }
        _ => cached_or_fetch(state, &format!("group_name:{conv_id}"), || {
            state
                .group_info(conv_id)
                .ok()
                .and_then(|g| g["name"].as_str().map(str::to_string))
                .unwrap_or_else(|| conv_id.to_string())
        }),
    }
}

/// kv 缓存读取 + 未命中时取值并回写。
fn cached_or_fetch(state: &AppState, cache_key: &str, fetch: impl FnOnce() -> String) -> String {
    let cached = with_local(state, |s| Ok(s.kv_get(cache_key)))
        .ok()
        .flatten()
        .unwrap_or_default();
    if !cached.is_empty() {
        return cached;
    }
    let name = fetch();
    let _ = with_local(state, |s| s.kv_put(cache_key, &name));
    name
}

fn conversation_summary_parts(
    state: &AppState,
    conv_id: &str,
    me: &str,
    max_seq: i64,
    max_at: i64,
) -> Value {
    let kind = conv_kind(conv_id);
    // 标题：直聊用 kv 缓存的对端名（缺失时拉一次 profile 并缓存）；
    // 群聊用 group_info 的名字（同样缓存）。
    let title = conv_title(state, conv_id, me, kind);
    let unread = state
        .http()
        .conv_unread(conv_id)
        .ok()
        .and_then(|v| v["unread"].as_i64())
        .unwrap_or(0);
    let flags = with_local(state, |s| {
        Ok(s.kv_get(&format!("conv_flags:{conv_id}"))
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .unwrap_or(json!({})))
    })
    .unwrap_or(json!({}));
    let draft = with_local(state, |s| {
        Ok(s.kv_get(&format!("draft:{conv_id}")).unwrap_or_default())
    })
    .unwrap_or_default();
    let at = if max_seq > 0 {
        max_at
    } else {
        wxlike_core::types::now_ms() as i64
    };
    json!({
        "conv_id": conv_id,
        "kind": kind,
        "title": title,
        "unread": unread,
        "pinned": flags["pinned"].as_bool().unwrap_or(false),
        "muted": flags["muted"].as_bool().unwrap_or(false),
        "last_preview": "",
        "last_at_ms": iso_ms(at),
        "draft": draft,
    })
}

/// 服务端这一页并入本地库（INSERT OR IGNORE）：本地是唯一渲染源。
fn merge_server_history(state: &AppState, conv_id: &str, before: i64, limit: i64) {
    let Ok(reply) = state.http().expect_json(
        "GET",
        &format!("/api/history/{conv_id}?before_seq={before}&limit={limit}"),
        None,
    ) else {
        return;
    };
    let Some(rows) = reply["messages"].as_array() else {
        return;
    };
    let _ = with_local(state, |s| {
        for row in rows {
            let _ = s.append(row);
        }
        Ok(())
    });
}

fn messages_page(state: &AppState, params: &Value) -> Result<Value, BridgeError> {
    let conv_id = params
        .get("conv_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE) as i64;
    let before = cursor_i64(params);
    merge_server_history(state, conv_id, before, limit);
    let me = state.user_id()?;
    let page = state.local_history(conv_id, before, limit as usize)?;
    let rows = page["messages"].as_array().cloned().unwrap_or_default();
    let items: Vec<Value> = rows.iter().map(|r| row_to_message(r, &me)).collect();
    let has_more = (rows.len() as i64) == limit;
    let next = if has_more {
        rows.first()
            .and_then(|r| r.get("seq"))
            .map(|v| Value::String(v.to_string()))
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    Ok(json!({ "items": items, "next_cursor": next, "has_more": has_more }))
}

fn recipients_of(conv_id: &str, me: &str) -> Vec<String> {
    if conv_id.starts_with("d-") {
        peer_of(conv_id, me).into_iter().collect()
    } else {
        Vec::new() // 群消息由 hub 按成员表扇出
    }
}

fn message_send<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    params: &Value,
) -> Result<Value, BridgeError> {
    let conv_id = params
        .get("conv_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let body = params.get("body").cloned().unwrap_or(Value::Null);
    let client_msg_id = params
        .get("client_msg_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if conv_id.is_empty() || !body.is_object() {
        return Err(BridgeError::Validation("消息参数不合法".into()));
    }
    let me = state.user_id()?;
    send_body_by_kind(state, conv_id, &me, &body)?;
    notify_sent_message(app, state, conv_id, &me, &client_msg_id)
}

/// 按 body.kind 分派到对应的 AppState 发送方法。
fn send_body_by_kind(
    state: &AppState,
    conv_id: &str,
    me: &str,
    body: &Value,
) -> Result<(), BridgeError> {
    let to = recipients_of(conv_id, me);
    let kind = body["kind"].as_str().unwrap_or_default();
    match kind {
        "text" => {
            let text = body["text"].as_str().unwrap_or_default();
            if text.trim().is_empty() {
                return Err(BridgeError::Validation("不能发送空消息".into()));
            }
            state.send_text(conv_id, to, text)?;
        }
        "image" => {
            let media_id = body["media_id"].as_str().unwrap_or_default();
            if media_id.is_empty() {
                return Err(BridgeError::Validation("图片尚未上传".into()));
            }
            state.send_image(conv_id, to, media_id)?;
        }
        "file" => {
            state.send_file(
                conv_id,
                to,
                body["media_id"].as_str().unwrap_or_default(),
                body["name"].as_str().unwrap_or_default(),
                body["size"].as_u64().unwrap_or(0),
                body["sha256"].as_str().unwrap_or(""),
            )?;
        }
        "voice" => {
            state.send_voice(
                conv_id,
                to,
                body["media_id"].as_str().unwrap_or_default(),
                body["duration_ms"].as_u64().unwrap_or(0) as u32,
                Vec::new(),
            )?;
        }
        "sticker" => {
            state.send_sticker(
                conv_id,
                to,
                body["media_id"].as_str().unwrap_or_default(),
                "default",
                body["sticker"].as_str().unwrap_or_default(),
            )?;
        }
        other => {
            return Err(BridgeError::NotImpl(format!("暂不支持发送 {other} 消息")));
        }
    }
    Ok(())
}

/// 发送落库后的本地回显：最新一条推给 UI 立即上屏。
fn notify_sent_message<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    conv_id: &str,
    me: &str,
    client_msg_id: &str,
) -> Result<Value, BridgeError> {
    let last = with_local(state, |s| {
        Ok(s.page(conv_id, 0, 1)
            .map(|mut rows| rows.pop())
            .unwrap_or(None))
    })?;
    let Some(row) = last else {
        return Ok(json!({ "client_msg_id": client_msg_id, "state": "SERVER_ACCEPTED" }));
    };
    let payload = row.to_json();
    let message = row_to_message(&payload, me);
    let msg_id = message["msg_id"].clone();
    emit_core(app, "message.created", conv_id, message.clone());
    emit_core(
        app,
        "conversation.upsert",
        conv_id,
        json!({ "conv_id": conv_id, "last_preview": preview_of(&payload), "last_at_ms": message["created_at_ms"] }),
    );
    Ok(json!({ "msg_id": msg_id, "client_msg_id": client_msg_id, "state": "SERVER_ACCEPTED" }))
}

// ---------------------------------------------------------------------------
// 关系 / 搜索 / 偏好
// ---------------------------------------------------------------------------

