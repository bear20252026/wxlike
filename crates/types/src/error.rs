//! 领域错误：三端共用的错误码。
//!
//! 一个 `String` 错误没法做三件事：判断要不要重试、判断该不该弹给用户、
//! 判断该不该上报。所以错误必须**分类**，而且分类必须在契约里：
//!
//! * `code` 是 serde 的标签，也是 TS / Dart 生成类型的判别字段；
//! * 改名 = 破坏兼容（有测试钉住 [`CODES`] 与序列化结果一致）；
//! * `is_retryable()` 只在一处判断——以前重试策略散在 4 个地方，
//!   403 会被重试 5 次，用户等 30 秒才看到"你不是群成员"。

use std::fmt;

use serde::{Deserialize, Serialize};

/// 领域错误。
///
/// JSON 形态：`{"code":"unauthorized","message":"..."}`，
/// 限流额外带 `retry_after_ms`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum DomainError {
    /// 参数错误（缺字段、类型不对、越界）。
    InvalidArgument {
        /// 人能读的原因。
        message: String,
        /// 哪个字段错了（可选）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
    },
    /// 没登录 / token 失效。
    Unauthorized {
        /// 人能读的原因。
        message: String,
    },
    /// 登录了但没权限。
    Forbidden {
        /// 人能读的原因。
        message: String,
    },
    /// 目标不存在（或不该让调用方知道它存在）。
    NotFound {
        /// 人能读的原因。
        message: String,
    },
    /// 与当前状态冲突（重复提交、版本过期）。
    Conflict {
        /// 人能读的原因。
        message: String,
    },
    /// 前置条件不满足（未验证、未开权限）。
    Precondition {
        /// 人能读的原因。
        message: String,
    },
    /// 被限流。
    RateLimited {
        /// 人能读的原因。
        message: String,
        /// 建议等待多少毫秒。
        retry_after_ms: u64,
    },
    /// 超时（本地预算用完，或服务端 408/504）。
    Timeout {
        /// 人能读的原因。
        message: String,
    },
    /// 调用方取消。
    Cancelled {
        /// 人能读的原因。
        message: String,
    },
    /// 服务不可用（断网、5xx、依赖挂了）。
    Unavailable {
        /// 人能读的原因。
        message: String,
    },
    /// 服务端内部错误。
    Internal {
        /// 人能读的原因。
        message: String,
    },
}

/// 全部错误码，顺序与枚举一致。生成代码与测试都读它。
pub const CODES: [&str; 11] = [
    "invalid_argument",
    "unauthorized",
    "forbidden",
    "not_found",
    "conflict",
    "precondition",
    "rate_limited",
    "timeout",
    "cancelled",
    "unavailable",
    "internal",
];

/// TS 端（`core-contract.ts` 的 `ErrorCode`）的错误码全集，SCREAMING_SNAKE。
pub const WIRE_CODES: [&str; 11] = [
    "VALIDATION",
    "UNAUTHENTICATED",
    "FORBIDDEN",
    "VALIDATION",
    "CONFLICT",
    "REQUIRES_CONFIRMATION",
    "RATE_LIMITED",
    "DEPENDENCY_UNAVAILABLE",
    "CANCELLED",
    "OFFLINE",
    "INTERNAL",
];

impl DomainError {
    /// 稳定的错误码（就是 JSON 里的 `code`）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            DomainError::InvalidArgument { .. } => "invalid_argument",
            DomainError::Unauthorized { .. } => "unauthorized",
            DomainError::Forbidden { .. } => "forbidden",
            DomainError::NotFound { .. } => "not_found",
            DomainError::Conflict { .. } => "conflict",
            DomainError::Precondition { .. } => "precondition",
            DomainError::RateLimited { .. } => "rate_limited",
            DomainError::Timeout { .. } => "timeout",
            DomainError::Cancelled { .. } => "cancelled",
            DomainError::Unavailable { .. } => "unavailable",
            DomainError::Internal { .. } => "internal",
        }
    }

    /// 人能读的原因。
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            DomainError::InvalidArgument { message, .. }
            | DomainError::Unauthorized { message }
            | DomainError::Forbidden { message }
            | DomainError::NotFound { message }
            | DomainError::Conflict { message }
            | DomainError::Precondition { message }
            | DomainError::RateLimited { message, .. }
            | DomainError::Timeout { message }
            | DomainError::Cancelled { message }
            | DomainError::Unavailable { message }
            | DomainError::Internal { message } => message,
        }
    }

    /// 值得重试吗？
    ///
    /// **不重试**的一律是"再来一次也是这个结果"：参数错、没权限、不存在、
    /// 状态冲突、被取消。`Internal` 不重试：服务端内部错误重试通常只是把
    /// 同样的故障再打一遍，交给上层熔断。
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            DomainError::RateLimited { .. }
                | DomainError::Timeout { .. }
                | DomainError::Unavailable { .. }
        )
    }

    /// 该不该让用户看到原始 `message`。
    ///
    /// 内部错误与未知错误不展示：里面常带服务端路径、SQL、主机名。
    #[must_use]
    pub fn is_user_facing(&self) -> bool {
        !matches!(self, DomainError::Internal { .. })
    }

    /// 参数错误。
    #[must_use]
    pub fn invalid(message: impl Into<String>) -> Self {
        DomainError::InvalidArgument {
            message: message.into(),
            field: None,
        }
    }

    /// 参数错误（指明字段）。
    #[must_use]
    pub fn invalid_field(field: impl Into<String>, message: impl Into<String>) -> Self {
        DomainError::InvalidArgument {
            message: message.into(),
            field: Some(field.into()),
        }
    }

    /// 未授权。
    #[must_use]
    pub fn unauthorized(message: impl Into<String>) -> Self {
        DomainError::Unauthorized {
            message: message.into(),
        }
    }

    /// 无权限。
    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        DomainError::Forbidden {
            message: message.into(),
        }
    }

    /// 不存在。
    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        DomainError::NotFound {
            message: message.into(),
        }
    }

    /// 状态冲突。
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        DomainError::Conflict {
            message: message.into(),
        }
    }

    /// 超时。
    #[must_use]
    pub fn timeout(message: impl Into<String>) -> Self {
        DomainError::Timeout {
            message: message.into(),
        }
    }

    /// 被取消。
    #[must_use]
    pub fn cancelled() -> Self {
        DomainError::Cancelled {
            message: "调用已取消".to_string(),
        }
    }

    /// 服务不可用。
    #[must_use]
    pub fn unavailable(message: impl Into<String>) -> Self {
        DomainError::Unavailable {
            message: message.into(),
        }
    }

    /// 服务端内部错误。
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        DomainError::Internal {
            message: message.into(),
        }
    }

    /// HTTP 状态码 → 领域错误。**只在这一处**做映射。
    ///
    /// 服务端返回的 body 若已经是 `DomainError` 的 JSON，优先用它的 `message`
    /// 与 `code`（调用方见 `error_from_response`）；这个函数只处理"body 不是
    /// 结构化错误"的兜底。
    #[must_use]
    pub fn from_http_status(status: u16, body_hint: &str) -> Self {
        let hint = if body_hint.is_empty() {
            String::new()
        } else {
            format!("（服务端：{body_hint}）")
        };
        match status {
            400 | 422 => DomainError::invalid(format!("请求参数不被接受{hint}")),
            401 => DomainError::unauthorized(format!("未登录或登录已过期{hint}")),
            403 => DomainError::forbidden(format!("没有权限{hint}")),
            404 => DomainError::not_found(format!("目标不存在{hint}")),
            408 | 504 => DomainError::timeout(format!("服务端超时{hint}")),
            409 => DomainError::conflict(format!("状态冲突{hint}")),
            412 => DomainError::Precondition {
                message: format!("前置条件不满足{hint}"),
            },
            429 => DomainError::RateLimited {
                message: format!("请求过于频繁{hint}"),
                retry_after_ms: 1_000,
            },
            500..=599 => DomainError::Internal {
                message: format!("服务端错误 {status}{hint}"),
            },
            _ => DomainError::Unavailable {
                message: format!("未预期的响应 {status}{hint}"),
            },
        }
    }

    /// 映射到 TS 端（`core-contract.ts` 的 `ErrorCode`）的 SCREAMING_SNAKE 码。
    ///
    /// TS 的码表与本枚举不是一一对应：`NOT_FOUND` 在 TS 端没有对应位
    /// （目标不存在按"输入所指无效"折到 `VALIDATION`）；`DEPENDENCY_UNAVAILABLE`
    /// 与 `OFFLINE` 拆开了超时/不可用两态。**只在这一处**做这个映射，
    /// `client-web` 的 wasm 出口直接调它。
    #[must_use]
    pub fn to_wire_code(&self) -> &'static str {
        match self {
            DomainError::InvalidArgument { .. } => "VALIDATION",
            DomainError::Unauthorized { .. } => "UNAUTHENTICATED",
            DomainError::Forbidden { .. } => "FORBIDDEN",
            DomainError::NotFound { .. } => "VALIDATION",
            DomainError::Conflict { .. } => "CONFLICT",
            DomainError::Precondition { .. } => "REQUIRES_CONFIRMATION",
            DomainError::RateLimited { .. } => "RATE_LIMITED",
            DomainError::Timeout { .. } => "DEPENDENCY_UNAVAILABLE",
            DomainError::Cancelled { .. } => "CANCELLED",
            DomainError::Unavailable { .. } => "OFFLINE",
            DomainError::Internal { .. } => "INTERNAL",
        }
    }

    /// [`DomainError::to_wire_code`] 的逆映射：TS 端的错误码 → 领域错误。
    ///
    /// `NOT_IMPLEMENTED` 在本枚举没有对应位，折成 `InvalidArgument`
    /// （"再来一次也是这个结果"，不进重试）。**未知码折成 `Internal`**：
    /// 宁可保守地不重试、不透出原文，也不猜。
    #[must_use]
    pub fn from_wire_code(code: &str) -> Self {
        match code {
            "UNAUTHENTICATED" => DomainError::unauthorized("登录已失效，请重新登录"),
            "FORBIDDEN" => DomainError::forbidden("没有权限执行该操作"),
            "CONFLICT" => DomainError::conflict("数据已变化，请刷新后重试"),
            "REQUIRES_CONFIRMATION" => DomainError::Precondition {
                message: "需要再次确认".to_string(),
            },
            "RATE_LIMITED" => DomainError::RateLimited {
                message: "操作过于频繁，请稍后重试".to_string(),
                retry_after_ms: 1_000,
            },
            "DEPENDENCY_UNAVAILABLE" => DomainError::timeout("依赖服务暂不可用"),
            "OFFLINE" => DomainError::unavailable("网络已断开"),
            "CANCELLED" => DomainError::cancelled(),
            "INTERNAL" => DomainError::internal("内部错误，请稍后重试"),
            // TS 端有、本枚举没有：不重试的"能力未接入"
            "NOT_IMPLEMENTED" => DomainError::invalid("该能力尚未接入"),
            "VALIDATION" => DomainError::invalid("输入有误，请检查后重试"),
            // 未知码保守折 INTERNAL 语义：不重试、不透出原文
            _ => DomainError::internal("内部错误，请稍后重试"),
        }
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code(), self.message())
    }
}

impl std::error::Error for DomainError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_matches_serde_tag_for_every_variant() {
        let samples = [
            DomainError::invalid("x"),
            DomainError::unauthorized("x"),
            DomainError::forbidden("x"),
            DomainError::not_found("x"),
            DomainError::conflict("x"),
            DomainError::Precondition {
                message: "x".to_string(),
            },
            DomainError::RateLimited {
                message: "x".to_string(),
                retry_after_ms: 1,
            },
            DomainError::timeout("x"),
            DomainError::cancelled(),
            DomainError::unavailable("x"),
            DomainError::internal("x"),
        ];
        assert_eq!(samples.len(), CODES.len());
        for (sample, expected) in samples.iter().zip(CODES) {
            assert_eq!(sample.code(), expected);
            let json = serde_json::to_value(sample).expect("ser");
            assert_eq!(json["code"], expected, "serde 标签与 code() 漂移");
        }
    }

    #[test]
    fn retry_policy_is_explicit() {
        assert!(DomainError::timeout("t").is_retryable());
        assert!(DomainError::unavailable("u").is_retryable());
        assert!(DomainError::RateLimited {
            message: "r".to_string(),
            retry_after_ms: 10
        }
        .is_retryable());
        assert!(!DomainError::forbidden("f").is_retryable());
        assert!(!DomainError::internal("i").is_retryable());
        assert!(!DomainError::cancelled().is_retryable());
    }

    #[test]
    fn internal_errors_are_not_shown_verbatim() {
        assert!(!DomainError::internal("sql: select * from users").is_user_facing());
        assert!(DomainError::not_found("群不存在").is_user_facing());
    }

    #[test]
    fn http_mapping_covers_the_cases_clients_care_about() {
        assert_eq!(
            DomainError::from_http_status(401, "").code(),
            "unauthorized"
        );
        assert_eq!(DomainError::from_http_status(403, "").code(), "forbidden");
        assert_eq!(DomainError::from_http_status(404, "").code(), "not_found");
        assert_eq!(
            DomainError::from_http_status(429, "").code(),
            "rate_limited"
        );
        assert_eq!(DomainError::from_http_status(503, "").code(), "internal");
        assert_eq!(DomainError::from_http_status(200, "").code(), "unavailable");
    }

    /// 每个变体：`to_wire_code` 落在 TS 端 `ErrorCode` 全集里，且逆映射
    /// 回**同一个变体**。唯一的例外是 `NotFound`——TS 端没有 `NOT_FOUND` 位，
    /// 按 `core-contract.ts` 的口径折到 `VALIDATION`（→ `InvalidArgument`），
    /// 这里单独断言。
    #[test]
    fn wire_code_round_trips_every_variant() {
        let samples = [
            DomainError::invalid("x"),
            DomainError::unauthorized("x"),
            DomainError::forbidden("x"),
            DomainError::not_found("x"),
            DomainError::conflict("x"),
            DomainError::Precondition {
                message: "x".to_string(),
            },
            DomainError::RateLimited {
                message: "x".to_string(),
                retry_after_ms: 1,
            },
            DomainError::timeout("x"),
            DomainError::cancelled(),
            DomainError::unavailable("x"),
            DomainError::internal("x"),
        ];
        assert_eq!(samples.len(), CODES.len());
        for sample in &samples {
            let wire = sample.to_wire_code();
            assert!(
                [
                    "UNAUTHENTICATED",
                    "FORBIDDEN",
                    "VALIDATION",
                    "CONFLICT",
                    "RATE_LIMITED",
                    "REQUIRES_CONFIRMATION",
                    "DEPENDENCY_UNAVAILABLE",
                    "NOT_IMPLEMENTED",
                    "OFFLINE",
                    "CANCELLED",
                    "INTERNAL"
                ]
                .contains(&wire),
                "{wire} 不在 TS 端 ErrorCode 全集里"
            );
            if matches!(sample, DomainError::NotFound { .. }) {
                // 例外：TS 无 NOT_FOUND 位，折到 VALIDATION（→ InvalidArgument）
                assert_eq!(wire, "VALIDATION");
                assert!(matches!(
                    DomainError::from_wire_code(wire),
                    DomainError::InvalidArgument { .. }
                ));
                continue;
            }
            let back = DomainError::from_wire_code(wire);
            assert!(
                std::mem::discriminant(&back) == std::mem::discriminant(sample),
                "{wire} 往返后变体漂移：{:?} → {:?}",
                sample.code(),
                back.code()
            );
            // 可重试性不能在往返中翻转（UI 的重试按钮据此显示）
            assert_eq!(back.is_retryable(), sample.is_retryable());
        }
    }

    /// wire-only 的码与未知输入：`NOT_IMPLEMENTED` 不重试，未知码保守折 INTERNAL
    /// 语义（不透出原文、不重试）。
    #[test]
    fn wire_only_codes_map_to_safe_defaults() {
        let not_implemented = DomainError::from_wire_code("NOT_IMPLEMENTED");
        assert_eq!(not_implemented.code(), "invalid_argument");
        assert!(!not_implemented.is_retryable());
        let unknown = DomainError::from_wire_code("SOMETHING_ELSE");
        assert_eq!(unknown.code(), "internal");
        assert!(!unknown.is_retryable());
        assert!(!unknown.is_user_facing());
        // TS 全集里其余码都落到语义对应的变体（抽查两处关键分流）
        assert_eq!(
            DomainError::from_wire_code("DEPENDENCY_UNAVAILABLE").code(),
            "timeout"
        );
        assert_eq!(DomainError::from_wire_code("OFFLINE").code(), "unavailable");
        assert_eq!(DomainError::from_wire_code("CANCELLED").code(), "cancelled");
    }
}
