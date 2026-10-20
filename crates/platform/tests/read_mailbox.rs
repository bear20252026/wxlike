//! P3 接线验收：未读 / 已读 / 同步指令，全部走**已有**的信箱实现。
//!
//! 这一版不再自己存游标——`crates/message::MessageStore` 里的
//! `head_seq` / `unread` / `ack` / `sync_plan` 就是唯一真相。
//! 这里验证的是"接线对了"：conv_id → 属主三元组映射、HTTP 语义、权限边界。
//!
//! 覆盖范围：channel 已经写进信箱（`communities::send_channel_message`）。
//! direct/group 仍是 `core::Store` 扇出、未镜像，因此未读会是 0——那是接线
//! 未完成，不是算错（见 `docs/p3-mailbox.md`）。

mod common;

use common::{app, req, signup};
use serde_json::json;

#[tokio::test]
async fn channel_unread_ack_and_sync_over_http() {
    let state = common::platform();
    let router = app(state.clone());
    let (alice, ta) = signup(&router, "alice").await;
    let (bob, tb) = signup(&router, "bob").await;

    let cid = state
        .create_community(&alice, "team", 100)
        .expect("community");
    state.join_community(&cid, &bob).expect("join");
    let ch = state
        .create_channel(&cid, &alice, "general", "text")
        .expect("channel");

    for i in 0..3 {
        state
            .send_channel_message(&ch, &alice, "text", &format!("m{i}"))
            .await
            .expect("send");
    }

    // bob 未读 3，属主识别为 channel（靠 conv_owner 映射，不靠 id 前缀猜）
    let r = req("GET", &format!("/api/conv/{ch}/unread"), None, Some(&tb));
    let (status, body) = common::call(&router, r).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["unread"], json!(3), "{body}");
    assert_eq!(body["owner"], json!("channel"), "{body}");
    assert_eq!(body["head_seq"], json!(3), "{body}");

    // 读到最新（空 body 合法）
    let r = req("POST", &format!("/api/conv/{ch}/read"), None, Some(&tb));
    let (status, body) = common::call(&router, r).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["unread"], json!(0), "{body}");

    // 另一个人的游标不受影响
    let r = req("GET", &format!("/api/conv/{ch}/unread"), None, Some(&ta));
    let (_, body) = common::call(&router, r).await;
    assert_eq!(body["unread"], json!(3), "alice 没读，仍是 3：{body}");

    // 五层差分：落后就给出 delta 之类的指令，而不是让客户端全量拉
    let r = req(
        "GET",
        &format!("/api/conv/{ch}/sync?version=0"),
        None,
        Some(&ta),
    );
    let (status, body) = common::call(&router, r).await;
    assert_eq!(status, 200, "{body}");
    assert!(
        body["directive"].is_string(),
        "应返回指令类型而不是裸数组：{body}"
    );
}

/// 未登录不能读未读（游标属于个人）。
#[tokio::test]
async fn unread_requires_auth() {
    let router = app(common::platform());
    let (alice, _) = signup(&router, "alice").await;
    let r = req("GET", &format!("/api/conv/{alice}/unread"), None, None);
    let (status, _) = common::call(&router, r).await;
    assert_eq!(status, 401);
}

/// 建群时会把 owner_type 落表；direct 会话靠 `d-` 前缀推导。
#[tokio::test]
async fn owner_kind_is_recorded_or_derived() {
    use wxlike_message::OwnerType;
    let p = common::platform();
    let router = app(p.clone());
    let (alice, _) = signup(&router, "alice").await;
    let g = p.create_group(&alice, "g", &[]).expect("group");
    assert_eq!(p.owner_kind_of(&g).unwrap(), OwnerType::Group);
    assert_eq!(
        p.owner_kind_of("d-alice~bob").unwrap(),
        OwnerType::Direct,
        "直聊不需要落表，id 自带语义"
    );
}
