fn act_relations_list<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    relations_list(state, params)
}

fn act_relations_requests_incoming<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let reply = state.friend_requests_incoming()?;
    let mut rows = Vec::new();
    for item in reply.as_array().cloned().unwrap_or_default() {
        // 服务端字段是 `from_uid`（crates/platform/src/friends.rs 的
        // `RequestRow`，旧 UI 的 `ui_commands.rs:135` 也读这个）。旧代码读
        // `from_user_id` → 恒缺省，于是每条申请的 `from.user_id`/`from.name`
        // 都是空串，新 UI 的"新好友"页看不出是谁在申请（#296 附带发现）。
        let from_id = item["from_uid"]
            .as_str()
            .or_else(|| item["from_user_id"].as_str())
            .unwrap_or_default()
            .to_string();
        // 显示名与直聊标题同一套：kv 缓存 + profile 兜底，取不到就用 id。
        let from_name = if from_id.is_empty() {
            String::new()
        } else {
            cached_or_fetch(state, &format!("peer_name:{from_id}"), || {
                state
                    .http()
                    .expect_json("GET", &format!("/api/profile/{from_id}"), None)
                    .ok()
                    .and_then(|p| p["name"].as_str().map(str::to_string))
                    .unwrap_or_else(|| from_id.clone())
            })
        };
        rows.push(json!({
            "request_id": item["id"].clone(),
            "from": { "user_id": from_id, "name": from_name },
            "message": item["message"].as_str().unwrap_or_default().to_string(),
            "status": "PENDING",
            "created_at_ms": iso_ms(item["created_at"].as_i64().unwrap_or_default()),
        }));
    }
    Ok(Value::Array(rows))
}

fn act_relations_request_send<R: tauri::Runtime>(
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
    let target = str_param("user_id");
    let message = str_param("message");
    if target.is_empty() {
        return Err(BridgeError::Validation("请选择联系人".into()));
    }
    state.friend_request(&target, &message)?;
    Ok(json!({ "request_id": format!("out-{}", req.request_id), "status": "PENDING" }))
}

fn act_relations_request_accept<R: tauri::Runtime>(
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
    let request_id = str_param("request_id");
    let accept = req.action_id.ends_with("accept");
    if accept {
        state.friend_accept(&request_id)?;
    } else {
        state.friend_decline(&request_id)?;
    }
    Ok(json!({ "status": if accept { "ACCEPTED" } else { "DECLINED" } }))
}

fn act_relations_block<R: tauri::Runtime>(
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
    let user_id = str_param("user_id");
    let blocked = params
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if blocked {
        state
            .http()
            .expect_json("POST", "/api/blocks", Some(&json!({ "user_id": user_id })))?;
    } else {
        state
            .http()
            .expect_empty("DELETE", &format!("/api/blocks/{user_id}"), None)?;
    }
    Ok(json!({ "user_id": user_id, "blocked": blocked }))
}

fn act_relations_note_set<R: tauri::Runtime>(
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
    let user_id = str_param("user_id");
    let note = str_param("note");
    state.http().expect_json(
        "PUT",
        &format!("/api/friends/{user_id}/remark"),
        Some(&json!({ "remark": note })),
    )?;
    Ok(json!({ "user_id": user_id, "note": note }))
}

fn act_relations_tag_set<R: tauri::Runtime>(
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
    let user_id = str_param("user_id");
    let key = format!("contact_meta:{user_id}");
    let mut meta = with_local(state, |s| {
        Ok(s.kv_get(&key)
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .unwrap_or(json!({})))
    })?;
    if req.action_id.ends_with("tag.set") {
        meta["tags"] = params.get("tags").cloned().unwrap_or(json!([]));
    } else {
        meta["star"] = params.get("star").cloned().unwrap_or(json!(false));
    }
    with_local(state, |s| s.kv_put(&key, &meta.to_string()))?;
    // 投影只有一处真相：base_contact 读的就是刚写进去的同一个 kv 键，
    // 且对 tags/star 都做了类型兜底。旧代码在此又用
    // `contact["tags"] = meta["tags"].clone()` 覆写一遍，于是
    // `relations.star.set` 会把没设过标签的联系人的 tags 写成 **null**
    // （契约是 string[]，contacts-page.tsx 直接 contact.tags.length → 崩）。
    // #296 删掉这层覆写：分支只改自己的字段，投影交给 base_contact。
    let contact = base_contact(state, &user_id)?;
    emit_core(app, "contact.updated", &user_id, contact.clone());
    Ok(contact)
}

fn act_relations_profile<R: tauri::Runtime>(
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
    let user_id = str_param("user_id");
    let profile = state
        .http()
        .expect_json("GET", &format!("/api/profile/{user_id}"), None)?;
    let mut value = json!({
        "user_id": profile["user_id"].clone(),
        "name": profile["name"].clone(),
        "avatar": profile["avatar"].clone(),
        "signature": profile["signature"].clone(),
        "is_friend": true,
        "blocked": false,
        "common_groups": [],
    });
    if let Some(remark) = remark_of(state, &user_id) {
        value["note"] = json!(remark);
    }
    Ok(value)
}

fn act_relations_common_groups<R: tauri::Runtime>(
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
    let user_id = str_param("user_id");
    let groups = state.groups_mine()?;
    let mut out = Vec::new();
    for g in groups.as_array().cloned().unwrap_or_default() {
        let gid = g["group_id"].as_str().unwrap_or_default().to_string();
        let in_group = state
            .group_info(&gid)
            .ok()
            .and_then(|info| info["members"].as_array().cloned())
            .map(|members| {
                members
                    .iter()
                    .any(|x| x["user_id"] == json!(user_id) || x["member_id"] == json!(user_id))
            });
        if in_group == Some(true) {
            out.push(json!({ "group_id": gid, "name": g["name"].clone() }));
        }
    }
    Ok(Value::Array(out))
}

fn act_relations_group_members<R: tauri::Runtime>(
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
    let group_id = str_param("group_id");
    let info = state.group_info(&group_id)?;
    let me = state.user_id()?;
    let mut members = Vec::new();
    for m in info["members"].as_array().cloned().unwrap_or_default() {
        members.push(json!({
                    "user_id": m["user_id"].as_str().filter(|s| !s.is_empty()).or_else(|| m["member_id"].as_str()).unwrap_or_default().to_string(),
                    "name": m["name"].as_str().filter(|s| !s.is_empty()).or_else(|| m["member_id"].as_str()).unwrap_or_default().to_string(),
                    "role": m["role"].as_str().map(|r| json!(r)).unwrap_or_else(|| json!("member")),
                    "joined_at_ms": iso_ms(m["joined_at"].as_i64().unwrap_or_default()),
                }));
    }
    let my_role = info["members"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|m| m["user_id"] == json!(me) || m["member_id"] == json!(me))
        })
        .map(|m| {
            m["role"]
                .as_str()
                .map(|r| json!(r))
                .unwrap_or_else(|| json!("member"))
        })
        .unwrap_or_else(|| json!("member"));
    Ok(json!({
        "group_id": group_id,
        "name": info["name"].clone(),
        "my_role": my_role,
        "members": members,
    }))
}

fn act_relations_group_kick<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = core;
    let params = &req.params;
    let str_param = |key: &str| -> String {
        params
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let group_id = str_param("group_id");
    let user_id = str_param("user_id");
    // 空 id 会拼出 `/api/groups//members/` 这种畸形路径，先拦掉。
    if group_id.is_empty() || user_id.is_empty() {
        return Err(BridgeError::Validation(
            "group_id 与 user_id 都不能为空".into(),
        ));
    }
    // 真路由：crates/platform/src/groups.rs 的
    // `.route("/api/groups/{group_id}/members/{user_id}", delete(remove_member))`
    // ——owner-only、拒绝移除群主，与 CLI `group.kick`
    // （crates/client/src/execute/handlers.rs:208）走同一条，不另起一套业务逻辑。
    // 该端点成功时 204：必须 expect_empty（into_json 会把空 body 当解码错误）。
    state.http().expect_empty(
        "DELETE",
        &format!("/api/groups/{group_id}/members/{user_id}"),
        None,
    )?;
    // UI 的 kick() 自己会 group.reload()；这条事件是给"没点踢人但开着群页"的
    // 其它投影用的（store.tsx 认 group.members.updated → 增量刷新）。
    emit_core(
        app,
        "group.members.updated",
        &group_id,
        json!({ "group_id": group_id, "removed": user_id }),
    );
    Ok(json!({ "group_id": group_id, "removed": user_id }))
}

fn act_relations_group_settings_set<R: tauri::Runtime>(
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
    let group_id = str_param("group_id");
    let name = str_param("name");
    if name.is_empty() {
        return Err(BridgeError::Validation("群名称不能为空".into()));
    }
    // 服务端只有公告路由有写入口；改名路由未挂载时如实报错。
    state.http().expect_json(
        "PUT",
        &format!("/api/groups/{group_id}"),
        Some(&json!({ "name": name })),
    )?;
    emit_core(
        app,
        "group.updated",
        &group_id,
        json!({ "group_id": group_id, "name": name }),
    );
    Ok(json!({ "group_id": group_id, "name": name }))
}

fn remark_of(state: &AppState, user_id: &str) -> Option<String> {
    state
        .friends()
        .ok()?
        .as_array()?
        .iter()
        .find(|f| f["friend_id"] == json!(user_id))
        .and_then(|f| f["remark"].as_str().map(str::to_string))
        .filter(|r| !r.is_empty())
}

fn base_contact(state: &AppState, user_id: &str) -> Result<Value, BridgeError> {
    let friends = state.friends()?;
    let row = friends
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|f| f["friend_id"] == json!(user_id));
    let meta = with_local(state, |s| {
        Ok(s.kv_get(&format!("contact_meta:{user_id}"))
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .unwrap_or(json!({})))
    })
    .unwrap_or(json!({}));
    let name = row
        .as_ref()
        .and_then(|r| {
            r["remark"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| user_id.to_string());
    Ok(json!({
        "user_id": user_id,
        "name": name,
        "tags": if meta["tags"].is_array() { meta["tags"].clone() } else { json!([]) },
        "star": meta["star"].as_bool().unwrap_or(false),
        "online": false,
        "blocked": false,
    }))
}

fn relations_list(state: &AppState, params: &Value) -> Result<Value, BridgeError> {
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let star_only = params.get("star").and_then(Value::as_bool).unwrap_or(false);
    let tag_filter = params
        .get("tag")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let friends = state.friends()?;
    let mut items = Vec::new();
    let mut all_tags = Vec::new();
    for f in friends.as_array().cloned().unwrap_or_default() {
        let user_id = f["friend_id"].as_str().unwrap_or_default().to_string();
        if user_id.is_empty() {
            continue;
        }
        let contact = base_contact(state, &user_id)?;
        for t in contact["tags"].as_array().cloned().unwrap_or_default() {
            if let Some(s) = t.as_str() {
                all_tags.push(s.to_string());
            }
        }
        items.push(contact);
    }
    if !query.is_empty() {
        items.retain(|c| {
            c["name"]
                .as_str()
                .unwrap_or_default()
                .to_lowercase()
                .contains(&query)
                || c["user_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query)
        });
    }
    if !tag_filter.is_empty() {
        items.retain(|c| {
            c["tags"]
                .as_array()
                .map(|a| a.iter().any(|t| t.as_str() == Some(tag_filter)))
                .unwrap_or(false)
        });
    }
    if star_only {
        items.retain(|c| c["star"].as_bool().unwrap_or(false));
    }
    all_tags.sort();
    all_tags.dedup();
    Ok(json!({ "items": items, "tags": all_tags }))
}

