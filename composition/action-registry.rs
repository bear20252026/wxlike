//! G00-03（#101）：组合根契约的核心执行形状。
//!
//! 本文件是**自包含**的（仅 std）：`rustc --edition 2021 --test
//! composition/action-registry.rs` 可独立编译并跑完测试——在 G01-01 把它
//! 挂载进 `crates/types`（REQ-G01-01-mount-registry）之前，独立可验证，
//! 不存在"未验证的源码"状态。挂载后 ActionRequest/EventEnvelope/
//! DomainError/ActorContext 四个类型成为全部表面的公共契约。
//!
//! 设计决定（与 teams/G00.md G00-03 对应）：
//! * 同一 action 不得注册两次——`register` 返回错误而非覆盖；
//! * 未授权命令在**分派前**拒绝（`authorize` 先于 handler）；
//! * 未知字段：参数读取按名取用，未知参数**忽略**（前向兼容窗口）；
//! * 未知枚举：`ActionKind` 之外的字符串落在 `Unknown` 变体，不 panic；
//! * 大整数：`seq`/金额一律 i64/u64 原生承载；跨 JS 表面时由
//!   envelope.schema.json 声明传输约束（web 走 bincode，不走 JSON Number）；
//! * 幂等：写动作必须携带 idempotency key（`ActionRequest.idempotency_key`），
//!   registry 在分派层校验（写动作缺 key = DomainError::MissingIdempotencyKey）。

use std::collections::HashMap;

/// 命令的调用者上下文：身份只来自已验证的会话，绝不来自参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorContext {
    pub user_id: String,
    /// 会话/设备标识（多端同账号时用于审计归因）。
    pub device_id: String,
    /// 已验证的角色（RBAC 输入；授权判定只信这里，不信请求参数）。
    pub roles: Vec<String>,
}

/// 领域参数值：跨表面传输的最小类型集。
/// JSON/JSON-RPC 表面把外部值收敛到这里；bincode 表面直接构造。
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Str(String),
    Int(i64),
    Uint(u64),
    Bool(bool),
    Bytes(Vec<u8>),
}

impl ParamValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ParamValue::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            ParamValue::Int(v) => Some(*v),
            ParamValue::Uint(v) => i64::try_from(*v).ok(),
            _ => None,
        }
    }
}

/// 有序参数袋：按名取用；**未知参数忽略**（前向兼容——旧客户端发来的
/// 多余字段不构成错误，新字段对旧端不可见）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Params {
    entries: Vec<(String, ParamValue)>,
}

impl Params {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn set(&mut self, key: &str, value: ParamValue) {
        self.entries.push((key.to_string(), value));
    }
    pub fn get(&self, key: &str) -> Option<&ParamValue> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn get_str(&self, key: &str) -> Result<&str, DomainError> {
        self.get(key)
            .and_then(ParamValue::as_str)
            .ok_or(DomainError::MissingParam(key.to_string()))
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 一条待执行命令：id + 调用者 + 参数 + 幂等键。
#[derive(Debug, Clone)]
pub struct ActionRequest {
    /// 命令 id（与 action-registry 注册名一致，如 "message.send"）。
    pub action: String,
    pub actor: ActorContext,
    pub params: Params,
    /// 写操作的幂等键（客户端生成，服务端去重）；读操作为 None。
    pub idempotency_key: Option<String>,
}

/// 事件信封：服务端 → 客户端的全部通知形状（与 envelope.schema.json 对齐）。
#[derive(Debug, Clone, PartialEq)]
pub struct EventEnvelope {
    pub version: u8,
    pub conv_id: String,
    pub sender: String,
    pub to: Vec<String>,
    pub seq: u64,
    pub kind: String,
    pub sent_at_ms: u64,
    /// 消息 id：16 字节（传输面 hex/base64，本层保持字节）。
    pub msg_id: [u8; 16],
    /// 未知字段聚合位：前向兼容窗口内，新字段在这里携带，旧端忽略。
    pub unknown_fields: Vec<(String, String)>,
}

/// 领域错误：跨表面统一的失败分类（HTTP/WS/QUIC 映射各自的状态码）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    /// 参数缺失/非法（HTTP 422）。
    MissingParam(String),
    /// 未授权（HTTP 401/403）——分派前拒绝。
    Unauthorized,
    /// 同一 action 重复注册（组合根装配期错误，非运行时）。
    DuplicateAction(String),
    /// 写操作缺幂等键（HTTP 428 语义）。
    MissingIdempotencyKey,
    /// 未知 action（不在 registry，HTTP 404 语义——不泄露存在性）。
    UnknownAction(String),
    /// 未知枚举值：不 panic，携带原值供审计。
    UnknownEnum(String),
}

/// 授权判定函数指针：由装配方注入（platform 的 policy 表），
/// registry 不内嵌权限逻辑。
pub type AuthorizeFn = fn(actor: &ActorContext, action: &str) -> bool;

/// handler 元数据：动作名 + 是否写动作（写 ⇒ 幂等键强制）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionMeta {
    pub write: bool,
}

/// 动作注册表：**同一 action 只能注册一次**；分派前先授权。
pub struct ActionRegistry {
    actions: HashMap<String, ActionMeta>,
    authorize: AuthorizeFn,
}

impl ActionRegistry {
    pub fn new(authorize: AuthorizeFn) -> Self {
        Self { actions: HashMap::new(), authorize }
    }

    /// 注册一个 action。重复注册返回 [`DomainError::DuplicateAction`]，
    /// **不覆盖**——组合根装配期的重复是 bug，静默覆盖会掩盖它。
    pub fn register(&mut self, action: &str, meta: ActionMeta) -> Result<(), DomainError> {
        if self.actions.contains_key(action) {
            return Err(DomainError::DuplicateAction(action.to_string()));
        }
        self.actions.insert(action.to_string(), meta);
        Ok(())
    }

    pub fn is_registered(&self, action: &str) -> bool {
        self.actions.contains_key(action)
    }

    /// 分派前闸门：未知 action → UnknownAction；未授权 → Unauthorized；
    /// 已注册的写动作缺幂等键 → MissingIdempotencyKey。
    /// 返回 Ok 才允许调用真实 handler。
    pub fn admit(&self, req: &ActionRequest) -> Result<ActionMeta, DomainError> {
        let meta = self
            .actions
            .get(&req.action)
            .copied()
            .ok_or_else(|| DomainError::UnknownAction(req.action.clone()))?;
        if !(self.authorize)(&req.actor, &req.action) {
            return Err(DomainError::Unauthorized);
        }
        if meta.write && req.idempotency_key.is_none() {
            return Err(DomainError::MissingIdempotencyKey);
        }
        Ok(meta)
    }
}

/// 未知枚举的确定性收敛：字符串不在已知集 ⇒ Unknown（不 panic）。
pub fn classify_enum<'a>(known: &[&'a str], value: &str) -> Result<&'a str, DomainError> {
    known
        .iter()
        .copied()
        .find(|k| *k == value)
        .ok_or_else(|| DomainError::UnknownEnum(value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> ActorContext {
        ActorContext {
            user_id: "u1".into(),
            device_id: "d1".into(),
            roles: vec!["member".into()],
        }
    }

    fn allow_all(_: &ActorContext, _: &str) -> bool {
        true
    }
    fn deny_all(_: &ActorContext, _: &str) -> bool {
        false
    }

    fn write_req(key: Option<&str>) -> ActionRequest {
        let mut params = Params::new();
        params.set("conv", ParamValue::Str("d-a~b".into()));
        ActionRequest {
            action: "message.send".into(),
            actor: actor(),
            params,
            idempotency_key: key.map(str::to_string),
        }
    }

    #[test]
    fn duplicate_action_registration_is_an_error_not_a_silent_overwrite() {
        let mut reg = ActionRegistry::new(allow_all);
        reg.register("message.send", ActionMeta { write: true }).unwrap();
        let second = reg.register("message.send", ActionMeta { write: true });
        assert_eq!(second, Err(DomainError::DuplicateAction("message.send".into())));
    }

    #[test]
    fn write_action_without_idempotency_key_is_rejected_before_dispatch() {
        let mut reg = ActionRegistry::new(allow_all);
        reg.register("message.send", ActionMeta { write: true }).unwrap();
        assert!(matches!(
            reg.admit(&write_req(None)),
            Err(DomainError::MissingIdempotencyKey)
        ));
        assert!(reg.admit(&write_req(Some("idem-1"))).is_ok());
    }

    #[test]
    fn unauthorized_is_rejected_before_any_handler_runs() {
        let mut reg = ActionRegistry::new(deny_all);
        reg.register("message.send", ActionMeta { write: true }).unwrap();
        assert!(matches!(reg.admit(&write_req(Some("k"))), Err(DomainError::Unauthorized)));
    }

    #[test]
    fn unknown_action_and_unknown_enum_are_deterministic() {
        let mut reg = ActionRegistry::new(allow_all);
        let mut req = write_req(Some("k"));
        req.action = "message.teleport".into();
        assert!(matches!(
            reg.admit(&req),
            Err(DomainError::UnknownAction(_))
        ));
        assert!(matches!(
            classify_enum(&["text", "blob"], "hologram"),
            Err(DomainError::UnknownEnum(_))
        ));
        assert_eq!(classify_enum(&["text", "blob"], "text"), Ok("text"));
    }

    #[test]
    fn unknown_params_are_ignored_and_known_params_round_trip() {
        let mut params = Params::new();
        params.set("conv", ParamValue::Str("d-a~b".into()));
        params.set("future_field_v9", ParamValue::Str("whatever".into()));
        assert_eq!(params.get_str("conv").unwrap(), "d-a~b");
        assert!(params.get("future_field_v9").is_some(), "未知字段保留但不强制");
        assert!(params.get("missing").is_none());
        // 大整数：Rust 侧 i64/u64 原生无损；JS 安全线（2^53）是传输层约束
        // （web 走 bincode，见 envelope.schema.json），不在本层伪装
        let big = ParamValue::Uint(9_007_199_254_740_993); // 2^53+1
        assert_eq!(big.as_i64(), Some(9_007_199_254_740_993), "i64 范围内无损");
        let beyond = ParamValue::Uint(u64::MAX);
        assert_eq!(beyond.as_i64(), None, "超出 i64 的值不得有损转换");
    }

    #[test]
    fn event_envelope_carries_unknown_fields_forward() {
        let env = EventEnvelope {
            version: 1,
            conv_id: "d-a~b".into(),
            sender: "a".into(),
            to: vec!["b".into()],
            seq: 42,
            kind: "text".into(),
            sent_at_ms: 1_700_000_000_000,
            msg_id: [7; 16],
            unknown_fields: vec![("v9_priority".into(), "high".into())],
        };
        assert_eq!(env.unknown_fields.len(), 1, "未知字段进兼容窗口，旧端忽略");
    }
}
