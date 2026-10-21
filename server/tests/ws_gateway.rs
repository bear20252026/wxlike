//! P3 ADR-8: WebSocket gateway — a browser-shaped WS client interoperates
//! with a QUIC client through the SAME hub path (same wire protocol, same
//! fan-out, same parking). This is the transport-abstraction acceptance:
//! the hub cannot tell the carriers apart, and neither can the business.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use wxlike_core::{Envelope, MsgBody, Store};
use wxlike_net::Client;
use wxlike_server::{start_stack, StackOptions};

/// Minimal WS-side client speaking the wxlike wire protocol: one binary
/// WebSocket message per bincode WireFrame; Hello first.
struct WsClient {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl WsClient {
    /// 握手：发**凭据**（不是 user_id），等 `HelloAck` 确认服务端认出的身份。
    ///
    /// 返回 `(client, 服务端认定的 user_id)`——测试要断言"服务端解析出的身份
    /// 与我们注册的那个一致"，而不是我们自己传什么就信什么（issue #3）。
    async fn connect(url: &str, token: &str) -> (Self, String) {
        let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let mut me = Self { socket };
        let hello = WireFrame::Hello {
            token: token.to_string(),
        };
        me.send_frame(&hello.to_bytes().unwrap()).await;

        let frame = me.socket.next().await.unwrap().unwrap();
        let bytes = match frame {
            Message::Binary(b) => b,
            other => panic!("expected binary HelloAck, got {other:?}"),
        };
        match WireFrame::from_bytes(&bytes).unwrap() {
            WireFrame::HelloAck { user_id, .. } => (me, user_id),
            other => panic!("expected HelloAck, got {other:?}"),
        }
    }

    async fn send_frame(&mut self, bytes: &[u8]) {
        self.socket
            .send(Message::Binary(bytes.to_vec()))
            .await
            .unwrap();
    }

    async fn send_text(
        &mut self,
        sender: &str,
        conv_id: &str,
        to: Vec<String>,
        text: &str,
    ) -> Envelope {
        let env = Envelope::direct(conv_id, sender, to, 0, text);
        let frame = WireFrame::Msg(env.clone()).to_bytes().unwrap();
        self.send_frame(&frame).await;
        env
    }

    async fn next_frame(&mut self) -> WireFrame {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(15), self.socket.next())
                .await
                .expect("ws frame within 15s")
                .expect("ws open")
                .unwrap();
            match msg {
                Message::Binary(b) => return WireFrame::from_bytes(&b).unwrap(),
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected ws frame: {other:?}"),
            }
        }
    }

    /// 跳过系统帧（Notice，好友/群事件）只取内容帧——befriend 的系统事件
    /// 必然先于被测内容到达。
    async fn next_text_frame(&mut self) -> Envelope {
        loop {
            match self.next_frame().await {
                WireFrame::Msg(env) => {
                    if !matches!(env.body, MsgBody::Notice(_)) {
                        return env;
                    }
                }
                other => panic!("expected envelope, got {other:?}"),
            }
        }
    }
}

use wxlike_net::wire::WireFrame;

/// HTTP 注册，返回 (user_id, token)。
fn register(stack: &wxlike_server::Stack, name: &str) -> (String, String) {
    let r: serde_json::Value = ureq::post(&format!("http://{}/api/register", stack.http_addr))
        .send_json(serde_json::json!({"name": name, "password": "pw"}))
        .unwrap()
        .into_json()
        .unwrap();
    (
        r["user_id"].as_str().unwrap().to_string(),
        r["token"].as_str().unwrap().to_string(),
    )
}

/// 好友流程（不变式 9：直发双方须为好友或已接受陌生人）。
fn befriend(stack: &wxlike_server::Stack, _a: &str, a_tok: &str, b: &str, b_tok: &str) {
    ureq::post(&format!("http://{}/api/friends/requests", stack.http_addr))
        .set("Authorization", &format!("Bearer {a_tok}"))
        .send_json(serde_json::json!({"to_user_id": b, "message": ""}))
        .unwrap();
    let incoming: serde_json::Value = ureq::get(&format!(
        "http://{}/api/friends/requests/incoming",
        stack.http_addr
    ))
    .set("Authorization", &format!("Bearer {b_tok}"))
    .call()
    .unwrap()
    .into_json()
    .unwrap();
    let id = incoming[0]["id"].as_str().unwrap().to_string();
    ureq::post(&format!(
        "http://{}/api/friends/requests/{id}/accept",
        stack.http_addr
    ))
    .set("Authorization", &format!("Bearer {b_tok}"))
    .call()
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_client_and_quic_client_interoperate() {
    let stack = start_stack(StackOptions::default()).await.unwrap();
    let _http = format!("http://{}", stack.http_addr);

    // alice over QUIC, carol over WS — the hub must not care
    let http = format!("http://{}", stack.http_addr);
    let (alice_uid, alice_tok) = register(&stack, "alice");
    let (carol_uid, carol_tok) = register(&stack, "carol");

    // 第三个参数是**凭据**：hub 从 token 解析身份（issue #3）
    let alice = Client::connect(stack.quic_addr, "localhost", &alice_tok)
        .await
        .unwrap();
    assert_eq!(alice.user_id, alice_uid, "服务端解析出的身份应与注册时一致");
    let ws_url = format!("ws://{}/ws", stack.http_addr);
    let (mut carol_ws, ws_uid) = WsClient::connect(&ws_url, &carol_tok).await;
    assert_eq!(ws_uid, carol_uid, "WS 侧身份也应来自服务端解析");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 不变式 9 生效后，直发双方须为好友
    befriend(&stack, &alice_uid, &alice_tok, &carol_uid, &carol_tok);

    let conv = wxlike_platform::direct_conv_id(&alice_uid, &carol_uid);

    // QUIC -> WS
    alice
        .send(&Envelope::direct(
            &conv,
            &alice_uid,
            vec![carol_uid.clone()],
            1,
            "from desktop",
        ))
        .await
        .unwrap();
    let env = carol_ws.next_text_frame().await;
    assert!(matches!(&env.body, MsgBody::Text(t) if t == "from desktop"));

    // WS -> QUIC
    let sent = carol_ws
        .send_text(&carol_uid, &conv, vec![alice_uid.clone()], "from browser")
        .await;
    // befriend 系统事件帧先于内容到达：按 msg_id 匹配目标帧
    let env = loop {
        let env = tokio::time::timeout(Duration::from_secs(15), alice.next_envelope())
            .await
            .expect("quic peer received")
            .unwrap();
        if env.msg_id == sent.msg_id {
            break env;
        }
    };
    assert!(matches!(&env.body, MsgBody::Text(t) if t == "from browser"));

    // the hub persisted both sides identically (transport-agnostic storage)
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rows = stack.store.list_messages(&conv).unwrap();
    assert_eq!(rows.len(), 2, "both directions in one timeline: {rows:?}");

    let _ = http;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_connection_participates_in_parking_like_quic() {
    let stack = start_stack(StackOptions::default()).await.unwrap();
    let ws_url = format!("ws://{}/ws", stack.http_addr);

    // carol (WS) never connects; alice (QUIC) writes; a WS device connecting
    // later must flush the parked frame exactly like the QUIC path
    let (alice_uid, alice_tok) = register(&stack, "alice");
    let (carol_uid, carol_tok) = register(&stack, "carol");
    let alice = Client::connect(stack.quic_addr, "localhost", &alice_tok)
        .await
        .unwrap();
    befriend(&stack, &alice_uid, &alice_tok, &carol_uid, &carol_tok);
    let conv = wxlike_platform::direct_conv_id(&alice_uid, &carol_uid);
    alice
        .send(&Envelope::direct(
            &conv,
            &alice_uid,
            vec![carol_uid.clone()],
            1,
            "while you were away",
        ))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let (mut carol_ws, _) = WsClient::connect(&ws_url, &carol_tok).await;
    let env = carol_ws.next_text_frame().await;
    assert!(matches!(&env.body, MsgBody::Text(t) if t == "while you were away"));

    // and the WS device going away must release the route: a second alice
    // message now parks again (is_online false after close)
    drop(carol_ws);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !stack.hub.is_online(&carol_uid).await,
        "ws close must unbind like a quic close"
    );
}

#[allow(dead_code)]
fn _keep_store_import(_: &Store) {}
