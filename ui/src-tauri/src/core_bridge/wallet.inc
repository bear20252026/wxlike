fn act_wallet_balance<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let reply = authed_json(state, "GET", "/api/wallet/balance", None, None)?;
    Ok(json!({
        // 单位：分。服务端字段叫 `balance`，UI 模型叫 `balance_minor`（整数分）。
        "balance_minor": need_i64(&reply, "balance")?,
        "currency": WALLET_CURRENCY,
    }))
}

/// 账本一行。字段逐个必需。
///
/// `amount_minor` 是服务端给的**金额大小（整数分，恒为正）**，方向在 `kind` 里
/// （`transfer_out`/`transfer_in`/`redpacket_out`/`redpacket_in`/
/// `redpacket_refund`/`fund_in`，见 `crates/platform/src/wallet.rs` 的 `mod kind`）。
/// 桥**不**按 kind 给它补负号：那是在客户端另立一份服务端账本口径（`refund` 算进
/// 还是算出只有服务端知道），两边各猜一套迟早分叉。`created_at_ms` 按桥的惯例
/// 转成 ISO-8601（见 `shell/models.ts`）。
fn ledger_entry(row: &Value) -> Result<Value, BridgeError> {
    Ok(json!({
        "entry_id": need_i64(row, "id")?,
        "kind": need_str(row, "kind")?,
        "amount_minor": need_i64(row, "amount")?,
        "counterparty": need_str(row, "counterparty")?,
        "ref_id": need_str(row, "ref_id")?,
        "memo": need_str(row, "memo")?,
        "created_at_ms": iso_ms(need_i64(row, "created_at")?),
    }))
}

fn act_wallet_ledger_page<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    // `since`/`limit` 是**请求**侧的默认值（分页游标缺省从头、页大小缺省 50），
    // 与"把缺字段兜成默认值上报"是两回事：这里没有任何数据被编出来。
    let since = cursor_i64(&req.params).max(0);
    let limit = req
        .params
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(LEDGER_DEFAULT_LIMIT)
        .clamp(1, LEDGER_MAX_LIMIT);
    let path = format!("/api/wallet/ledger?since={since}&limit={limit}");
    let reply = authed_json(state, "GET", &path, None, None)?;
    let rows = need_array(&reply, "entries")?;
    let items = rows
        .iter()
        .map(ledger_entry)
        .collect::<Result<Vec<Value>, BridgeError>>()?;
    Ok(json!({
        "items": items,
        "next_cursor": need_opt_i64(&reply, "cursor")?,
        "has_more": need_bool(&reply, "has_more")?,
    }))
}

fn act_wallet_transfer<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let params = &req.params;
    let to_uid = param_path_id(params, "to_uid")?;
    let amount = param_amount(params, "amount_minor")?;
    let memo = opt_str(params, "memo")?;
    let body = json!({ "to_uid": to_uid, "amount": amount, "memo": memo });
    let reply = authed_json(
        state,
        "POST",
        "/api/wallet/transfer",
        Some(&body),
        req.idempotency_key.as_deref(),
    )?;
    Ok(json!({ "transfer_id": need_str(&reply, "transfer_id")? }))
}

fn act_wallet_redpacket_create<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let params = &req.params;
    let amount = param_amount(params, "amount_minor")?;
    let count = param_amount(params, "count")?;
    let memo = opt_str(params, "memo")?;
    // 会话可选：没给（或给了空串）就发 null，服务端按红包自身入账。
    let conv = params
        .get("conv_id")
        .and_then(Value::as_str)
        .filter(|c| !c.trim().is_empty());
    let body = json!({
        "amount": amount,
        "count": count,
        "memo": memo,
        "conv_id": conv,
    });
    let reply = authed_json(
        state,
        "POST",
        "/api/wallet/redpacket",
        Some(&body),
        req.idempotency_key.as_deref(),
    )?;
    Ok(json!({ "redpacket_id": need_str(&reply, "redpacket_id")? }))
}

/// 红包本体 → UI 形状。`remaining` 是**算出来的**（count − claimed），
/// 两个加数都来自服务端且都必需，缺任何一个就报错，不许猜 0。
fn redpacket_record(reply: &Value) -> Result<Value, BridgeError> {
    let count = need_i64(reply, "count")?;
    let claimed = need_i64(reply, "claimed")?;
    Ok(json!({
        "redpacket_id": need_str(reply, "id")?,
        "sender_id": need_str(reply, "from_uid")?,
        "conv_id": need_str(reply, "conv_id")?,
        "total_minor": need_i64(reply, "total")?,
        "count": count,
        "claimed": claimed,
        "remaining": count - claimed,
        "claimed_total_minor": need_i64(reply, "claimed_total")?,
        "memo": need_str(reply, "memo")?,
        "status": need_str(reply, "status")?,
        "created_at_ms": iso_ms(need_i64(reply, "created_at")?),
        "expire_at_ms": iso_ms(need_i64(reply, "expire_at")?),
    }))
}

fn act_wallet_redpacket_get<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = param_path_id(&req.params, "redpacket_id")?;
    let reply = authed_json(
        state,
        "GET",
        &format!("/api/wallet/redpacket/{id}"),
        None,
        None,
    )?;
    redpacket_record(&reply)
}

fn act_wallet_redpacket_claim<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    state: &AppState,
    _core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let id = param_path_id(&req.params, "redpacket_id")?;
    let path = format!("/api/wallet/redpacket/{id}/claim");
    let reply = authed_json(state, "POST", &path, None, req.idempotency_key.as_deref())?;
    Ok(json!({
        "redpacket_id": id,
        "amount_minor": need_i64(&reply, "amount")?,
    }))
}

/// 红包列表：**服务端没有这条路由**，所以这里只有如实拒绝。
///
/// `crates/platform/src/wallet.rs` 挂的是 `POST /api/wallet/redpacket`、
/// `GET /api/wallet/redpacket/{id}`、`POST /api/wallet/redpacket/{id}/claim`、
/// `GET /api/wallet/balance`、`GET /api/wallet/ledger`、`POST /api/wallet/transfer`。
/// 没有列表就没有可靠来源：拿本地消息镜像拼一份"我发过的包"会漏掉别人在群里发的、
/// 还会让"领取"按钮点进一条不存在的路。登记在 [`STUBBED_ACTIONS`]（capabilities
/// 不吹它），等服务端补 `GET /api/wallet/redpacket` 后接 [`authed_json`] 落实。
fn act_wallet_redpacket_list<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    _state: &AppState,
    _core: &CoreState,
    _req: &CoreRequest,
) -> Result<Value, BridgeError> {
    Err(BridgeError::NotImpl(
        "服务端没有红包列表路由（GET /api/wallet/redpacket 未挂载）：见 core_bridge 的 STUBBED_ACTIONS".into(),
    ))
}

