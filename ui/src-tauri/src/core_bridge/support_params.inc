// support 碎片 6/6：参数与响应字段校验（required_str、malformed/missing_param、
// field_*/need_*/param_*）。由 support.inc 按序 include。

/// 服务端响应缺字段 = 契约破了，如实报错。
///
/// 这里刻意不用 `value["x"].as_i64().unwrap_or_default()`：那会把"没给余额"
/// 上报成 `balance_minor: 0`，UI 显示的就是一份**看起来真实**的 0 元余额。
fn malformed(field: &str) -> BridgeError {
    BridgeError::Other(format!(
        "服务端响应缺少字段 {field}（拒绝把它兜成 0/空串再上报）"
    ))
}

/// 调用方少给了必填参数 = 校验错（不是 INTERNAL：服务端没做错任何事）。
fn missing_param(field: &str) -> BridgeError {
    BridgeError::Validation(format!("缺少参数 {field}"))
}

fn field_i64(value: &Value, field: &str) -> Option<i64> {
    value.get(field).and_then(Value::as_i64)
}

fn field_str(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_string)
}

// —— 以下 `need_*` 读的是**服务端响应**：缺字段一律 [`malformed`]。——
fn need_i64(value: &Value, field: &str) -> Result<i64, BridgeError> {
    field_i64(value, field).ok_or_else(|| malformed(field))
}

fn need_str(value: &Value, field: &str) -> Result<String, BridgeError> {
    field_str(value, field).ok_or_else(|| malformed(field))
}

fn need_bool(value: &Value, field: &str) -> Result<bool, BridgeError> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| malformed(field))
}

fn need_array(value: &Value, field: &str) -> Result<Vec<Value>, BridgeError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| malformed(field))
}

/// `cursor` 允许是 `null`（服务端还没有账本头），但字段必须存在。
fn need_opt_i64(value: &Value, field: &str) -> Result<Option<i64>, BridgeError> {
    let raw = value.get(field).ok_or_else(|| malformed(field))?;
    if raw.is_null() {
        return Ok(None);
    }
    raw.as_i64().map(Some).ok_or_else(|| malformed(field))
}

// —— 以下 `param_*` 读的是**调用方请求**：缺/错一律 VALIDATION，且都在发请求之前。——
fn param_i64(value: &Value, field: &str) -> Result<i64, BridgeError> {
    field_i64(value, field).ok_or_else(|| missing_param(field))
}

fn param_str(value: &Value, field: &str) -> Result<String, BridgeError> {
    let raw = field_str(value, field).ok_or_else(|| missing_param(field))?;
    if raw.trim().is_empty() {
        return Err(missing_param(field));
    }
    Ok(raw)
}

/// 必填的正整数金额/个数（单位：分、条）。缺、非整数、≤0 一律先拦掉再发请求。
fn param_amount(value: &Value, field: &str) -> Result<i64, BridgeError> {
    let amount = param_i64(value, field)?;
    if amount > 0 {
        return Ok(amount);
    }
    Err(BridgeError::Validation(format!(
        "{field} 必须是正整数（单位：分/条）"
    )))
}

/// 可选字符串参数：没给或 `null` → 空串；给了但不是字符串 → 校验错
/// （不许 `"memo": 123` 悄悄变成一条空备注的转账）。
fn opt_str(value: &Value, field: &str) -> Result<String, BridgeError> {
    match value.get(field).filter(|v| !v.is_null()) {
        Some(v) => v
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| BridgeError::Validation(format!("{field} 必须是字符串"))),
        None => Ok(String::new()),
    }
}

/// 进 URL 路径的 id：只允许安全字符。空串、`/`、`?`、`#`、空格、`..` 都会拼出
/// 畸形路径（`/api/wallet/redpacket//claim`）甚至改写请求目标，一律先拒。
/// 与 `relations.group.kick` 的空 id 拦截同口径，这里挡得更全（钱包 id 直接进路径）。
fn param_path_id(value: &Value, field: &str) -> Result<String, BridgeError> {
    let id = param_str(value, field)?;
    let safe = id.len() <= PATH_ID_MAX
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'));
    if safe {
        return Ok(id);
    }
    Err(BridgeError::Validation(format!(
        "{field} 不是合法的 id（长度 ≤{PATH_ID_MAX}，只允许字母数字与 -_.~）"
    )))
}


