//! contract-v1 适配层：把 catalog 的命令翻译成 `wxlike-types` 的公共契约。
//!
//! G00-03 冻结的 `ActionRequest` / `ActionResult` / `DomainError` 是三端共用的
//! 唯一形状（`crates/types`）。这一层只做四件事，不重复实现业务：
//!
//! 1. **请求成形**：`action_id` 走 `ActionId::parse`（字符集由契约定义），
//!    写命令按 catalog 的 `Idempotency` 强制幂等键，`deadline` 转绝对时间；
//! 2. **本地拒绝**：过期 / 缺幂等键 / 未知 action 在执行前就返回领域错误，
//!    错误码与 UI 看到的一致；
//! 3. **结果成形**：三态 `ok / empty / failed`——空结果不是错误，
//!    这是 UI 上"没有更多消息"与"出错了"的分界；
//! 4. **错误分类**：执行器的字符串错误收敛到 `DomainError`，
//!    CLI 退出码与 MCP `isError` 都从这里派生，不再各判一套。

use wxlike_types::{
    ActionId, ActionOutcome, ActionRequest, ActionResult, Deadline, DomainError, EntryId,
    IdempotencyKey, TraceId,
};

use crate::catalog::CommandSpec;

/// 一次调用的链路信息（CLI 参数 / MCP `_meta` / 默认生成）。
#[derive(Clone, Debug)]
pub struct Envelope {
    pub trace_id: TraceId,
    pub entry_id: Option<EntryId>,
    pub deadline: Option<Deadline>,
}

impl Default for Envelope {
    fn default() -> Self {
        Self {
            trace_id: new_trace_id(),
            entry_id: None,
            deadline: None,
        }
    }
}

impl Envelope {
    /// 从可选字符串构造：空串等于没给，非法值直接报领域错误而不是静默丢弃。
    pub fn from_raw(
        trace_id: Option<&str>,
        entry_id: Option<&str>,
        deadline_ms: Option<i64>,
    ) -> Result<Self, DomainError> {
        let trace_id = match trace_id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => TraceId::parse(raw).map_err(|e| {
                DomainError::invalid_field("trace_id", format!("trace_id 不合法：{e}"))
            })?,
            None => new_trace_id(),
        };
        let entry_id = match entry_id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => Some(EntryId::parse(raw).map_err(|e| {
                DomainError::invalid_field("entry_id", format!("entry_id 不合法：{e}"))
            })?),
            None => None,
        };
        Ok(Self {
            trace_id,
            entry_id,
            deadline: deadline_ms.map(Deadline::at),
        })
    }
}

/// 生成一个可读的 trace id（`tr-` + 16 位十六进制）。
///
/// 不用 UUID：日志里要能一眼看出"这是我们自己发的"，且不引入新依赖。
pub fn new_trace_id() -> TraceId {
    let raw: [u8; 8] = rand::random();
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    // 字符集与长度都在契约允许范围内（字母数字 + `-`）
    TraceId::parse(format!("tr-{hex}")).unwrap_or_else(|_| {
        // 理论上不可达；真的不可达时也不能 panic——退回一个常量级合法值
        TraceId::parse("tr-0000000000000000").unwrap_or_else(|_| unreachable!("常量 trace id"))
    })
}

/// catalog id → `ActionId`。id 是编译期常量，解析失败说明有人写错了 catalog。
pub fn action_id(spec: &CommandSpec) -> ActionId {
    ActionId::parse(spec.id).unwrap_or_else(|e| panic!("catalog id {:?} 不合契约：{e}", spec.id))
}

/// 成形一条请求。**写命令缺幂等键在这里就被拒**，不发请求。
pub fn build_request(
    spec: &CommandSpec,
    params: &serde_json::Value,
    env: &Envelope,
) -> Result<ActionRequest, DomainError> {
    let mut req = ActionRequest::new(action_id(spec), params.clone(), env.trace_id.clone());
    req.entry_id = env.entry_id.clone();
    req.deadline = env.deadline;
    if spec.idempotency.is_required() {
        let raw = params
            .get("idempotency_key")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let key = IdempotencyKey::parse(raw).map_err(|e| {
            DomainError::invalid_field("idempotency_key", format!("幂等键不合法：{e}"))
        })?;
        req.idempotency_key = Some(key);
    }
    // catalog 与契约都认为写命令要幂等键：两边都过一遍，避免只改一处
    req.check_idempotency(spec.idempotency.is_required())?;
    Ok(req)
}

/// 截止时间检查：本地预算用完就别再发请求，错误码是 `timeout`。
pub fn check_deadline(env: &Envelope, now_ms: i64) -> Result<(), DomainError> {
    match env.deadline {
        Some(d) if d.expired(now_ms) => Err(DomainError::timeout(format!(
            "deadline 已过（{} ms），本地直接拒绝，未发请求",
            d.unix_ms()
        ))),
        _ => Ok(()),
    }
}

/// 成功结果。空数组 / 空对象 / `null` 一律判为 `empty`：
/// 把"没有更多了"塞进 `Ok(null)`，前端只能靠猜。
pub fn ok_result(spec: &CommandSpec, data: serde_json::Value, now_ms: u64) -> ActionResult {
    let id = action_id(spec);
    if is_empty_payload(&data) {
        return ActionResult::empty(id, now_ms);
    }
    ActionResult::ok(id, data, now_ms)
}

fn is_empty_payload(data: &serde_json::Value) -> bool {
    match data {
        serde_json::Value::Null => true,
        serde_json::Value::Array(items) => items.iter().all(is_empty_payload),
        // `{"count":0,"messages":[]}` 这类"包了一层"的空结果也算空：
        // 只要每个值都是 0 / 空 / null，就没有真实数据可展示。
        serde_json::Value::Object(map) => map.values().all(is_empty_payload),
        serde_json::Value::Number(n) => n.as_f64() == Some(0.0),
        _ => false,
    }
}

/// 失败结果。错误是**契约里的分类**，不是一句话。
pub fn failed_result(spec: &CommandSpec, error: DomainError, now_ms: u64) -> ActionResult {
    ActionResult::failed(action_id(spec), error, now_ms)
}

/// 执行器的字符串错误 → 领域错误。
///
/// 映射顺序与 `policy::Deny` / `auth::AuthError` 的文案一一对应；
/// 新加一种错误文案时，这里与 CLI 退出码表要一起改（有测试钉住常见几种）。
pub fn classify(message: &str) -> DomainError {
    let m = message.to_ascii_lowercase();
    if message.contains("unknown command") || message.contains("尚未实现") {
        return DomainError::not_found(message.to_string());
    }
    if message.contains("not allowed")
        || message.contains("write access was not granted")
        || message.contains("destructive access was not granted")
        || message.contains("read-only")
        || message.contains("missing scope")
        || message.contains("is an admin action")
    {
        return DomainError::forbidden(message.to_string());
    }
    if message.contains("needs a `conv` param")
        || message.contains("缺少")
        || message.contains("untrusted content")
        || message.contains("confirmation")
        || message.contains("参数")
    {
        return DomainError::invalid(message.to_string());
    }
    if message.contains("token expired") || message.contains("token audience") {
        return DomainError::unauthorized(message.to_string());
    }
    if m.contains("connect")
        || m.contains("timed out")
        || m.contains("timeout")
        || m.contains("dns")
    {
        return DomainError::unavailable(message.to_string());
    }
    DomainError::internal(message.to_string())
}

/// 结果 JSON（CLI 的 `--json` 与 MCP 的 `structuredContent` 用同一份）。
pub fn result_json(result: &ActionResult) -> serde_json::Value {
    serde_json::to_value(result).unwrap_or_else(|e| {
        serde_json::json!({
            "status": "failed",
            "error": {"code": "internal", "message": format!("结果序列化失败：{e}")},
            "completed_at_ms": 0,
        })
    })
}

/// 结果的三态名（退出码与日志都用它）。
pub fn status_name(result: &ActionResult) -> &'static str {
    match result.outcome {
        ActionOutcome::Ok { .. } => "ok",
        ActionOutcome::Empty => "empty",
        ActionOutcome::Failed { .. } => "failed",
    }
}

/// 当前 unix 毫秒（`completed_at_ms` 的来源，契约里是 u64）。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// 同一时刻的有符号版本（`Deadline` 用 i64：允许"1970 之前"这种历史值）。
pub fn now_ms_i64() -> i64 {
    i64::try_from(now_ms()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;
    use serde_json::json;

    fn spec(id: &str) -> &'static CommandSpec {
        catalog::get(id).expect("spec")
    }

    #[test]
    fn every_catalog_id_is_a_valid_contract_action_id() {
        for s in catalog::CATALOG {
            let id = action_id(s);
            assert_eq!(id.as_str(), s.id);
        }
    }

    #[test]
    fn write_commands_without_a_key_are_rejected_before_any_request() {
        let env = Envelope::default();
        let err = build_request(
            spec("msg.send"),
            &json!({"conv": "g-1", "text": "hi"}),
            &env,
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
        assert!(err.message().contains("幂等"));

        let req = build_request(
            spec("msg.send"),
            &json!({"conv": "g-1", "text": "hi", "idempotency_key": "k-1"}),
            &env,
        )
        .expect("request");
        assert_eq!(
            req.idempotency_key.as_ref().map(IdempotencyKey::as_str),
            Some("k-1")
        );
        assert_eq!(req.action_id.as_str(), "msg.send");
    }

    #[test]
    fn read_commands_need_no_key_and_carry_trace() {
        let env = Envelope::from_raw(Some("tr-abc"), Some("ENT-SEED-0025"), None).expect("env");
        let req = build_request(spec("conv.list"), &json!({}), &env).expect("request");
        assert!(req.idempotency_key.is_none());
        assert_eq!(req.trace_id.as_str(), "tr-abc");
        assert_eq!(
            req.entry_id.as_ref().map(EntryId::as_str),
            Some("ENT-SEED-0025")
        );
        assert_eq!(req.contract, wxlike_types::CONTRACT_VERSION);
    }

    /// 过期 deadline 在本地就拒，错误码是 timeout —— 别把注定超时的请求发出去。
    #[test]
    fn expired_deadline_is_rejected_locally() {
        let env = Envelope::from_raw(None, None, Some(1_000)).expect("env");
        let err = check_deadline(&env, 2_000).unwrap_err();
        assert_eq!(err.code(), "timeout");
        assert!(err.message().contains("未发请求"));
        assert!(check_deadline(&env, 999).is_ok());
        // 没给 deadline 就没有预算问题
        assert!(check_deadline(&Envelope::default(), i64::MAX).is_ok());
    }

    #[test]
    fn illegal_trace_and_entry_ids_are_reported_not_ignored() {
        let err = Envelope::from_raw(Some("bad id with space"), None, None).unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
        let err = Envelope::from_raw(None, Some("入口 id"), None).unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
        // 空串等于没给：自动生成 trace，不报错
        let env = Envelope::from_raw(Some("  "), Some(""), None).expect("env");
        assert!(env.trace_id.as_str().starts_with("tr-"));
        assert!(env.entry_id.is_none());
    }

    #[test]
    fn empty_payloads_become_the_empty_state_not_an_error() {
        for empty in [
            json!(null),
            json!([]),
            json!({}),
            json!({"messages": []}),
            json!({"count": 0, "messages": []}),
        ] {
            let result = ok_result(spec("conv.history"), empty.clone(), 1);
            assert_eq!(status_name(&result), "empty", "{empty} 应当是 empty");
            assert!(result.is_ok(), "空结果不是错误");
        }
        let result = ok_result(spec("conv.history"), json!({"messages": [{"seq": 1}]}), 1);
        assert_eq!(status_name(&result), "ok");
    }

    #[test]
    fn results_carry_action_id_and_completion_time() {
        let result = ok_result(spec("msg.get"), json!({"msg_id": "m"}), 1_700_000_000_000);
        let payload = result_json(&result);
        assert_eq!(payload["action_id"], "msg.get");
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["data"]["msg_id"], "m");
        assert_eq!(payload["completed_at_ms"], 1_700_000_000_000_u64);

        let failed = failed_result(spec("msg.get"), DomainError::not_found("nope"), 5);
        let payload = result_json(&failed);
        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["error"]["code"], "not_found");
    }

    /// 错误分类与 CLI 退出码、MCP `isError` 同源：这里错一个码，三处一起错。
    #[test]
    fn error_classification_matches_the_contract_codes() {
        let cases = [
            ("unknown command \"msg.nope\"", "not_found"),
            (
                "conversation g-2 is not allowed: outside the allowlist",
                "forbidden",
            ),
            ("missing scope conv:g-2", "forbidden"),
            (
                "msg.send needs a `conv` param, but none was given",
                "invalid_argument",
            ),
            ("token expired at 1 (now 2)", "unauthorized"),
            ("connection refused: connect error", "unavailable"),
            ("request timed out", "unavailable"),
            ("server: 500 internal error", "internal"),
        ];
        for (message, want) in cases {
            assert_eq!(classify(message).code(), want, "{message}");
        }
    }

    #[test]
    fn generated_trace_ids_are_contract_legal_and_unique() {
        let a = new_trace_id();
        let b = new_trace_id();
        assert_ne!(a.as_str(), b.as_str());
        assert!(a.as_str().starts_with("tr-"));
        assert!(TraceId::parse(a.as_str()).is_ok());
    }
}
