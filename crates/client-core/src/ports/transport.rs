//! 网络端口。
//!
//! 核心只发两种东西：**一次性的请求**与**长活的订阅**。两者的失败语义不同
//! （请求可以重试，订阅要重连并从游标续），所以是两个方法而不是一个
//! "给我一个字节流"。
//!
//! 三条约束写进类型里：
//!
//! * 请求带 [`TransportRequest::command`]：服务端靠它记审计。历史上移动端
//!   绕过执行器自己拼 URL，审计里就少了那几条命令。
//! * 请求带 [`TransportRequest::deadline`]：绝对时间，不是"再给我 30 秒"。
//! * 订阅带游标：重连必须能续，不能从头重放（从头重放 = 用户看到消息重复）。

use wxlike_types::{ActionId, Deadline, DomainError, EventEnvelope, SubscribeRequest};

use super::MaybeSend;

/// HTTP 方法。只列核心真的会用到的。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HttpMethod {
    /// 读。
    Get,
    /// 新建 / 触发。
    Post,
    /// 全量替换。
    Put,
    /// 删除。
    Delete,
}

impl HttpMethod {
    /// 线上写法。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Delete => "DELETE",
        }
    }
}

/// 一条待发请求。
#[derive(Clone, Debug)]
pub struct TransportRequest {
    /// 方法。
    pub method: HttpMethod,
    /// 相对路径（`/api/...`），base 由实现决定。
    pub path: String,
    /// 请求体（JSON 文本或二进制）。
    pub body: Option<Vec<u8>>,
    /// 内容类型；`None` 时实现按"有没有 body"决定。
    pub content_type: Option<String>,
    /// 额外头（鉴权、追踪）。
    pub headers: Vec<(String, String)>,
    /// 审计用的命令 id。
    pub command: Option<ActionId>,
    /// 绝对截止时间。
    pub deadline: Option<Deadline>,
}

impl TransportRequest {
    /// 构造一个 GET。
    #[must_use]
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: HttpMethod::Get,
            path: path.into(),
            body: None,
            content_type: None,
            headers: Vec::new(),
            command: None,
            deadline: None,
        }
    }

    /// 构造一个 JSON POST。
    #[must_use]
    pub fn post_json(path: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            method: HttpMethod::Post,
            path: path.into(),
            body: Some(body),
            content_type: Some("application/json".to_string()),
            headers: Vec::new(),
            command: None,
            deadline: None,
        }
    }

    /// 打上审计标记。
    #[must_use]
    pub fn with_command(mut self, command: ActionId) -> Self {
        self.command = Some(command);
        self
    }

    /// 挂截止时间。
    #[must_use]
    pub fn with_deadline(mut self, deadline: Deadline) -> Self {
        self.deadline = Some(deadline);
        self
    }
}

/// 响应。状态码到领域错误的映射**不在这里**做（见
/// [`DomainError::from_http_status`]），传输层只管把字节搬回来。
#[derive(Clone, Debug)]
pub struct TransportResponse {
    /// HTTP 状态码。
    pub status: u16,
    /// 响应体。
    pub body: Vec<u8>,
}

impl TransportResponse {
    /// 是否 2xx。
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// 按 JSON 解析响应体。
    ///
    /// # Errors
    /// 非 2xx 时先把 body 里的 `code` 读成精确的 [`DomainError`]，读不到才按
    /// 状态码兜底（[`DomainError::from_http_status`]）；2xx 但 body 不是 JSON
    /// 时算协议错误。
    pub fn json(&self) -> Result<serde_json::Value, DomainError> {
        if !self.is_success() {
            return Err(self.domain_error());
        }
        serde_json::from_slice(&self.body)
            .map_err(|err| DomainError::unavailable(format!("响应不是合法 JSON：{err}")))
    }

    /// 非 2xx 响应体 → 领域错误。
    ///
    /// **顺序不能反**：先看 body 的 `code`，再看状态码。状态码会折叠语义——
    /// 403 与 409 各含两种领域错误，413/428 以前更是折成 `Unavailable`，
    /// 于是客户端对着"文件太大"和"需要人机验证"重试 5 次。`code` 不分叉，
    /// 所以它优先。
    ///
    /// 以下三种情况退回状态码兜底：body 不是 JSON、没有 `code`、`code` 不在
    /// 契约码表里。**不猜**——宁可粗一点，也不要编一个码表外的分类。
    fn domain_error(&self) -> DomainError {
        let message = self.hint();
        let code = serde_json::from_slice::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|body| body.get("code").and_then(|c| c.as_str()).map(str::to_owned))
            .unwrap_or_default();
        // 空码（没有 `code`）与码表外的码走同一条兜底：不猜。
        DomainError::from_contract_code(&code, &message)
            .unwrap_or_else(|| DomainError::from_http_status(self.status, &message))
    }

    /// 服务端原文。body 是 JSON 且带 `error` 时只取那一句——否则界面上会
    /// 直接显示 `{"error":...}`。非 JSON（网关 HTML、纯文本）原样截断。
    fn hint(&self) -> String {
        if let Ok(body) = serde_json::from_slice::<serde_json::Value>(&self.body) {
            if let Some(message) = body.get("error").and_then(|m| m.as_str()) {
                return message.chars().take(200).collect();
            }
        }
        String::from_utf8_lossy(&self.body)
            .chars()
            .take(200)
            .collect()
    }
}

/// 传输层失败。
///
/// 与 [`DomainError`] 的区别：这里只有"网络怎么了"，没有业务语义。
/// 映射在一处做（[`TransportError::into_domain`]），别在调用点各写一遍。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// 断网 / DNS 失败 / 连接被拒。
    Offline(String),
    /// 超过截止时间。
    Timeout,
    /// 协议层错误（TLS、非法响应、帧错误）。
    Protocol(String),
    /// 被调用方取消。
    Cancelled,
}

impl TransportError {
    /// 映射成领域错误。
    #[must_use]
    pub fn into_domain(self) -> DomainError {
        match self {
            TransportError::Offline(detail) => {
                DomainError::unavailable(format!("网络不可用：{detail}"))
            }
            TransportError::Timeout => DomainError::timeout("请求超时"),
            TransportError::Protocol(detail) => {
                DomainError::unavailable(format!("传输协议错误：{detail}"))
            }
            TransportError::Cancelled => DomainError::cancelled(),
        }
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Offline(detail) => write!(f, "网络不可用：{detail}"),
            TransportError::Timeout => f.write_str("超时"),
            TransportError::Protocol(detail) => write!(f, "协议错误：{detail}"),
            TransportError::Cancelled => f.write_str("已取消"),
        }
    }
}

impl std::error::Error for TransportError {}

/// 事件流。`next()` 返回 `None` 表示流正常结束（服务端要求换连接）。
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait EventSource: MaybeSend {
    /// 下一条事件。
    ///
    /// # Errors
    /// 断线、超时、协议错误。调用方按 [`TransportError::into_domain`] 分类。
    async fn next(&mut self) -> Result<Option<EventEnvelope>, TransportError>;

    /// 主动关闭。幂等。
    async fn close(&mut self);
}

/// 网络端口。
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait Transport: MaybeSend {
    /// 发一条请求。
    ///
    /// 实现必须做到：截止时间到就放弃、请求带上 `command` 审计标记、
    /// 不自己重试（重试策略在核心，只有一处）。
    ///
    /// # Errors
    /// 见 [`TransportError`]。
    async fn send(&self, request: TransportRequest) -> Result<TransportResponse, TransportError>;

    /// 打开事件流。
    ///
    /// # Errors
    /// 建连失败时返回 [`TransportError`]。
    async fn subscribe(
        &self,
        request: SubscribeRequest,
    ) -> Result<Box<dyn EventSource>, TransportError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, body: &str) -> TransportResponse {
        TransportResponse {
            status,
            body: body.as_bytes().to_vec(),
        }
    }

    /// 服务端给了精确 `code` 时按它分类，文案用 body 里的 `error`。
    #[test]
    fn body_code_wins_over_status() {
        let err = response(
            409,
            r#"{"error":"already exists: email","code":"conflict"}"#,
        )
        .json()
        .unwrap_err();
        assert_eq!(err.code(), "conflict");
        assert_eq!(err.message(), "already exists: email");
        assert!(!err.is_retryable());
    }

    /// 同一个 409，`blocked` 与 `conflict` 必须分开——状态码做不到。
    #[test]
    fn blocked_is_not_conflict() {
        let err = response(409, r#"{"error":"wallet not empty","code":"blocked"}"#)
            .json()
            .unwrap_err();
        assert_eq!(err.code(), "blocked");
        assert_eq!(err.message(), "wallet not empty");
    }

    /// 413 / 428：精确码 + 不可重试。这是"文件太大重试 5 次"那个 bug 的回归网。
    #[test]
    fn too_large_and_captcha_are_not_retried() {
        let too_large = response(
            413,
            r#"{"error":"media too large","code":"payload_too_large"}"#,
        )
        .json()
        .unwrap_err();
        assert_eq!(too_large.code(), "payload_too_large");
        assert!(!too_large.is_retryable());

        let captcha = response(
            428,
            r#"{"error":"human verification required","code":"precondition"}"#,
        )
        .json()
        .unwrap_err();
        assert_eq!(captcha.code(), "precondition");
        assert!(!captcha.is_retryable());
    }

    /// 503 可重试、500 不可重试：分工不能被状态码折叠掉。
    #[test]
    fn unavailable_is_retryable_but_internal_is_not() {
        let down = response(
            503,
            r#"{"error":"dependency unavailable: storage","code":"unavailable"}"#,
        )
        .json()
        .unwrap_err();
        assert_eq!(down.code(), "unavailable");
        assert!(down.is_retryable());

        let boom = response(
            500,
            r#"{"error":"internal dependency unavailable","code":"internal"}"#,
        )
        .json()
        .unwrap_err();
        assert_eq!(boom.code(), "internal");
        assert!(!boom.is_retryable());
        assert!(!boom.is_user_facing());
    }

    /// 没有 `code` 的老服务端 / 网关：退回状态码，文案保持可读（不塞裸 JSON）。
    #[test]
    fn falls_back_to_status_when_there_is_no_code() {
        let err = response(404, r#"{"error":"not found"}"#)
            .json()
            .unwrap_err();
        assert_eq!(err.code(), "not_found");
        assert!(err.message().contains("not found"));
        assert!(!err.message().contains('{'), "兜底文案不该是裸 JSON");
    }

    /// body 不是 JSON、或 `code` 不在契约码表里：一律退回状态码，不猜。
    #[test]
    fn unknown_or_malformed_bodies_fall_back_to_status() {
        let plain = response(502, "<html>bad gateway</html>")
            .json()
            .unwrap_err();
        assert_eq!(plain.code(), "internal", "502 是 5xx");

        let unknown = response(418, r#"{"error":"teapot","code":"SOMETHING_ELSE"}"#)
            .json()
            .unwrap_err();
        assert_eq!(unknown.code(), "unavailable", "码表外的 code 不猜");
    }

    #[test]
    fn success_parses_json_and_rejects_garbage() {
        let ok = response(200, r#"{"ok":true}"#).json().unwrap();
        assert_eq!(ok["ok"], true);

        let bad = response(200, "not json").json().unwrap_err();
        assert_eq!(bad.code(), "unavailable");
    }
}
