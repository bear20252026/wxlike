//! G02-09 设备侧联动的 net 层验收：`HubHandle::disconnect_user` /
//! `disconnect_device` 真的把连接从 hub 上摘掉，且对端能观察到关闭信号。
//!
//! 载体走 `HubHandle::attach_transport`（ADR-8 的 WS 路径，无 `ConnCloser`），
//! 所以测的是 `PeerConn::close()` → `FrameTx::close()` 这条链——生产里它
//! 对应 `WsTx::close`（发 WS Close 帧）。测试的 [`TestTx`] 在 `close` 时
//! 置位标志并 shutdown 写半，这样"对端读到 EOF"和"hub 表被清空"是两个
//! 独立断言，不会互相顶替。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::json;
use tokio::io::{AsyncWriteExt, DuplexStream};
use tower::ServiceExt as _;
use wxlike_mq::memory::{MemoryMq, MemoryRoutes};
use wxlike_mq::MqRouter;
use wxlike_net::auth::StaticAuth;
use wxlike_net::conn::{FrameRx, FrameTx, PeerConn};
use wxlike_net::offline::MemoryOffline;
use wxlike_net::policy::Limits;
use wxlike_net::proto::PROTOCOL_V3;
use wxlike_net::wire::WireFrame;
use wxlike_net::{framing, run_mq_hub_with, MqHub, MqHubOptions};

/// 写半：记录 `close` 是否被调用，并 shutdown 让对端读到 EOF。
struct TestTx {
    inner: DuplexStream,
    closed: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl FrameTx for TestTx {
    async fn send_frame(&mut self, frame: &[u8]) -> anyhow::Result<()> {
        framing::write_frame(&mut self.inner, frame).await?;
        Ok(())
    }

    async fn close(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.inner.shutdown().await;
    }
}

struct ReadHalf(DuplexStream);

#[async_trait::async_trait]
impl FrameRx for ReadHalf {
    async fn read_frame(&mut self) -> anyhow::Result<Vec<u8>> {
        Ok(framing::read_frame(&mut self.0).await?)
    }
}

struct Peer {
    to_hub: DuplexStream,
    from_hub: DuplexStream,
    closed: Arc<AtomicBool>,
}

impl Peer {
    fn attach(hub: &MqHub) -> Self {
        let (hub_out, client_in) = tokio::io::duplex(64 * 1024);
        let (client_out, hub_in) = tokio::io::duplex(64 * 1024);
        let closed = Arc::new(AtomicBool::new(false));
        let tx = TestTx {
            inner: hub_out,
            closed: Arc::clone(&closed),
        };
        hub.handle()
            .attach_transport(PeerConn::new(Box::new(tx), Box::new(ReadHalf(hub_in))));
        Self {
            to_hub: client_out,
            from_hub: client_in,
            closed,
        }
    }

    async fn send(&mut self, frame: &WireFrame) {
        let bytes = frame.to_bytes().expect("encode");
        framing::write_frame(&mut self.to_hub, &bytes)
            .await
            .expect("send frame");
    }

    /// 下一帧；连接已关（EOF / 解析失败 / 超时）返回 `None`。
    async fn next(&mut self) -> Option<WireFrame> {
        let read = framing::read_frame(&mut self.from_hub);
        match tokio::time::timeout(Duration::from_secs(2), read).await {
            Ok(Ok(bytes)) => WireFrame::from_bytes(&bytes).ok(),
            _ => None,
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// v3 握手（与 `proto_v3` 同形）：ClientHello → Hello → HelloAck + Auth。
async fn hello_v3(peer: &mut Peer, user: &str, device: &str) {
    peer.send(&WireFrame::ClientHello {
        device_id: device.to_string(),
        protocol: PROTOCOL_V3,
        caps: 0,
    })
    .await;
    peer.send(&WireFrame::Hello {
        token: user.to_string(),
    })
    .await;
    assert!(matches!(
        peer.next().await,
        Some(WireFrame::HelloAck { .. })
    ));
    match peer.next().await {
        Some(WireFrame::Auth {
            user_id, device_id, ..
        }) => assert_eq!((user_id.as_str(), device_id.as_str()), (user, device)),
        other => panic!("expected the v3 Auth frame, got {other:?}"),
    }
}

async fn hub() -> MqHub {
    run_mq_hub_with(
        "hub-gov",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            auth: Some(Arc::new(StaticAuth)),
            offline: MemoryOffline::new(500),
            limits: Limits::default(),
            ..Default::default()
        },
    )
    .await
    .expect("hub")
}

#[tokio::test]
async fn disconnect_user_drops_every_device_and_is_idempotent() {
    let hub = hub().await;
    let mut a = Peer::attach(&hub);
    hello_v3(&mut a, "u1", "d1").await;
    let mut b = Peer::attach(&hub);
    hello_v3(&mut b, "u1", "d2").await;
    assert_eq!(hub.handle().conn_count("u1").await, 2, "两台设备都该在");

    let dropped = hub.handle().disconnect_user("u1").await;
    assert_eq!(dropped, 2, "应断开两台设备");
    assert_eq!(hub.handle().conn_count("u1").await, 0);
    assert_eq!(hub.handle().online_users().await, 0);

    // 对端可观测：既置了关闭标志，也读到了 EOF（不是"过一会儿再说"）。
    assert!(a.is_closed() && b.is_closed(), "对端应收到关闭信号");
    assert!(a.next().await.is_none(), "被撤销的连接对端应读到 EOF");
    assert!(b.next().await.is_none());

    // 幂等：serve 循环随后也会走到 release，重复调用必须是 no-op。
    assert_eq!(
        hub.handle().disconnect_user("u1").await,
        0,
        "重复撤销不该再报断开数"
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn disconnect_device_only_touches_that_device() {
    let hub = hub().await;
    let mut a = Peer::attach(&hub);
    hello_v3(&mut a, "u1", "d1").await;
    let mut b = Peer::attach(&hub);
    hello_v3(&mut b, "u1", "d2").await;

    assert!(
        hub.handle().disconnect_device("u1", "d1").await,
        "目标设备存在，应断开"
    );
    assert!(a.is_closed(), "d1 应已关闭");
    assert!(!b.is_closed(), "d2 不该被误伤");
    assert_eq!(hub.handle().conn_count("u1").await, 1, "只剩 d2");
    assert_eq!(hub.handle().online_users().await, 1);

    // 重复撤销同一设备：幂等命中，不再有连接可断。
    assert!(
        !hub.handle().disconnect_device("u1", "d1").await,
        "重复撤销应返回 false"
    );
    // 未认领设备（空 device_id）的连接不会被按设备名误伤。
    assert!(
        !hub.handle().disconnect_device("u1", "ghost-device").await,
        "不存在的设备应返回 false"
    );
    assert_eq!(hub.handle().conn_count("u1").await, 1, "d2 仍在");
    hub.shutdown().await;
}

#[tokio::test]
async fn disconnect_unknown_user_is_a_noop() {
    let hub = hub().await;
    assert_eq!(
        hub.handle().disconnect_user("ghost").await,
        0,
        "没有连接的用户返回 0"
    );
    hub.shutdown().await;
}

// ---------------------------------------------------------------------------
// 端到端：真实平台 + 真实 hub
// ---------------------------------------------------------------------------

/// 封禁命令从 HTTP 管理端点一路打到 hub 断连。这是装配面（`server`）将来
/// 要串的那条链，这里用真实平台与真实 hub 钉住，而不是各层分别 mock。
#[tokio::test]
async fn end_to_end_admin_suspend_closes_live_hub_connection() {
    use wxlike_identity::{session, Audience};
    use wxlike_platform::{ChannelGovernanceSink, GovernanceDisconnect, Platform, PlatformOptions};

    // 平台（文件库：治理端口的第二条连接才有）。
    let dir = std::env::temp_dir().join(format!(
        "wxlike-gov-e2e-{}-{}",
        std::process::id(),
        wxlike_platform::random_hex(6)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("wxlike.db");
    let path_str = path.to_str().unwrap().to_string();
    let opts = PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        ..Default::default()
    };
    let platform = Platform::open_with(Some(&path_str), opts).unwrap();
    let (sink, mut cmds) = ChannelGovernanceSink::new();
    platform.set_governance_sink(Arc::new(sink));
    let app = Platform::router(platform.shared());

    let hub = hub().await;
    let hub_handle = hub.handle();
    tokio::spawn(async move {
        while let Some(cmd) = cmds.recv().await {
            match cmd {
                GovernanceDisconnect::User(u) => {
                    hub_handle.disconnect_user(&u).await;
                }
                GovernanceDisconnect::Device { user_id, device_id } => {
                    hub_handle.disconnect_device(&user_id, &device_id).await;
                }
            }
        }
    });

    // 受害者：注册 + 登录（制造一个可被撤销的会话）。
    let (status, body) = http(
        &app,
        "POST",
        "/api/register",
        Some(json!({"name": "e2e-victim", "password": "pw"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let victim = body["user_id"].as_str().unwrap().to_string();
    let (status, body) = http(
        &app,
        "POST",
        "/api/login",
        Some(json!({"name": "e2e-victim", "password": "pw"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "victim login: {body}");

    // 操作者：Admin audience 会话（普通用户令牌进不来）。
    let conn = rusqlite::Connection::open(&path_str).unwrap();
    conn.execute_batch("PRAGMA busy_timeout = 5000;").unwrap();
    let (status, body) = http(
        &app,
        "POST",
        "/api/register",
        Some(json!({"name": "e2e-operator", "password": "pw"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let operator = body["user_id"].as_str().unwrap().to_string();
    let reg = wxlike_identity::device::Registration {
        platform: "desktop".to_string(),
        label: "管理端".to_string(),
        os_version: "win11".to_string(),
        app_version: "1.0.0".to_string(),
        identity_key: "key-admin".to_string(),
    };
    wxlike_identity::device::register(
        &conn,
        &operator,
        "device-e2e-admin",
        &reg,
        wxlike_core::types::now_ms() as i64,
    )
    .unwrap();
    let grant = session::open(
        &conn,
        &operator,
        "device-e2e-admin",
        Audience::Admin,
        60_000,
        "127.0.0.1",
        wxlike_core::types::now_ms() as i64,
    )
    .unwrap();

    // 受害者连上 hub（StaticAuth 下 token 即 user_id）。
    let mut peer = Peer::attach(&hub);
    hello_v3(&mut peer, &victim, "e2e-dev-1").await;
    assert_eq!(hub.handle().conn_count(&victim).await, 1);

    let (status, receipt) = admin_http(
        &app,
        "POST",
        &format!("/api/admin/identity/users/{victim}/suspend"),
        Some(json!({"reason": "spam", "actor": "ops"})),
        &grant.access_token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert!(
        receipt["sessions_revoked"].as_u64().unwrap() >= 1,
        "封禁应撤销会话: {receipt}"
    );

    // 连接应被平台断开（命令经通道交给 hub，不是各层各自 mock）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !peer.is_closed() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "封禁后 hub 连接未断开"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(hub.handle().conn_count(&victim).await, 0, "hub 表已清空");
    assert!(peer.next().await.is_none(), "对端读到 EOF");
    hub.shutdown().await;
}

async fn http(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    bearer: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = bearer {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = b
        .body(match body {
            Some(v) => Body::from(v.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
    });
    (status, body)
}

/// 管理端请求：`x-admin-token` = Admin 会话令牌。
async fn admin_http(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
    operator_token: &str,
) -> (StatusCode, serde_json::Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    b = b.header("x-admin-token", operator_token);
    let req = b
        .body(match body {
            Some(v) => Body::from(v.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
    });
    (status, body)
}
