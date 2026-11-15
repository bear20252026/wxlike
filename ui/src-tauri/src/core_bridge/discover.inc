// ---------------------------------------------------------------------------
// 发现域（D6 逐域退役）：服务目录控制面经认证桥。
//
// 服务端是 G07（crates/service-catalog，组合根 discover_mount 装配，与平台同
// sessions 鉴权）。前端旧路径 /api/services、/api/quotes、/api/orders* 由消费侧
// 路由层映射到这里的 discover.* action，桥再打到服务端真实端点：
//   discover.services     → GET  /api/services            （{items} 信封透传）
//   discover.quote        → GET  /api/quotes?service_id=  （Quote 投影透传）
//   discover.order_create → POST /api/orders              （ServiceOrder 透传）
//   discover.order_get    → GET  /api/orders/{id}         （ServiceOrder 透传）
//   discover.orders       → GET  /api/orders              （{items} 信封透传）
//   discover.order_refund → POST /api/orders/{id}/refund  （ServiceOrder 透传）
//
// 识图/翻译（/api/vision*，multipart + 外部 AI）没有核心提供者，在消费侧显式
// 阻塞，不在本桥伪造。凭据口径与钱包/通话/视频/内容/文件域相同：authed_json
// 是唯一认证出口，Bearer 不出 Rust。无供应商凭据时服务端诚实回 501，桥如实
// 映射成 NOT_IMPLEMENTED，不伪造价格/订单。
// ---------------------------------------------------------------------------

fn discover_id(req: &CoreRequest, field: &str) -> Result<String, BridgeError> {
    param_path_id(&req.params, field)
}

/// 服务目录：→ {items:[ServiceEntry]}。空目录就是空数组（管理后台未配置）。
fn act_discover_services<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    authed_json(state, "GET", "/api/services", None, None)
}

/// 报价：service_id 必填 → Quote（price_minor/currency/expires_at）。
fn act_discover_quote<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let service_id = param_str(&req.params, "service_id")?;
    let path = format!("/api/quotes?service_id={service_id}");
    authed_json(state, "GET", &path, None, None)
}

/// 下单：{service_id, quote_id} + 幂等键 → ServiceOrder。
fn act_discover_order_create<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let params = &req.params;
    let body = json!({
        "service_id": param_str(params, "service_id")?,
        "quote_id": param_str(params, "quote_id")?,
    });
    authed_json(state, "POST", "/api/orders", Some(&body), req.idempotency_key.as_deref())
}

/// 查单：order_id → ServiceOrder。
fn act_discover_order_get<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = discover_id(req, "order_id")?;
    authed_json(state, "GET", &format!("/api/orders/{id}"), None, None)
}

/// 订单列举：→ {items:[ServiceOrder]}（只含当前用户自己的）。
fn act_discover_orders<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    authed_json(state, "GET", "/api/orders", None, None)
}

/// 退款：order_id + 幂等键 → ServiceOrder。
fn act_discover_order_refund<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = discover_id(req, "order_id")?;
    authed_json(state, "POST", &format!("/api/orders/{id}/refund"), None, req.idempotency_key.as_deref())
}