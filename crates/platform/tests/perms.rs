//! 权限矩阵契约（Rocket.Chat 的 roles × permissions 模型）。
//!
//! 重点不是"谁能做什么"这一张表，而是三条容易写错的性质：
//! 1. **提权只有群主能做**——管理员不能自我复制；
//! 2. **同级不能互踢**——管理员踢不动管理员，只能群主来；
//! 3. **判角色 vs 判权限点**——端点只认权限点，加权限不改端点。

mod common;

use axum::Router;
use common::{app, call, platform, req, signup};
use serde_json::json;
use wxlike_platform::Platform;

struct Fx {
    p: wxlike_platform::PlatformState,
    community: String,
    channel: String,
    owner: String,
    admin: String,
    member: String,
    outsider: String,
}

/// owner 建社区，admin/member 加入；admin 被 owner 提为管理员。
fn fx() -> Fx {
    let p = Platform::in_memory().unwrap();
    let owner = p.register("owner", "pw").unwrap();
    let admin = p.register("admin", "pw").unwrap();
    let member = p.register("member", "pw").unwrap();
    let outsider = p.register("outsider", "pw").unwrap();
    let community = p.create_community(&owner, "权限社区", 100).unwrap();
    p.join_community(&community, &admin).unwrap();
    p.join_community(&community, &member).unwrap();
    p.set_member_role(&community, &owner, &admin, "admin")
        .unwrap();
    let channel = p
        .create_channel(&community, &owner, "综合", "text")
        .unwrap();
    Fx {
        p: p.shared(),
        community,
        channel,
        owner,
        admin,
        member,
        outsider,
    }
}

#[test]
fn member_only_gets_post_message() {
    let f = fx();
    let perms = f.p.effective_perms(&f.community, &f.member).unwrap();
    assert_eq!(perms, vec!["post-message"]);
    assert!(!f
        .p
        .can(
            &f.community,
            &f.member,
            wxlike_platform::perms::Perm::ManageChannel
        )
        .unwrap());
    assert!(f
        .p
        .can(
            &f.community,
            &f.member,
            wxlike_platform::perms::Perm::PostMessage
        )
        .unwrap());
}

#[test]
fn outsider_has_no_perms_but_no_error() {
    let f = fx();
    assert!(f
        .p
        .effective_perms(&f.community, &f.outsider)
        .unwrap()
        .is_empty());
    assert!(!f
        .p
        .can(
            &f.community,
            &f.outsider,
            wxlike_platform::perms::Perm::PostMessage
        )
        .unwrap());
}

#[test]
fn admin_cannot_grant_admin_owner_can() {
    let f = fx();
    // 管理员给自己人提权 → 拒绝（防自我复制）
    assert!(f
        .p
        .set_member_role(&f.community, &f.admin, &f.member, "admin")
        .is_err());
    // 群主可以
    f.p.set_member_role(&f.community, &f.owner, &f.member, "admin")
        .unwrap();
    let perms = f.p.effective_perms(&f.community, &f.member).unwrap();
    assert!(perms.contains(&"manage-channel"));
    assert!(!perms.contains(&"manage-roles"), "管理员也没有提权权限");
}

#[test]
fn admin_can_demote_member_but_not_peer_admin() {
    let f = fx();
    let carol = f.p.register("carol", "pw").unwrap();
    f.p.join_community(&f.community, &carol).unwrap();
    f.p.set_member_role(&f.community, &f.owner, &carol, "admin")
        .unwrap();

    // admin 降级普通成员：可以
    f.p.set_member_role(&f.community, &f.admin, &f.member, "member")
        .unwrap();
    // admin 降级另一个 admin：不行（内斗防护）
    assert!(f
        .p
        .set_member_role(&f.community, &f.admin, &carol, "member")
        .is_err());
    // 群主可以
    f.p.set_member_role(&f.community, &f.owner, &carol, "member")
        .unwrap();
}

#[test]
fn cannot_demote_owner() {
    let f = fx();
    assert!(f
        .p
        .set_member_role(&f.community, &f.owner, &f.owner, "member")
        .is_err());
}

#[test]
fn kick_rules() {
    let f = fx();
    // 普通成员踢不动人
    assert!(f.p.kick_member(&f.community, &f.member, &f.admin).is_err());
    // 管理员能踢普通成员
    f.p.kick_member(&f.community, &f.admin, &f.member).unwrap();
    assert!(f
        .p
        .effective_perms(&f.community, &f.member)
        .unwrap()
        .is_empty());
    // 踢完就不在社区里了
    assert!(!f
        .p
        .can(
            &f.community,
            &f.member,
            wxlike_platform::perms::Perm::PostMessage
        )
        .unwrap());

    // 管理员之间不能互踢，群主可以
    let carol = f.p.register("carol", "pw").unwrap();
    f.p.join_community(&f.community, &carol).unwrap();
    f.p.set_member_role(&f.community, &f.owner, &carol, "admin")
        .unwrap();
    assert!(f.p.kick_member(&f.community, &f.admin, &carol).is_err());
    f.p.kick_member(&f.community, &f.owner, &carol).unwrap();
    // 群主永远踢不掉
    assert!(f.p.kick_member(&f.community, &f.owner, &f.owner).is_err());
}

#[test]
fn channel_rename_requires_manage_channel() {
    let f = fx();
    assert!(f
        .p
        .update_channel(&f.channel, &f.member, Some("改名"), None)
        .is_err());
    assert!(f
        .p
        .update_channel(&f.channel, &f.outsider, Some("改名"), None)
        .is_err());
    f.p.update_channel(&f.channel, &f.admin, Some("公告板"), Some("announce"))
        .unwrap();
    // 非法类型被拒
    assert!(f
        .p
        .update_channel(&f.channel, &f.admin, None, Some("hologram"))
        .is_err());
}

#[test]
fn announce_channel_blocks_member_posts() {
    let f = fx();
    let ann =
        f.p.create_channel(&f.community, &f.owner, "公告", "announce")
            .unwrap();
    assert!(
        !f.p.channel_send_allowed(&ann, &f.member).unwrap(),
        "成员不能发公告频道"
    );
    assert!(
        f.p.channel_send_allowed(&ann, &f.admin).unwrap(),
        "管理员可以"
    );
    // 普通频道成员可以
    assert!(f.p.channel_send_allowed(&f.channel, &f.member).unwrap());
}

#[test]
fn delete_channel_requires_permission() {
    let f = fx();
    assert!(f.p.delete_channel(&f.channel, &f.member).is_err());
    f.p.delete_channel(&f.channel, &f.admin).unwrap();
    assert!(
        f.p.delete_channel(&f.channel, &f.admin).is_err(),
        "删过的频道 404"
    );
}

#[tokio::test]
async fn http_surface() {
    let state = platform();
    let (owner, to) = signup(&app(state.clone()), "o").await;
    let (member, tm) = signup(&app(state.clone()), "m").await;
    let community = state.create_community(&owner, "HTTP 社区", 10).unwrap();
    state.join_community(&community, &member).unwrap();
    let channel = state
        .create_channel(&community, &owner, "c", "text")
        .unwrap();
    let app: Router = app(state);

    // 我的权限：owner 全量、member 只有 post-message
    let (status, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/communities/{community}/permissions"),
            None,
            Some(&to),
        ),
    )
    .await;
    assert_eq!(status, 200);
    let perms = body["perms"].as_array().unwrap();
    assert!(perms.contains(&json!("manage-roles")));

    let (_, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/communities/{community}/permissions"),
            None,
            Some(&tm),
        ),
    )
    .await;
    assert_eq!(body["perms"], json!(["post-message"]));

    // 成员改频道名 → 403；owner → 204
    let (status, _) = call(
        &app,
        req(
            "PATCH",
            &format!("/api/channels/{channel}"),
            Some(json!({"name": "hack"})),
            Some(&tm),
        ),
    )
    .await;
    assert_eq!(status, 403);
    let (status, _) = call(
        &app,
        req(
            "PATCH",
            &format!("/api/channels/{channel}"),
            Some(json!({"name": "改好了"})),
            Some(&to),
        ),
    )
    .await;
    assert_eq!(status, 204);

    // 踢人：成员踢人 → 403；owner 踢成员 → 204
    let (status, _) = call(
        &app,
        req(
            "DELETE",
            &format!("/api/communities/{community}/members/{member}"),
            None,
            Some(&tm),
        ),
    )
    .await;
    assert_eq!(status, 403);
    let (status, _) = call(
        &app,
        req(
            "DELETE",
            &format!("/api/communities/{community}/members/{member}"),
            None,
            Some(&to),
        ),
    )
    .await;
    assert_eq!(status, 204);

    // 角色字符串非法 → 422（`BadInput` 的既有映射是 UNPROCESSABLE_ENTITY）
    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/communities/{community}/members/{owner}/role"),
            Some(json!({"role": "god"})),
            Some(&to),
        ),
    )
    .await;
    assert_eq!(status, 422, "body={body}");
}
