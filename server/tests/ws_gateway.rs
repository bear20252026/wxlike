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
    async fn connect(url: &str, user_id: &str) -> Self {
        let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let mut me = Self { socket };
        let hello = WireFrame::Hello {
            user_id: user_id.to_string(),
        };
        me.send_frame(&hello.to_bytes().unwrap()).await;
        me
    }

    async fn send_frame(&mut self, bytes: &[u8]) {
        self.socket
            .send(Message::Binary(bytes.to_vec()))
            .await
            .unwrap();
    }

    async fn send_text(&mut self, conv_id: &str, to: Vec<String>, text: &str) -> Envelope {
        let env = Envelope::direct(conv_id, "carol", to, 0, text);
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
}

use wxlike_net::wire::WireFrame;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_client_and_quic_client_interoperate() {
    let stack = start_stack(StackOptions::default()).await.unwrap();
    let http = format!("http://{}", stack.http_addr);

    // alice over QUIC, carol over WS — the hub must not care
    let alice = Client::connect(stack.quic_addr, "localhost", "alice")
        .await
        .unwrap();
    let ws_url = format!("ws://{}/ws", stack.http_addr);
    let mut carol_ws = WsClient::connect(&ws_url, "carol").await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let conv = wxlike_platform::direct_conv_id("alice", "carol");

    // QUIC -> WS
    alice
        .send(&Envelope::direct(
            &conv,
            "alice",
            vec!["carol".into()],
            1,
            "from desktop",
        ))
        .await
        .unwrap();
    let env = match carol_ws.next_frame().await {
        WireFrame::Msg(env) => env,
        other => panic!("expected envelope, got {other:?}"),
    };
    assert!(matches!(&env.body, MsgBody::Text(t) if t == "from desktop"));

    // WS -> QUIC
    let sent = carol_ws
        .send_text(&conv, vec!["alice".into()], "from browser")
        .await;
    let env = tokio::time::timeout(Duration::from_secs(15), alice.next_envelope())
        .await
        .expect("quic peer received")
        .unwrap();
    assert_eq!(env.msg_id, sent.msg_id, "same envelope crossed carriers");
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
    let alice = Client::connect(stack.quic_addr, "localhost", "alice")
        .await
        .unwrap();
    alice
        .send(&Envelope::direct(
            "d-alice~carol",
            "alice",
            vec!["carol".into()],
            1,
            "while you were away",
        ))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let mut carol_ws = WsClient::connect(&ws_url, "carol").await;
    let env = match carol_ws.next_frame().await {
        WireFrame::Msg(env) => env,
        other => panic!("expected parked envelope, got {other:?}"),
    };
    assert!(matches!(&env.body, MsgBody::Text(t) if t == "while you were away"));

    // and the WS device going away must release the route: a second alice
    // message now parks again (is_online false after close)
    drop(carol_ws);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !stack.hub.is_online("carol").await,
        "ws close must unbind like a quic close"
    );
}

#[allow(dead_code)]
fn _keep_store_import(_: &Store) {}
