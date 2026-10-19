//! P3 read-service 切片：未读数与已读游标。
//!
//! 为什么不用"最大 seq − 已读 seq"：`messages.seq` 是**全局** AUTOINCREMENT，
//! 不同会话交错增长，差值会把别人的消息算进来。所以未读必须是区间计数。
//! 这个测试专门造两个会话交错发消息，就是为了守住这一点。
//!
//! 另一条硬性质：游标**只前进**。两台设备一台读到最新、另一台报旧值，
//! 未读数不能反弹——否则用户会看到"读过的消息又变成未读"。

mod common;

use common::{app, req, signup};
use serde_json::json;
use wxlike_core::store::Store;
use wxlike_core::Envelope;
use wxlike_platform::Platform;

/// 同一个 SQLite 文件上开 Platform（平台表）与 Store（消息表）——
/// 服务端就是这么起的，测试也必须这样，否则消息表根本不在 Platform 的连接里。
fn open(name: &str) -> (Platform, Store, String) {
    let path = std::env::temp_dir().join(format!("wxlike-read-{}-{name}.db", std::process::id()));
    let p = path.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&p);
    let platform = Platform::open(Some(&p)).expect("platform");
    let store = Store::open(Some(&p)).expect("store");
    (platform, store, p)
}

fn send(store: &Store, conv: &str, sender: &str, text: &str) {
    let env = Envelope::text(conv, sender, 0, text);
    store.append_message(&env, false).expect("append");
}

#[test]
fn unread_counts_only_this_conversation() {
    let (platform, store, path) = open("interleave");
    let a = "d-alice~bob";
    let b = "d-alice~carol";

    // 交错发：a,b,a,b,a —— 若用 MAX(seq)-read_seq 会把 b 的两条算进 a
    send(&store, a, "alice", "a1");
    send(&store, b, "alice", "b1");
    send(&store, a, "alice", "a2");
    send(&store, b, "alice", "b2");
    send(&store, a, "alice", "a3");

    let u = platform.conv_unread("bob", a).expect("unread a");
    assert_eq!(u.unread, 3, "a 会话只有 3 条，不能把 b 的算进来");
    assert_eq!(u.read_seq, 0);
    assert!(u.max_seq > 0);

    // alice 是 b 会话的参与者（bob 不是，会被 404 挡掉——那是另一条测试的事）
    let u = platform.conv_unread("alice", b).expect("unread b");
    assert_eq!(u.unread, 2, "b 会话 2 条");

    let _ = std::fs::remove_file(path);
}

#[test]
fn cursor_only_moves_forward_and_clamps() {
    let (platform, store, path) = open("monotonic");
    let conv = "d-alice~bob";
    for i in 0..5 {
        send(&store, conv, "alice", &format!("m{i}"));
    }

    let u = platform.conv_unread("bob", conv).unwrap();
    assert_eq!(u.unread, 5);
    let max = u.max_seq;

    // 读到最新
    let u = platform.mark_conv_read("bob", conv, None).unwrap();
    assert_eq!(u.unread, 0);
    assert_eq!(u.read_seq, max);

    // 另一台设备报旧游标（比如刚上线还没同步）：不能倒退
    let u = platform.mark_conv_read("bob", conv, Some(1)).unwrap();
    assert_eq!(u.read_seq, max, "游标只前进");
    assert_eq!(u.unread, 0);

    // 报一个超过最大 seq 的值：按最大 seq 截断，不留下"未来游标"
    let u = platform
        .mark_conv_read("bob", conv, Some(max + 999))
        .unwrap();
    assert_eq!(u.read_seq, max);
    assert_eq!(u.unread, 0);

    let _ = std::fs::remove_file(path);
}

#[test]
fn one_users_cursor_does_not_affect_another() {
    let (platform, store, path) = open("peruser");
    let conv = "d-alice~bob";
    send(&store, conv, "alice", "hi");
    send(&store, conv, "alice", "again");

    platform.mark_conv_read("bob", conv, None).unwrap();
    assert_eq!(platform.conv_unread("bob", conv).unwrap().unread, 0);
    assert_eq!(
        platform.conv_unread("alice", conv).unwrap().unread,
        2,
        "alice 自己还没读，未读数不受 bob 影响"
    );

    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn http_requires_auth_and_hides_foreign_conversations() {
    let router = app(common::platform());
    let (alice, token) = signup(&router, "alice").await;
    let (bob, _bob_token) = signup(&router, "bob").await;

    // 未登录 → 401
    let r = req(
        "GET",
        &format!("/api/conv/d-{alice}~{bob}/unread"),
        None,
        None,
    );
    let (status, _) = common::call(&router, r).await;
    assert_eq!(status, 401);

    // 自己的会话 → 200
    let r = req(
        "GET",
        &format!("/api/conv/d-{alice}~{bob}/unread"),
        None,
        Some(&token),
    );
    let (status, body) = common::call(&router, r).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["unread"], json!(0));

    // 别人的会话 → 404（不泄露"这个会话存在"）
    let r = req("GET", "/api/conv/d-x~y/unread", None, Some(&token));
    let (status, _) = common::call(&router, r).await;
    assert_eq!(status, 404);

    // 上报已读：空 body 也允许（等于"读到最新"）
    let r = req(
        "POST",
        &format!("/api/conv/d-{alice}~{bob}/read"),
        None,
        Some(&token),
    );
    let (status, body) = common::call(&router, r).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["unread"], json!(0));
}
