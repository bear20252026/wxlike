//! S4 / ADR-29：宿主装配层的实时事件源。
//!
//! 分层（dependency_tree 守卫）：客户端闭包（client-native 等）**不许**链接
//! wxlike-net——net 含 axum（hub 服务端），链接它等于把 Web 框架塞进 APK。
//! 所以纯映射 `envelope_to_event` 放在 client-native（仅 core+types），而把
//! QUIC 客户端包装成规范 `EventSource` 的活儿放在**宿主层**（本 crate 本就
//! 链接 net）。桌面与未来的原生壳各自在宿主层做同样的包装，共用同一份映射。
//!
//! 连接生命周期（建连/鉴权/重连）由宿主持有 `Client` 负责；本结构只消费一个
//! **已连接**的客户端，不自己建连。

use wxlike_client_core::ports::{EventSource, TransportError};
use wxlike_client_native::realtime::envelope_to_event;
use wxlike_types::EventEnvelope;

/// 把宿主无关的 QUIC 客户端（[`wxlike_net::Client`]）包装成规范 [`EventSource`]。
/// `next()` 拉一条线上信封并经 `envelope_to_event` 映射成规范事件。
pub struct QuicEventSource {
    client: wxlike_net::Client,
    closed: bool,
}

impl QuicEventSource {
    /// 用一个已连接的客户端构造事件源。
    #[must_use]
    pub fn new(client: wxlike_net::Client) -> Self {
        Self {
            client,
            closed: false,
        }
    }
}

#[async_trait::async_trait]
impl EventSource for QuicEventSource {
    async fn next(&mut self) -> Result<Option<EventEnvelope>, TransportError> {
        if self.closed {
            return Ok(None);
        }
        match self.client.next_envelope().await {
            Ok(env) => Ok(Some(envelope_to_event(&env))),
            // 连接断了：流结束（None），调用方据此重连/重订阅。
            Err(_) => Ok(None),
        }
    }

    async fn close(&mut self) {
        self.closed = true;
    }
}