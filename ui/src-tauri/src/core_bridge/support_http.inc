// support 碎片 5/6：平台 HTTP 出口（Route 签名、require_session、
// authed_json/call_platform/status_error）。由 support.inc 按序 include。

// ---------------------------------------------------------------------------
// dispatch：全部已登记 action 的总入口
// ---------------------------------------------------------------------------

/// 域路由入口的统一签名（四个参数就是 `act_*` 的那四个）。
type Route<R> =
    fn(&tauri::AppHandle<R>, &AppState, &CoreState, &CoreRequest) -> Result<Value, BridgeError>;

/// 账本一页的默认条数与上限（与服务端 `LEDGER_DEFAULT_LIMIT` / `clamp(1, 200)`
/// 同口径；这里重复一次是为了把超限的请求先削平，别让服务端报 400）。
const LEDGER_DEFAULT_LIMIT: i64 = 50;
const LEDGER_MAX_LIMIT: i64 = 200;
/// 进 URL 路径的 id 允许的最长长度（服务端生成的 id 远小于此，超限即畸形请求）。
const PATH_ID_MAX: usize = 128;
/// 钱包币种：服务端只有虚拟零钱账户，账本金额一律**整数分**。
const WALLET_CURRENCY: &str = "CNY";

/// 会话闸门：**没登录就一个字节都不发**（#293 的核心要求）。
///
/// 拦在发请求之前，是为了让"未登录"稳定报 `UNAUTHENTICATED`，而不是拿一个不带
/// 凭据的请求去换服务端的 401——断网时那条路还会退化成 `OFFLINE`，UI 更加猜不到
/// 是会话没了。也绝不许"没登录就返回空账本"：那是把没有会话伪装成没有数据。
fn require_session(state: &AppState) -> Result<(), BridgeError> {
    if state.auth_token().is_some() {
        return Ok(());
    }
    Err(BridgeError::Unauth(
        "尚未登录：钱包数据只经认证的 Rust 核心会话读取".into(),
    ))
}

/// 桥内**唯一**允许直连平台的出口（`wallet` 域用它；其余域逐步收拢到这里）。
///
/// * 凭据：base URL 与 Bearer 都在 [`AppState`] 的那一份 `Http` 里，由
///   `wxlike-client` 统一挂头。token 的值不经过这个函数，也不会出现在任何
///   action 的返回值里（`tests/bridge_wallet_auth.rs` 逐条验）；
/// * 幂等：`idem` 有值就转成 `Idempotency-Key`（重试只落一笔），没值就**不发**
///   这个头——绝不替调用方编一个键，那会让服务端把两笔不同的操作认成同一笔；
/// * 失败：非 2xx 一律按状态码如实报错（见 [`status_error`]），不折成
///   `{"data": null}` 那种"看着像成功"的空载荷。
fn authed_json(
    state: &AppState,
    method: &str,
    path: &str,
    body: Option<&Value>,
    idem: Option<&str>,
) -> Result<Value, BridgeError> {
    require_session(state)?;
    let read = matches!(method, "GET" | "DELETE");
    if !read && !matches!(method, "POST" | "PUT" | "PATCH") {
        return Err(BridgeError::Other(format!("core 桥不支持的方法：{method}")));
    }
    if read && body.is_some() {
        return Err(BridgeError::Validation("读方法不该带 body".into()));
    }
    let (status, value) = call_platform(state, method, path, body, idem)?;
    if status.is_success() {
        return Ok(value);
    }
    Err(status_error(status, value, path))
}

/// 把请求发出去并把响应体读成 JSON。传输层失败（连不上/超时/DNS）折成
/// `OFFLINE`；HTTP 状态码原样带回去，由 [`status_error`] 分流——那里不再
/// 靠"错误文案里有没有 401 这几个字"猜状态。
fn call_platform(
    state: &AppState,
    method: &str,
    path: &str,
    body: Option<&Value>,
    idem: Option<&str>,
) -> Result<(http::StatusCode, Value), BridgeError> {
    let guard = state.http();
    let got = if matches!(method, "GET" | "DELETE") {
        guard.req(method, path).call()
    } else {
        platform_write(&guard, method, path, body, idem)
    };
    let mut resp =
        got.map_err(|err| BridgeError::Offline(format!("{method} {path}: 传输失败：{err}")))?;
    let status = resp.status();
    Ok((status, reply_json(&mut resp)))
}

/// 写请求（POST/PUT）：带幂等键（有的话），body 缺省时发空体——
/// 服务端的 `…/claim` 就不看 body，但 POST 无体才是合法形状（GET 才有 typestate 区分）。
fn platform_write(
    client: &Http,
    method: &str,
    path: &str,
    body: Option<&Value>,
    idem: Option<&str>,
) -> Result<http::Response<ureq::Body>, ureq::Error> {
    let req = client.req_write(method, path);
    let req = match idem {
        Some(key) => req.header("Idempotency-Key", key),
        None => req,
    };
    match body {
        Some(v) => req.send_json(v),
        None => req.send_empty(),
    }
}

/// 响应体 → JSON：空体给 `{}`（2xx 无体的端点），非 JSON 的体包成
/// `{"error": 原文}`。两者都**不**等于"服务端什么都没说"——字段提取那一步
/// 照样会因为缺字段而报错，不会静默变成一份空数据。
fn reply_json(resp: &mut http::Response<ureq::Body>) -> Value {
    let text = resp.body_mut().read_to_string().unwrap_or_default();
    if text.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(&text).unwrap_or_else(|_| json!({ "error": text }))
}

/// 状态码 → 契约错误码：**这份表是桥里的唯一出处**。
///
/// 401 与 403 必须是错误信封而不是 `{"data":null}`：前者要重新登录，后者重新登录
/// 也没用（`core-contract.ts` 的 `ErrorCode` 本来就有 `FORBIDDEN`，桥此前没用过）。
/// 404 归 `VALIDATION`——契约里没有 `NOT_FOUND` 这一档，服务端对无权读的红包也
/// 一律回 404（不确认存在性），把它说成"内部错误"才是真误导。
fn status_error(status: http::StatusCode, value: Value, path: &str) -> BridgeError {
    let reason = value
        .get("error")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("服务端拒绝了该请求");
    let message = format!("{reason}（{path} → {status}）");
    match status.as_u16() {
        401 => BridgeError::Unauth(message),
        403 => BridgeError::Forbidden(message),
        404 => BridgeError::Validation(message),
        409 => BridgeError::Conflict(message),
        429 => BridgeError::RateLimited(message),
        _ if status.is_server_error() => BridgeError::Dependency(message),
        _ => BridgeError::Other(message),
    }
}

