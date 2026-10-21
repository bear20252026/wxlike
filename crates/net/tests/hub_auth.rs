//! issue #3（P0）：hub 的身份**只**从凭据解析，客户端不能自报 `user_id`。
//!
//! 这个文件是那条修复的护栏。改动前的协议是 `Hello { user_id }`，hub 收到就
//! 注册 peers、绑路由、`offline.drain(user_id)`——而 `user_id` 是 16 位 hex，
//! 出现在每条消息的 `sender` 里。知道它就能冒充收实时消息、**取走并销毁**
//! 离线队列、以该身份发消息。
//!
//! 四条断言对应四种攻击面：
//!
//! 1. 没配认证器的 hub **拒绝一切**（fail closed，不是"不校验"）；
//! 2. 空凭据 / 未知凭据被拒；
//! 3. `HelloAck` 里的身份来自认证器，不是客户端说什么就是什么；
//! 4. 凭据被吊销后，长连接在下一次心跳时断开。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use wxlike_mq::memory::{MemoryMq, MemoryRoutes};
use wxlike_mq::MqRouter;
use wxlike_net::auth::{DenyAll, HubIdentity, StaticAuth};
use wxlike_net::offline::MemoryOffline;
use wxlike_net::{run_mq_hub_with, Client, MqHub, MqHubOptions};

async fn hub_with(
    auth: Option<Arc<dyn wxlike_net::HubAuthenticator>>,
) -> (MqHub, std::net::SocketAddr) {
    let hub = run_mq_hub_with(
        "hub-auth",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            auth,
            offline: MemoryOffline::new(64),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    (hub, addr)
}

/// 只认 `good-token`，映射到 `alice`。
fn alice_auth() -> Arc<dyn wxlike_net::HubAuthenticator> {
    Arc::new(|token: &str| {
        (token == "good-token").then(|| HubIdentity {
            user_id: "alice".into(),
            session_id: "sess-1".into(),
        })
    })
}

/// `Client` 没实现 `Debug`，所以用 match 而不是 `expect_err`。
async fn expect_connect_err(addr: std::net::SocketAddr, token: &str) -> String {
    match Client::connect(addr, "localhost", token).await {
        Ok(_) => panic!("token={token:?} 不该连上"),
        Err(err) => format!("{err:#}"),
    }
}

/// 1) 没有认证器 → 拒绝。**这条是 fail-closed 的护栏**：
///    如果有人为了"兼容老客户端"把 `None` 改成放行，这里会红。
#[tokio::test]
async fn hub_without_authenticator_refuses_every_connection() {
    let (_hub, addr) = hub_with(None).await;
    let text = expect_connect_err(addr, "alice").await;
    assert!(
        text.contains("no authenticator") || text.contains("not accepting"),
        "错误要说清是服务端没配认证器：{text}"
    );
}

/// 2) 空凭据、未知凭据都进不来。
#[tokio::test]
async fn empty_and_unknown_tokens_are_rejected() {
    let (_hub, addr) = hub_with(Some(alice_auth())).await;

    for token in ["", "wrong-token", "alice"] {
        let text = expect_connect_err(addr, token).await;
        assert!(
            text.contains("credential") || text.contains("hello"),
            "token={token:?} 的错误要能看出是认证失败：{text}"
        );
    }
}

/// 3) 身份来自认证器：客户端发 `good-token`，`HelloAck` 回的是 `alice`。
///
/// 这条同时证明"客户端无法声明自己是别人"——协议里根本没有 user_id 字段可填。
#[tokio::test]
async fn identity_comes_from_the_authenticator() {
    let (_hub, addr) = hub_with(Some(alice_auth())).await;
    let client = Client::connect(addr, "localhost", "good-token")
        .await
        .expect("有效凭据应当连上");
    assert_eq!(
        client.user_id, "alice",
        "user_id 必须是认证器给的，不是客户端传的 token 本身"
    );
}

/// 4) 吊销后长连接断开：HTTP 退出登录 / 远程下线要能踢掉 QUIC/WS。
#[tokio::test]
async fn revoked_credential_closes_the_long_connection() {
    struct Revocable {
        revoked: Arc<AtomicBool>,
    }
    impl wxlike_net::HubAuthenticator for Revocable {
        fn authenticate(&self, token: &str) -> Option<HubIdentity> {
            (token == "good-token").then(|| HubIdentity {
                user_id: "alice".into(),
                session_id: "sess-1".into(),
            })
        }
        fn is_revoked(&self, _token: &str) -> bool {
            self.revoked.load(Ordering::SeqCst)
        }
    }

    let revoked = Arc::new(AtomicBool::new(false));
    let (_hub, addr) = hub_with(Some(Arc::new(Revocable {
        revoked: revoked.clone(),
    })))
    .await;
    let client = Client::connect(addr, "localhost", "good-token")
        .await
        .expect("先要能连上");

    // 连上之后吊销，然后发一次心跳——服务端就是在收到 Ping 时复查的。
    revoked.store(true, Ordering::SeqCst);
    // 发 Ping 本身可能先成功（服务端读到之后才关），所以不把它的结果当断言。
    let _ = client.ping().await;

    let closed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if !client.is_alive() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        closed,
        "吊销后长连接应当关闭（当前实现：收到下一次 Ping 时复查）"
    );
}

/// 5) `DenyAll` 是"这个 hub 不该被外部连"的表达方式，与"没配"区分开。
#[tokio::test]
async fn deny_all_rejects_without_claiming_misconfiguration() {
    let (_hub, addr) = hub_with(Some(Arc::new(DenyAll))).await;
    let text = expect_connect_err(addr, "anything").await;
    assert!(
        text.contains("credential"),
        "应当是认证失败，而不是配置缺失：{text}"
    );
}

/// 顺带确认 `StaticAuth` 的语义（测试与本地开发用）：token 原样当 user_id。
#[tokio::test]
async fn static_auth_maps_token_to_user_id() {
    let (_hub, addr) = hub_with(Some(Arc::new(StaticAuth))).await;
    let client = Client::connect(addr, "localhost", "bob").await.unwrap();
    assert_eq!(client.user_id, "bob");
}
