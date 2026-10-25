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
    /// 非 2xx 时按状态码映射成 [`DomainError`]；2xx 但 body 不是 JSON 时算协议错误。
    pub fn json(&self) -> Result<serde_json::Value, DomainError> {
        if !self.is_success() {
            let hint = String::from_utf8_lossy(&self.body);
            let hint: String = hint.chars().take(200).collect();
            return Err(DomainError::from_http_status(self.status, &hint));
        }
        serde_json::from_slice(&self.body)
            .map_err(|err| DomainError::unavailable(format!("响应不是合法 JSON：{err}")))
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
