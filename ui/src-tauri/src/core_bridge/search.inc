fn act_search_query<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    let params = &req.params;
    search_query(state, params)
}

fn message_hits(state: &AppState, query: &str) -> Result<Vec<Value>, BridgeError> {
    let reply = state.search_messages(query, 20)?;
    let rows = reply["results"].as_array().cloned().unwrap_or_default();
    let mut hits = Vec::new();
    for row in rows {
        let text = row.get("text").and_then(Value::as_str).unwrap_or_default();
        hits.push(json!({
            "id": row.get("msg_id").cloned().unwrap_or_else(|| json!("")),
            "kind": "content",
            "title": text.chars().take(SEARCH_TITLE_CHARS).collect::<String>(),
            "subtitle": row.get("conv_id").and_then(Value::as_str).unwrap_or_default(),
            "conv_id": row.get("conv_id").cloned().unwrap_or(Value::Null),
            "snippet": text.chars().skip(SEARCH_TITLE_CHARS).take(SEARCH_SNIPPET_CHARS).collect::<String>(),
        }));
    }
    Ok(hits)
}

fn contact_hits(state: &AppState, query: &str) -> Result<Vec<Value>, BridgeError> {
    let items = relations_list(state, &json!({ "query": query }))?["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let hits = items
        .iter()
        .map(|contact| {
            json!({
                "id": contact["user_id"].clone(),
                "kind": "contact",
                "title": contact["name"].clone(),
                "subtitle": contact["user_id"].clone(),
                "user_id": contact["user_id"].clone(),
            })
        })
        .collect();
    Ok(hits)
}

fn group_hits(state: &AppState, query: &str) -> Result<Vec<Value>, BridgeError> {
    let groups = state.groups_mine()?;
    let needle = query.to_lowercase();
    let mut hits = Vec::new();
    for g in groups.as_array().cloned().unwrap_or_default() {
        let name = g["name"].as_str().unwrap_or_default();
        if !name.to_lowercase().contains(&needle) {
            continue;
        }
        hits.push(json!({
            "id": g["group_id"].clone(),
            "kind": "group",
            "title": name,
            "subtitle": format!("{} 人", g["member_count"].as_i64().unwrap_or(0)),
            "group_id": g["group_id"].clone(),
        }));
    }
    Ok(hits)
}

fn search_query(state: &AppState, params: &Value) -> Result<Value, BridgeError> {
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let category = params
        .get("category")
        .and_then(Value::as_str)
        .unwrap_or("all");
    if query.trim().is_empty() {
        return Ok(json!({ "category": category, "hits": [] }));
    }
    let mut hits = Vec::new();
    // 消息全文（服务端 /api/search/messages）——全部/内容/会话分类。
    if matches!(category, "all" | "content" | "chats") {
        hits.extend(message_hits(state, query)?);
    }
    // 联系人与群（本地已有的关系面）。
    if matches!(category, "all" | "contacts") {
        hits.extend(contact_hits(state, query)?);
    }
    if matches!(category, "all" | "groups") {
        hits.extend(group_hits(state, query)?);
    }
    Ok(json!({ "category": category, "hits": hits }))
}

