// ---------------------------------------------------------------------------
// 文件域（D6 逐域退役）：文件中心控制面经认证桥。
//
// 服务端是 G04（#295 已挂载的 `crates/features/media`，`/api/v1/*`，与平台同
// 服务器同 sessions 鉴权）。前端旧路径 `/api/files*` 由消费侧路由层映射到这里的
// file.* action，桥再打到 G04 的真实端点：
//   file.list   → GET    /api/v1/files            （FilePage 信封原样透传）
//   file.rename → PATCH  /api/v1/files/{id}       （FileItem 原样透传）
//   file.share  → POST   /api/v1/files/{id}/shares（ShareGrant 原样透传）
//   file.remove → DELETE /api/v1/files/{id}       （204 → {}）
//
// 二进制上传/下载（multipart / Range 流）没有 JSON 流式核心提供者，按视频域
// 上传同口径在消费侧显式阻塞，不在本桥伪造；备份/清理计划服务端形状（导出包
// 计划）与前端清理契约未对齐，同样显式阻塞。凭据口径与钱包/通话/视频/内容域
// 相同：authed_json 是唯一认证出口，Bearer 不出 Rust。
// ---------------------------------------------------------------------------

fn file_id(req: &CoreRequest, field: &str) -> Result<String, BridgeError> {
    param_path_id(&req.params, field)
}

/// 文件列表：category/folder/sort/cursor/limit 可选 → FilePage
/// {items, next_cursor, has_more, snapshot_version}。投影回 api.ts 的裸数组
/// 由消费侧路由层负责（同钱包/内容口径）。
fn act_file_list<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let params = &req.params;
    let mut path = String::from("/api/v1/files");
    let mut sep = '?';
    // 字符串型查询参数（category/folder/sort/cursor）：服务端按 snake_case 枚举
    // 解析，桥不替调用方猜默认值，缺省就不带。
    for key in ["category", "folder", "sort", "cursor"] {
        if let Some(value) = params.get(key).and_then(Value::as_str) {
            if value.is_empty() {
                continue;
            }
            path.push_str(&format!("{sep}{key}={value}"));
            sep = '&';
        }
    }
    if let Some(limit) = params.get("limit").and_then(Value::as_i64) {
        path.push_str(&format!("{sep}limit={limit}"));
    }
    authed_json(state, "GET", &path, None, None)
}

/// 重命名：{name} → FileItem。服务端 PATCH 同时支持 name/folder，前端文件中心
/// 只用改名；folder 移动留待后续契约面，这里不替调用方编一个 folder。
fn act_file_rename<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = file_id(req, "file_id")?;
    let name = param_str(&req.params, "name")?;
    let body = json!({ "name": name });
    authed_json(state, "PATCH", &format!("/api/v1/files/{id}"), Some(&body), req.idempotency_key.as_deref())
}

/// 分享：{ttl_ms?, audience?} → ShareGrant {share_id, token, url, expires_at}。
/// ttl/audience 缺省由服务端按配置兜底（桥不编默认 TTL）。
fn act_file_share<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = file_id(req, "file_id")?;
    let params = &req.params;
    let mut body = serde_json::Map::new();
    if let Some(ttl) = params.get("ttl_ms").and_then(Value::as_i64) {
        body.insert("ttl_ms".into(), json!(ttl));
    }
    if let Some(audience) = params.get("audience").filter(|v| v.is_object()) {
        body.insert("audience".into(), audience.clone());
    }
    let body = Value::Object(body);
    authed_json(state, "POST", &format!("/api/v1/files/{id}/shares"), Some(&body), req.idempotency_key.as_deref())
}

/// 删除（移入回收站）：DELETE → 204 → {}。服务端只摘文件引用，消息引用保留，
/// 物理 GC 由引用计数另行决定（桥不替服务端做删除语义）。
fn act_file_remove<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = file_id(req, "file_id")?;
    authed_json(state, "DELETE", &format!("/api/v1/files/{id}"), None, req.idempotency_key.as_deref())
}