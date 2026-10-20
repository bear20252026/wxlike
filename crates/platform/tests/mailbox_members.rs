//! P3 步骤 5：**成员名单归信箱**。
//!
//! 扇出要回答"这条消息发给谁"。以前这个答案来自平台的 `group_members` 表，
//! 现在来自信箱的 `member_snapshot`——平台在每次成员变更时全量重置快照。
//!
//! 这些测试盯的是"变更点有没有漏同步"。漏一处，那个人就永远收不到消息，
//! 而且没有任何报错——只能靠用户投诉发现。

mod common;

use common::{app, signup};
use wxlike_message::OwnerType;
use wxlike_platform::Platform;

/// 平台 + 两个用户，返回 (platform, alice, bob)。
async fn setup() -> (std::sync::Arc<Platform>, String, String) {
    let p = common::platform();
    let router = app(p.clone());
    let (alice, _) = signup(&router, "alice").await;
    let (bob, _) = signup(&router, "bob").await;
    (p, alice, bob)
}

fn members(p: &Platform, group: &str) -> Vec<String> {
    p.message_store()
        .expect("mailbox")
        .members((OwnerType::Group, group))
        .expect("members")
}

#[tokio::test]
async fn create_group_seeds_the_mailbox_snapshot() {
    let (p, alice, bob) = setup().await;
    let g = p
        .create_group(&alice, "team", std::slice::from_ref(&bob))
        .expect("group");
    let mut got = members(&p, &g);
    got.sort();
    let mut want = vec![alice.clone(), bob.clone()];
    want.sort();
    assert_eq!(got, want, "建群后信箱里应有全部成员");
}

#[tokio::test]
async fn add_and_remove_keep_the_snapshot_in_sync() {
    let (p, alice, bob) = setup().await;
    let g = p.create_group(&alice, "team", &[]).expect("group");
    assert_eq!(members(&p, &g), vec![alice.clone()], "只有群主");

    p.add_members(&g, &alice, std::slice::from_ref(&bob))
        .expect("add");
    assert!(
        members(&p, &g).contains(&bob),
        "拉人后必须出现在快照里：{:?}",
        members(&p, &g)
    );

    p.remove_member(&g, &alice, &bob).expect("remove");
    assert!(
        !members(&p, &g).contains(&bob),
        "踢人后必须从快照里消失：{:?}",
        members(&p, &g)
    );
}

#[tokio::test]
async fn leaving_removes_the_member_from_the_snapshot() {
    let (p, alice, bob) = setup().await;
    let g = p
        .create_group(&alice, "team", std::slice::from_ref(&bob))
        .expect("group");
    assert!(members(&p, &g).contains(&bob));

    p.leave_group(&g, &bob).expect("leave");
    assert!(
        !members(&p, &g).contains(&bob),
        "退群后必须从快照里消失：{:?}",
        members(&p, &g)
    );
}
