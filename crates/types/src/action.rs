//! 命令请求与结果：**唯一的**客户端 → 核心入口。
//!
//! 桌面、Web、手机三条路径最终都调同一个 `dispatch(ActionRequest)`，
//! 而不是各写一套 URL 拼接（历史上就有第四套实现：移动端绕过 Policy 自己
//! 拼 `/api/...`，服务端审计里看不到那条命令）。
//!
//! 三个字段是硬要求：
//!
//! * `idempotency_key`：**写命令必填**。没有它，重试一次就是两条消息、
//!   两个群。校验放在 [`ActionRequest::check_idempotency`]，由知道
//!   "这条命令是不是写"的那一层（catalog）调用。
//! * `deadline`：绝对时间，贯穿传输/重试/核心，不在每层各设一个相对超时。
//! * `trace_id`：客户端生成，服务端原样带回，日志对得上。

use serde::{Deserialize, Serialize};

use crate::contract::{ContractVersion, CONTRACT_VERSION};
use crate::cursor::Cursor;
use crate::deadline::Deadline;
use crate::error::DomainError;
use crate::ids::{ActionId, EntryId, IdempotencyKey, TraceId};

/// 一条命令请求。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionRequest {
    /// 命令 id，与 `crates/cmd` 的 catalog 一一对应。
    pub action_id: ActionId,
    /// 触发它的 UI 入口 id（G00-01 的入口总账）。后台任务 / CLI 为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<EntryId>,
    /// 命令参数。形状由 catalog 声明，这里不做校验（各域自己校验）。
    pub params: serde_json::Value,
    /// 幂等键：写命令必填。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<IdempotencyKey>,
    /// 绝对截止时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<Deadline>,
    /// 链路追踪 id。
    pub trace_id: TraceId,
    /// 调用方实现的契约版本。
    pub contract: ContractVersion,
}

impl ActionRequest {
    /// 构造一条请求（读命令用这个就够了）。
    #[must_use]
    pub fn new(action_id: ActionId, params: serde_json::Value, trace_id: TraceId) -> Self {
        Self {
            action_id,
            entry_id: None,
            params,
            idempotency_key: None,
            deadline: None,
            trace_id,
            contract: CONTRACT_VERSION,
        }
    }

    /// 挂上入口 id（Builder 风格，链式）。
    #[must_use]
    pub fn with_entry(mut self, entry_id: EntryId) -> Self {
        self.entry_id = Some(entry_id);
        self
    }

    /// 挂上幂等键。
    #[must_use]
    pub fn with_idempotency(mut self, key: IdempotencyKey) -> Self {
        self.idempotency_key = Some(key);
        self
    }

    /// 挂上截止时间。
    #[must_use]
    pub fn with_deadline(mut self, deadline: Deadline) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// 校验"写命令必须带幂等键"。
    ///
    /// `requires` 由 catalog 决定（`Capability::Write` / `Destructive`）。
    /// 放在这里而不是各端：三端各写一遍，必然有一端忘了。
    ///
    /// # Errors
    /// 写命令缺幂等键时返回 [`DomainError::InvalidArgument`]。
    pub fn check_idempotency(&self, requires: bool) -> Result<(), DomainError> {
        if requires && self.idempotency_key.is_none() {
            return Err(DomainError::invalid_field(
                "idempotency_key",
                "写命令必须带幂等键：没有它，重试一次就会产生两条记录",
            ));
        }
        Ok(())
    }
}

/// 命令结果。
///
/// 三态而不是 `Result<Value, Error>`：**空结果是正常的**（"没有更多消息"
/// 与"出错了"在 UI 上是完全不同的两个界面），塞进 `Ok(null)` 就会被
/// 前端当成"数据缺失"。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ActionOutcome {
    /// 有数据。
    Ok {
        /// 命令返回的业务数据。
        data: serde_json::Value,
    },
    /// 成功但没有数据（空列表、无未读、已是目标状态）。
    Empty,
    /// 失败。
    Failed {
        /// 领域错误。
        error: DomainError,
    },
}

/// 一条命令的结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionResult {
    /// 回显请求的命令 id，调用方据此对齐并发请求。
    pub action_id: ActionId,
    /// 结果三态。
    #[serde(flatten)]
    pub outcome: ActionOutcome,
    /// 执行后客户端应推进到的游标（增量命令返回）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
    /// 完成时间（unix 毫秒），用于本地观测与"多久没动"的判断。
    pub completed_at_ms: u64,
}

impl ActionResult {
    /// 成功且有数据。
    #[must_use]
    pub fn ok(action_id: ActionId, data: serde_json::Value, completed_at_ms: u64) -> Self {
        Self {
            action_id,
            outcome: ActionOutcome::Ok { data },
            cursor: None,
            completed_at_ms,
        }
    }

    /// 成功但没数据。
    #[must_use]
    pub fn empty(action_id: ActionId, completed_at_ms: u64) -> Self {
        Self {
            action_id,
            outcome: ActionOutcome::Empty,
            cursor: None,
            completed_at_ms,
        }
    }

    /// 失败。
    #[must_use]
    pub fn failed(action_id: ActionId, error: DomainError, completed_at_ms: u64) -> Self {
        Self {
            action_id,
            outcome: ActionOutcome::Failed { error },
            cursor: None,
            completed_at_ms,
        }
    }

    /// 挂上游标。
    #[must_use]
    pub fn with_cursor(mut self, cursor: Cursor) -> Self {
        self.cursor = Some(cursor);
        self
    }

    /// 是否成功（含空结果）。
    #[must_use]
    pub fn is_ok(&self) -> bool {
        !matches!(self.outcome, ActionOutcome::Failed { .. })
    }

    /// 转成便于 `?` 的形态：`Ok(Some(data))` / `Ok(None)`（空结果）/ `Err`。
    ///
    /// # Errors
    /// 结果失败时返回其中的 [`DomainError`]。
    pub fn into_payload(self) -> Result<Option<serde_json::Value>, DomainError> {
        match self.outcome {
            ActionOutcome::Ok { data } => Ok(Some(data)),
            ActionOutcome::Empty => Ok(None),
            ActionOutcome::Failed { error } => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(id: &str) -> ActionId {
        ActionId::parse(id).expect("action id")
    }

    fn trace() -> TraceId {
        TraceId::parse("t-1").expect("trace")
    }

    #[test]
    fn write_without_key_is_rejected_at_the_edge() {
        let req = ActionRequest::new(action("msg.send"), serde_json::json!({}), trace());
        let err = req.check_idempotency(true).expect_err("must fail");
        assert_eq!(err.code(), "invalid_argument");
        let key = IdempotencyKey::parse("k-1").expect("key");
        assert!(req.with_idempotency(key).check_idempotency(true).is_ok());
    }

    #[test]
    fn read_commands_need_no_key() {
        let req = ActionRequest::new(action("conv.list"), serde_json::json!({}), trace());
        assert!(req.check_idempotency(false).is_ok());
    }

    #[test]
    fn empty_is_not_an_error_and_not_null_data() {
        let result = ActionResult::empty(action("conv.list"), 1_000);
        assert!(result.is_ok());
        assert_eq!(result.clone().into_payload(), Ok(None));
        let json = serde_json::to_value(&result).expect("ser");
        assert_eq!(json["status"], "empty");
    }

    #[test]
    fn failed_result_carries_the_code_through() {
        let result = ActionResult::failed(action("msg.send"), DomainError::forbidden("no"), 1);
        let json = serde_json::to_value(&result).expect("ser");
        assert_eq!(json["status"], "failed");
        assert_eq!(json["error"]["code"], "forbidden");
        let back: ActionResult = serde_json::from_value(json).expect("de");
        assert_eq!(back, result);
    }

    #[test]
    fn request_round_trips() {
        let req = ActionRequest::new(
            action("msg.send"),
            serde_json::json!({"conv": "c1"}),
            trace(),
        )
        .with_idempotency(IdempotencyKey::parse("k-1").expect("key"))
        .with_deadline(Deadline::at(1_700_000_000_000));
        let json = serde_json::to_value(&req).expect("ser");
        assert_eq!(json["contract"], "1.0");
        assert_eq!(json["deadline"], "1700000000000");
        let back: ActionRequest = serde_json::from_value(json).expect("de");
        assert_eq!(back, req);
    }
}
