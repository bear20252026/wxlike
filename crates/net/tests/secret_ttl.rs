//! AR-6（第四轮审计 #30）：秘密会话 TTL 在实时链路的最后一跳生效。
//!
//! HTTP 检索/搜索面的 TTL 过滤已有 server 侧测试（secret_enforcement）；
//! 这里钉住的是**离线补发路径**：过期的秘密帧在 QUIC 客户端重连补发时
//! 必须被丢弃——"到期即不可再取"不能只覆盖 HTTP。两条实时链路（QUIC/WS）
//! 共用这条 claim→ack 补发路径，net 层一处判定即全覆盖。

use std::sync::Arc;
use std::time::Duration;

use wxlike_core::{Envelope, MsgBody, MsgKind};
use wxlike_mq::{memory::MemoryMq, memory::MemoryRoutes, MqRouter};
use wxlike_net::policy::DeliveryPolicy;
use wxlike_net::{run_mq_hub_with, Client, MqHubOptions};

/// 只有 `d-a~b` 是启用了 1s TTL 的秘密会话；其余会话不受约束。
struct SecretTtlPolicy;

#[async_trait::async_trait]
impl DeliveryPolicy for SecretTtlPolicy {
    async fn blocked(&self, _from: &str, _to: &str) -> bool {
        false
    }
    async fn muted(&self, _group: &str, _user: &str) -> bool {
        false
    }
    async fn direct_allowed(&self, _from: &str, _to: &str) -> bool {
        true
    }
    async fn secret_ttl_ms(&self, conv_id: &str) -> Option<u64> {
        (conv_id == "d-a~b").then_some(1_000)
    }
}

fn envelope(conv: &str, text: &str, sent_at_ms: u64) -> Envelope {
    Envelope {
        version: 1,
        conv_id: conv.to_string(),
        sender: "a".to_string(),
        to: vec!["bob".to_string()],
        seq: 0,
        kind: MsgKind::Text,
        body: MsgBody::Text(text.to_string()),
        sent_at_ms,
        msg_id: Envelope::new_msg_id(),
        quote_of: None,
        receipt: None,
    }
}

#[tokio::test]
async fn expired_secret_frame_is_not_redelivered_on_reconnect() {
    let router = MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let hub = run_mq_hub_with(
        "hub-ttl",
        "127.0.0.1:0".parse().unwrap(),
        router,
        MqHubOptions {
            policy: Some(Arc::new(SecretTtlPolicy)),
            auth: Some(Arc::new(wxlike_net::auth::StaticAuth)),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let now = wxlike_core::types::now_ms();
    // 已过期 10s 的秘密消息：发送后 bob 一直离线，帧停在补发队列里
    hub.handle()
        .publish(&envelope("d-a~b", "burned after reading", now - 10_000))
        .await
        .unwrap();
    // 同一会话的新鲜消息（TTL 1s 内）
    hub.handle()
        .publish(&envelope("d-a~b", "still fresh", now))
        .await
        .unwrap();

    // bob 重连：补发必须只给新鲜的
    let bob = Client::connect(hub.local_addr().unwrap(), "localhost", "bob")
        .await
        .unwrap();
    let mut got = Vec::new();
    for _ in 0..2 {
        if let Ok(Ok(env)) = tokio::time::timeout(Duration::from_secs(3), bob.next_envelope()).await
        {
            got.push(env);
        }
    }
    let texts: Vec<String> = got
        .iter()
        .filter_map(|e| match &e.body {
            MsgBody::Text(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert!(
        !texts.iter().any(|t| t.contains("burned")),
        "expired secret frame must not be redelivered: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t == "still fresh"),
        "fresh frame must still arrive: {texts:?}"
    );
}

#[tokio::test]
async fn expired_frame_in_plain_conv_is_still_delivered() {
    let router = MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let hub = run_mq_hub_with(
        "hub-plain",
        "127.0.0.1:0".parse().unwrap(),
        router,
        MqHubOptions {
            policy: Some(Arc::new(SecretTtlPolicy)),
            auth: Some(Arc::new(wxlike_net::auth::StaticAuth)),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // 普通会话没有 TTL：再旧的消息也要补发（TTL 只约束秘密会话）
    let now = wxlike_core::types::now_ms();
    hub.handle()
        .publish(&envelope("d-c~d", "old but plain", now - 86_400_000))
        .await
        .unwrap();

    let bob = Client::connect(hub.local_addr().unwrap(), "localhost", "bob")
        .await
        .unwrap();
    let env = tokio::time::timeout(Duration::from_secs(3), bob.next_envelope())
        .await
        .expect("plain conv frame must survive")
        .expect("frame decode");
    match env.body {
        MsgBody::Text(t) => assert_eq!(t, "old but plain"),
        other => panic!("unexpected body: {other:?}"),
    }
}

/// M-11（#32）：优雅停机把本 hub 从路由表注销——多节点下其他 hub 不再
/// 向它投递，残留路由不会被误当成"这台 hub 还活着"。
#[tokio::test]
async fn hub_shutdown_unregisters_from_route_table() {
    let router = MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new());
    let hubs = router.routes.clone();
    let hub = run_mq_hub_with(
        "hub-by",
        "127.0.0.1:0".parse().unwrap(),
        router,
        MqHubOptions {
            auth: Some(Arc::new(wxlike_net::auth::StaticAuth)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(hubs
        .list_hubs()
        .await
        .unwrap()
        .contains(&"hub-by".to_string()));

    hub.shutdown().await;
    assert!(
        !hubs
            .list_hubs()
            .await
            .unwrap()
            .contains(&"hub-by".to_string()),
        "shutdown must unregister the hub from the route table"
    );
}
