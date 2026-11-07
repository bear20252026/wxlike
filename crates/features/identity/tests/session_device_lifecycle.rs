//! #103 账号会话与设备生命周期的验收面（真实 SQLite，非内存仓储）。
//!
//! 验收原文要求的是四件事，这里逐件钉住：
//!
//! 1. **过期 / 撤销 / 重复刷新全拒绝**；
//! 2. **一端移除设备后，该设备的 API 与刷新令牌同时失效**；
//! 3. **管理面令牌与普通账号令牌受众分开**，双向都不通用；
//! 4. **登出撤销指定会话并给出缓存清理事件**。
//!
//! 另加两类：重复提交幂等（同设备重复注册、重复移除、重复登出）、
//! 越权（A 不能操作 B 的设备/会话）。

use wxlike_identity::{
    device::{self, Trust},
    refresh,
    session::{self, SessionRow},
    step_up, Audience, IdError,
};

mod common;
use common::{add_device, db, login};

const DEV1: &str = "device-aaaaaaaa";
const DEV2: &str = "device-bbbbbbbb";

#[test]
fn open_authenticate_and_logout_roundtrip_with_cache_events() {
    let conn = db();
    let (access, refresh_token) = login(&conn, "u1", DEV1, Audience::User);

    let ctx = session::authenticate(&conn, &access, Audience::User, 1_500).expect("auth ok");
    assert_eq!(ctx.user_id, "u1");
    assert_eq!(ctx.device_id, DEV1);
    assert!(!ctx.sid.is_empty(), "会话必须有可用于登出的 sid");

    let out = session::logout(&conn, "u1", &ctx.sid, 1_600).expect("logout");
    assert_eq!(out.refresh_revoked, 1, "登出要一并作废刷新链");
    assert!(
        out.cache_events
            .iter()
            .any(|e| e.scope == "session" && e.key == ctx.sid),
        "必须给出 session 维度的缓存清理事件，否则其它节点的会话缓存留着旧令牌"
    );
    assert!(out.cache_events.iter().any(|e| e.scope == "user"));

    // 登出后两把令牌都死了。
    assert!(matches!(
        session::authenticate(&conn, &access, Audience::User, 1_700),
        Err(IdError::Unauthorized(_))
    ));
    assert!(matches!(
        refresh::rotate(&conn, &refresh_token, 1_700),
        Err(IdError::Unauthorized(_))
    ));
    // 重复登出同一个 sid：说清楚"这个会话没了"。
    assert!(matches!(
        session::logout(&conn, "u1", &ctx.sid, 1_800),
        Err(IdError::NotFound(_))
    ));
}

#[test]
fn expired_session_is_rejected_and_row_removed() {
    let conn = db();
    add_device(&conn, "u1", DEV1);
    let grant =
        session::open(&conn, "u1", DEV1, Audience::User, 60_000, "1.2.3.4", 1_000).expect("open");

    // 窗口内可用。
    assert!(session::authenticate(&conn, &grant.access_token, Audience::User, 1_001).is_ok());
    // 过期即拒（同一句话不区分"过期"和"无效"，避免给探测者线索）。
    let err = session::authenticate(&conn, &grant.access_token, Audience::User, 61_001)
        .expect_err("过期令牌必须拒绝");
    assert!(matches!(err, IdError::Unauthorized(_)));
    assert!(
        !err.to_string().contains(&grant.access_token),
        "错误信息里不许出现令牌本身"
    );

    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "过期令牌鉴权时就该顺手删行，不能靠后台慢慢清");
}

#[test]
fn purge_expired_leaves_live_sessions_alone() {
    let conn = db();
    add_device(&conn, "u1", DEV1);
    add_device(&conn, "u1", DEV2);
    let dead = session::open(&conn, "u1", DEV1, Audience::User, 1_000, "ip", 1_000).unwrap();
    let alive = session::open(&conn, "u1", DEV2, Audience::User, 1_000, "ip", 2_000).unwrap();

    let removed = session::purge_expired(&conn, 2_500).unwrap();
    assert!(removed >= 1, "至少要清掉那一条死的，实际 {removed}");
    assert!(session::authenticate(&conn, &alive.access_token, Audience::User, 2_500).is_ok());
    assert!(session::authenticate(&conn, &dead.access_token, Audience::User, 2_500).is_err());
    // 清理是幂等的：再跑一次不会误伤活会话。
    let again = session::purge_expired(&conn, 2_500).unwrap();
    assert_eq!(again, 0);
}

#[test]
fn audience_is_a_hard_boundary_in_both_directions() {
    let conn = db();
    let (user_token, _) = login(&conn, "u1", DEV1, Audience::User);
    let (admin_token, _) = login(&conn, "ops", DEV2, Audience::Admin);

    // 各自的正面都能过。
    assert!(session::authenticate(&conn, &user_token, Audience::User, 1_500).is_ok());
    assert!(session::authenticate(&conn, &admin_token, Audience::Admin, 1_500).is_ok());
    // 交叉一律拒绝，而且不给"这是管理令牌"这类额外信息。
    let e1 = session::authenticate(&conn, &user_token, Audience::Admin, 1_500)
        .expect_err("用户令牌进不了管理面");
    let e2 = session::authenticate(&conn, &admin_token, Audience::User, 1_500)
        .expect_err("管理令牌不该被当成用户令牌用");
    for e in [e1, e2] {
        assert!(matches!(e, IdError::Unauthorized(_)), "实际: {e}");
    }
    // 列表里也要如实标出受众，运维页据此决定撤销谁。
    let rows = session::list(&conn, "ops", 1_600).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.audience).collect::<Vec<_>>(),
        vec![Audience::Admin]
    );
    let rows = session::list(&conn, "u1", 1_600).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.audience).collect::<Vec<_>>(),
        vec![Audience::User]
    );
}

#[test]
fn removing_a_device_kills_its_api_and_refresh_but_nothing_else() {
    let conn = db();
    let (access, refresh_token) = login(&conn, "u1", DEV1, Audience::User);
    let (other_access, other_refresh) = login(&conn, "u1", DEV2, Audience::User);
    let ctx = session::authenticate(&conn, &access, Audience::User, 1_500).unwrap();
    // 移除之前，这台设备上刚验证出来的敏感操作授权是有效的。
    step_up::grant_default(&conn, "u1", &ctx.sid, "password.change", 1_550).expect("grant");
    assert!(step_up::require(&conn, "u1", &ctx.sid, "password.change", 1_560).is_ok());

    let out = device::revoke(&conn, "u1", DEV1, 1_600).expect("revoke");
    assert_eq!(out.device_id, DEV1);
    assert!(out.sessions >= 1, "该设备的会话要被清掉");
    assert!(out.refresh >= 1, "该设备的刷新链要被作废");

    assert!(matches!(
        session::authenticate(&conn, &access, Audience::User, 1_700),
        Err(IdError::Unauthorized(_))
    ));
    assert!(matches!(
        refresh::rotate(&conn, &refresh_token, 1_700),
        Err(IdError::Unauthorized(_))
    ));
    // 另一台设备完全不受影响——这是"一端移除"的关键边界。
    assert!(session::authenticate(&conn, &other_access, Audience::User, 1_700).is_ok());
    assert!(refresh::rotate(&conn, &other_refresh, 1_700).is_ok());
    // 绑在这台设备上的敏感操作授权也必须一起消失（与 logout 同强度）。
    let e = step_up::require(&conn, "u1", &ctx.sid, "password.change", 1_700)
        .expect_err("设备已移除，敏感操作授权不许还能用");
    assert!(matches!(e, IdError::Unauthorized(_)), "实际: {e}");
    let e = step_up::consume(&conn, "u1", &ctx.sid, "password.change", 1_700)
        .expect_err("设备已移除，敏感操作授权不许还能被消费");
    assert!(matches!(e, IdError::Unauthorized(_)), "实际: {e}");
}

#[test]
fn device_revoke_is_idempotent_and_scoped_to_the_owner() {
    let conn = db();
    login(&conn, "u1", DEV1, Audience::User);

    let first = device::revoke(&conn, "u1", DEV1, 2_000).unwrap();
    assert!(first.sessions >= 1);
    let second = device::revoke(&conn, "u1", DEV1, 2_100).expect("重复移除必须幂等成功");
    assert_eq!(second.sessions, 0, "第二次不该再动任何会话");
    assert_eq!(second.refresh, 0);

    // 越权：B 移除 A 的设备。
    let err = device::revoke(&conn, "u2", DEV2, 2_200).expect_err("别人的设备动不了");
    assert!(matches!(err, IdError::NotFound(_)), "实际: {err}");
    // 而且报 not found 而不是 forbidden——不存在与不属于你是同一个答案。
    assert!(!err.to_string().contains(DEV2));

    // 设备行仍保留，只是被标记撤销（审计与历史消息签名要它）。
    let row = device::get(&conn, DEV1).unwrap().expect("设备行还在");
    assert_eq!(row.trust, Trust::Revoked);
    assert!(!row.is_active());
    assert_eq!(device::active_count(&conn, "u1").unwrap(), 0);
    assert!(
        device::list(&conn, "u1", false).unwrap().is_empty(),
        "默认列表不含已撤销"
    );
    assert_eq!(
        device::list(&conn, "u1", true).unwrap().len(),
        1,
        "显式要求才给历史"
    );
}

#[test]
fn registering_the_same_device_twice_updates_instead_of_multiplying() {
    let conn = db();
    let first = add_device(&conn, "u1", DEV1);
    assert_eq!(first.trust, Trust::Unverified);

    // 同一台设备、同一个身份密钥：只刷元数据，不产生第二行。
    let reg = device::Registration {
        platform: "desktop".to_string(),
        label: "改名后的本机".to_string(),
        os_version: "win11".to_string(),
        app_version: "2.0.0".to_string(),
        identity_key: format!("key-{DEV1}"),
    };
    let again = device::register(&conn, "u1", DEV1, &reg, 3_000).unwrap();
    assert_eq!(again.device_id, DEV1);
    assert_eq!(again.label, "改名后的本机");
    assert_eq!(again.app_version, "2.0.0");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM identity_device", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "重复注册不许产生第二台设备");

    // 身份密钥变了 = 同一台设备号后面换了个客户端（重装、伪造、盗用号段）。
    // 这必须是冲突，而不是"覆盖旧的"：覆盖了就等于让新密钥继承旧设备的信任。
    let swapped = device::Registration {
        identity_key: "key-someone-else".to_string(),
        ..reg.clone()
    };
    let err =
        device::register(&conn, "u1", DEV1, &swapped, 3_100).expect_err("身份密钥变更必须冲突");
    assert!(matches!(err, IdError::Conflict(_)), "实际: {err}");
    let still = device::get(&conn, DEV1).unwrap().expect("行还在");
    assert_eq!(
        still.identity_key,
        format!("key-{DEV1}"),
        "冲突不许覆盖已信任的密钥"
    );
    assert_eq!(still.label, "改名后的本机", "失败的那次不许改任何字段");

    // 已撤销的设备不能靠重复注册复活。
    device::revoke(&conn, "u1", DEV1, 3_200).unwrap();
    let err = device::register(&conn, "u1", DEV1, &reg, 3_300).expect_err("已撤销设备不许复活");
    assert!(matches!(err, IdError::Forbidden(_)), "实际: {err}");
    assert!(!device::get(&conn, DEV1)
        .unwrap()
        .expect("行仍在")
        .is_active());
}

#[test]
fn device_input_validation_and_capacity() {
    let conn = db();
    let reg = device::Registration {
        platform: "desktop".to_string(),
        ..Default::default()
    };
    // 太短/太长的设备 id、非白名单平台、含控制字符的标签。
    assert!(matches!(
        device::register(&conn, "u1", "short", &reg, 1_000),
        Err(IdError::BadInput("device_id"))
    ));
    let bad = device::Registration {
        platform: "tosaster".to_string(),
        ..Default::default()
    };
    assert!(matches!(
        device::register(&conn, "u1", DEV1, &bad, 1_000),
        Err(IdError::BadInput("platform"))
    ));
    let ctrl = device::Registration {
        platform: "web".to_string(),
        label: "a\nb".to_string(),
        ..Default::default()
    };
    assert!(matches!(
        device::register(&conn, "u1", DEV1, &ctrl, 1_000),
        Err(IdError::BadInput(_))
    ));
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM identity_device", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0,
        "校验失败不能留下半台设备"
    );

    // 设备数上限：到顶之后新设备被拒，而不是把旧的挤掉。
    for i in 0..device::MAX_DEVICES_PER_USER {
        let id = format!("device-{i:06}");
        device::register(
            &conn,
            "cap",
            &id,
            &device::Registration {
                platform: "android".to_string(),
                ..Default::default()
            },
            2_000,
        )
        .unwrap_or_else(|e| panic!("设备 {i} 注册失败: {e}"));
    }
    let over = device::register(
        &conn,
        "cap",
        "device-overflow",
        &device::Registration {
            platform: "android".to_string(),
            ..Default::default()
        },
        3_000,
    );
    assert!(over.is_err(), "超过上限必须拒绝，实际: {over:?}");
    assert_eq!(
        device::active_count(&conn, "cap").unwrap(),
        device::MAX_DEVICES_PER_USER
    );
    assert_eq!(
        device::active_count(&conn, "u1").unwrap(),
        0,
        "别人的上限不该被别人占用"
    );
}

#[test]
fn logout_all_revokes_everything_for_the_account() {
    let conn = db();
    let (a1, r1) = login(&conn, "u1", DEV1, Audience::User);
    let (a2, r2) = login(&conn, "u1", DEV2, Audience::User);
    let (other, _) = login(&conn, "u2", "device-cccccccc", Audience::User);

    let n = session::logout_all(&conn, "u1", 2_000).unwrap();
    assert_eq!(n, 2, "两条会话都要撤掉");
    for (token, audience) in [(&a1, Audience::User), (&a2, Audience::User)] {
        assert!(session::authenticate(&conn, token, audience, 2_100).is_err());
    }
    for rt in [&r1, &r2] {
        let e = refresh::rotate(&conn, rt, 2_100).expect_err("会话全撤后刷新链也必须全废");
        assert!(matches!(e, IdError::Unauthorized(_)), "实际: {e}");
    }
    // 别的账号不受牵连。
    assert!(session::authenticate(&conn, &other, Audience::User, 2_100).is_ok());
    // 重复"退出所有设备"是幂等的。
    assert_eq!(session::logout_all(&conn, "u1", 2_200).unwrap(), 0);
}

#[test]
fn session_list_is_scoped_and_reports_last_seen() {
    let conn = db();
    let (access, _) = login(&conn, "u1", DEV1, Audience::User);
    login(&conn, "u2", DEV2, Audience::User);
    let ctx = session::authenticate(&conn, &access, Audience::User, 5_000).unwrap();
    session::touch(&conn, &ctx.sid, 5_100).unwrap();

    let mine: Vec<SessionRow> = session::list(&conn, "u1", 5_200).unwrap();
    assert_eq!(mine.len(), 1, "只能看到自己的会话");
    assert_eq!(mine[0].device_id, DEV1);
    assert_eq!(mine[0].last_seen, 5_100, "心跳要如实反映最近活跃");
    // 别人的心跳动不了我的行。
    session::touch(&conn, "no-such-sid", 9_999).unwrap();
    assert_eq!(
        session::list(&conn, "u1", 5_200).unwrap()[0].last_seen,
        5_100
    );
}
