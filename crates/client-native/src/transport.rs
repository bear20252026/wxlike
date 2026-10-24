//! 原生传输：[`Transport`] 在桌面/移动端的实现。
//!
//! **不重写协议层**。鉴权头、审计标记（`X-Wxlike-Command`）、4xx/5xx 的
//! 人话文案都在 `wxlike-client` 里，这里只是把它包成异步端口：
//! `ureq` 是阻塞的，直接调用会卡住 tokio 的 executor 线程（表现是发消息时
//! 界面掉帧），所以每次请求都 `spawn_blocking`。
//!
//! 实时通道（WebSocket / 长连接）**不在这一层**：移动端的实时性由推送 +
//! 主动拉取承担（`crates/ffi` 的边界说明），桌面端的 WS 在 `ui/src-tauri`。
//! 这里返回一个明确的"未接入"错误，而不是假装订阅成功。

use serde_json::Value;
use wxlike_client::Http;
use wxlike_client_core::ports::{
    EventSource, HttpMethod, Transport, TransportError, TransportRequest, TransportResponse,
};
use wxlike_types::{Deadline, SubscribeRequest};

/// 基于 `wxlike-client` 的原生传输。
#[derive(Clone)]
pub struct HttpTransport {
    http: Http,
}

impl HttpTransport {
    /// 用服务端 base URL 构造。
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            http: Http::new(base),
        }
    }

    /// 带上已有的 bearer token。
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.http.token = Some(token.into());
        self
    }

    /// 换 token（登录/刷新之后）。
    pub fn set_token(&mut self, token: Option<String>) {
        self.http.token = token;
    }

    /// 底层 `Http`（上传/下载这类非 JSON 通道还要用它）。
    #[must_use]
    pub fn inner(&self) -> &Http {
        &self.http
    }
}

/// 把请求体字节解成 JSON。
fn body_value(body: Option<&Vec<u8>>) -> Result<Option<Value>, TransportError> {
    match body {
        None => Ok(None),
        Some(bytes) => serde_json::from_slice(bytes)
            .map(Some)
            .map_err(|err| TransportError::Protocol(format!("请求体不是合法 JSON：{err}"))),
    }
}

/// 截止时间检查：过期就别发。
fn check_deadline(deadline: Option<Deadline>, now_ms: i64) -> Result<(), TransportError> {
    match deadline {
        Some(deadline) if deadline.expired(now_ms) => Err(TransportError::Timeout),
        _ => Ok(()),
    }
}

/// 当前 unix 毫秒。
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Transport for HttpTransport {
    async fn send(&self, request: TransportRequest) -> Result<TransportResponse, TransportError> {
        check_deadline(request.deadline, now_ms())?;
        let http = match &request.command {
            Some(command) => self.http.with_command(command.as_str()),
            None => self.http.clone(),
        };
        let body = body_value(request.body.as_ref())?;
        let method = method_text(request.method).to_string();
        let path = request.path.clone();
        let joined =
            tokio::task::spawn_blocking(move || http.json(&method, &path, body.as_ref())).await;
        match joined {
            Ok(Ok((status, value))) => Ok(TransportResponse {
                status,
                body: value.to_string().into_bytes(),
            }),
            // 连接层失败：断网、DNS、TLS、连接被拒——都归到 Offline，
            // 由 `TransportError::into_domain` 统一映射成 unavailable。
            Ok(Err(message)) => Err(TransportError::Offline(message)),
            Err(join_error) => Err(TransportError::Protocol(format!(
                "传输线程异常：{join_error}"
            ))),
        }
    }

    async fn subscribe(
        &self,
        _request: SubscribeRequest,
    ) -> Result<Box<dyn EventSource>, TransportError> {
        Err(TransportError::Protocol(
            "原生实时通道未在这一层接入：桌面端走 ui/src-tauri 的 WS，移动端走推送 + 拉取"
                .to_string(),
        ))
    }
}

fn method_text(method: HttpMethod) -> &'static str {
    method.as_str()
}
