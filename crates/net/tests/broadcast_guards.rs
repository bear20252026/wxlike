//! 用户通道的边界（issue #13）：不能广播、不能伪造系统帧、直聊三元组必须自洽。
//!
//! 这三条以前都是"合法普通账号"能做的事：空 `to` 等于发给本 hub 所有人，
//! `Notice` 跳过关系过滤与发送闸门，`d-<a>~<b>` 里的 `to` 想填谁填谁。

use std::sync::Arc;
use std::time::Duration;

use wxlike_core::types::{MsgBody, MsgKind};
use wxlike_core::{Envelope, Store};
use wxlike_mq::{memory::MemoryMq, memory::MemoryRoutes, MqRouter};
use wxlike_net::{run_mq_hub_with, Client, MqHubOptions};

async fn hub() -> (Arc<Store>, std::net::SocketAddr) {
    let store = Arc::new(Store::open(None).unwrap());
    let hub = run_mq_hub_with(
        "hub-1",
        "127.0.0.1:0".parse().unwrap(),
        MqRouter::new(Arc::new(MemoryMq::new()), MemoryRoutes::new()),
        MqHubOptions {
            auth: Some(Arc::new(wxlike_net::auth::StaticAuth)),
            storage: Some(store.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let addr = hub.local_addr().unwrap();
    std::mem::forget(hub);
    (store, addr)
}

async fn connect(addr: std::net::SocketAddr, user: &str) -> Client {
    Client::connect(addr, "localhost", user).await.unwrap()
}

/// `within` 内**一条都不该有**。
async fn expect_silence(client: &Client, within: Duration, who: &str) {
    match tokio::time::timeout(within, client.next_envelope()).await {
        Err(_) => {}
        Ok(Ok(env)) => panic!("{who} 不该收到这条：{:?}", env.body),
        Ok(Err(err)) => panic!("{who} 连接出错：{err}"),
    }
}

/// 普通账号的空 `to` 广播：以前会发给本 hub 所有在线用户。
#[tokio::test]
async fn a_user_cannot_broadcast_to_everyone() {
    let (store, addr) = hub().await;
    let alice = connect(addr, "alice").await;
    let bob = connect(addr, "bob").await;
    let carol = connect(addr, "carol").await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    alice
        .send(&Envelope::text("lobby", "alice", 1, "hello everyone"))
        .await
        .unwrap();

    expect_silence(&bob, Duration::from_millis(400), "bob").await;
    expect_silence(&carol, Duration::from_millis(100), "carol").await;
    assert!(
        store.list_messages("lobby").unwrap().is_empty(),
        "广播帧不该落库"
    );
}

/// 未知群 ID + 空 `to`：同样不能"发散"出去。
#[tokio::test]
async fn an_unknown_group_id_does_not_fan_out() {
    let (store, addr) = hub().await;
    let alice = connect(addr, "alice").await;
    let bob = connect(addr, "bob").await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    alice
        .send(&Envelope::text("g-does-not-exist", "alice", 1, "anyone?"))
        .await
        .unwrap();
    expect_silence(&bob, Duration::from_millis(400), "bob").await;
    assert!(store.list_messages("g-does-not-exist").unwrap().is_empty());
}

/// 直聊 `d-<a>~<b>`：sender 必须是其中一员，`to` 必须是另一个人。
#[tokio::test]
async fn a_direct_conversation_must_match_its_participants() {
    let (store, addr) = hub().await;
    let alice = connect(addr, "alice").await;
    let bob = connect(addr, "bob").await;
    let carol = connect(addr, "carol").await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    // 1) 别人的直聊：alice 不在 d-bob~carol 里
    alice
        .send(&Envelope::direct(
            "d-bob~carol",
            "alice",
            vec!["carol".into()],
            1,
            "sneaky",
        ))
        .await
        .unwrap();
    expect_silence(&carol, Duration::from_millis(400), "carol").await;

    // 2) 自己在的直聊，但收件人填成第三个人
    alice
        .send(&Envelope::direct(
            "d-alice~bob",
            "alice",
            vec!["carol".into()],
            2,
            "wrong recipient",
        ))
        .await
        .unwrap();
    expect_silence(&carol, Duration::from_millis(400), "carol").await;

    // 3) 自洽的那条正常送达
    let ok = Envelope::direct("d-alice~bob", "alice", vec!["bob".into()], 3, "hi bob");
    alice.send(&ok).await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), bob.next_envelope())
        .await
        .expect("自洽的直聊必须送达")
        .unwrap();
    assert_eq!(got.msg_id, ok.msg_id);

    let rows = store.list_messages("d-alice~bob").unwrap();
    assert_eq!(rows.len(), 1, "只有自洽的那条落库");
}

/// 用户伪造 `Notice`（系统文案）：不送达、不落库；服务端入口不受影响。
#[tokio::test]
async fn a_user_cannot_forge_system_notices() {
    let (store, addr) = hub().await;
    let alice = connect(addr, "alice").await;
    let mallory = connect(addr, "mallory").await;
    tokio::time::sleep(Duration::from_millis(250)).await;

    let mut forged = Envelope::with_body(
        "d-alice~mallory",
        "mallory",
        vec!["alice".into()],
        1,
        MsgBody::Notice("你已被移出群聊".into()),
    );
    forged.kind = MsgKind::System;
    mallory.send(&forged).await.unwrap();

    expect_silence(&alice, Duration::from_millis(400), "alice").await;
    assert!(store.list_messages("d-alice~mallory").unwrap().is_empty());
}
