//! G02-09 账户治理的验收面（真实 SQLite，非内存仓储）。
//!
//! 逐件钉住卡面验收：
//!
//! 1. **封禁后该用户 API 立即失效、sessions_revoked 非零**；
//! 2. **解封恢复且留痕**（可逆）；
//! 3. **越权管理操作失败封闭**（普通用户令牌进不来）；
//! 4. **重复提交幂等**（already=true，不产生二次效果）；
//! 5. **临时封禁到期自动恢复**（惰性判定，不等后台清扫）；
//! 6. **管理员视图不含消息正文**（白名单投影）。
//!
//! 平台联动（users 表镜像、进程内事件）经 [`RecordingPort`] 记录断言——它是
//! 任务单允许的并行开发 fixture（上游 G00 集成面未合入），最终验收换真实端口。

use std::sync::Mutex;

use wxlike_identity::governance::{
    self, GovernanceEvent, GovernCommand, PlatformGovernancePort, UserMetadataPort,
};
use wxlike_identity::{session, Audience, IdError};

mod common;
use common::{add_device, db, login};

const DEV1: &str = "device-aaaaaaaa";
const DEV2: &str = "device-bbbbbbbb";

/// 记录平台联动调用的 fixture（镜像 + 事件）。
#[derive(Default)]
struct RecordingPort {
    mirrors: Mutex<Vec<(String, bool, bool, Option<i64>)>>,
    events: Mutex<Vec<GovernanceEvent>>,
}

impl PlatformGovernancePort for RecordingPort {
    fn mirror_user_state(
        &self,
        user_id: &str,
        suspended: bool,
        disabled: bool,
        until_ms: Option<i64>,
    ) -> wxlike_identity::Result<()> {
        self.mirrors
            .lock()
            .unwrap()
            .push((user_id.to_string(), suspended, disabled, until_ms));
        Ok(())
    }
    fn dispatch(&self, events: &[GovernanceEvent]) {
        self.events.lock().unwrap().extend(events.iter().cloned());
    }
}

/// 造一个 admin 操作者并返回其访问令牌。
fn admin_token(conn: &rusqlite::Connection) -> String {
    let (access, _) = login(conn, "ops", "device-ops00001", Audience::Admin);
    access
}

fn cmd(target: &str) -> GovernCommand {
    GovernCommand {
        target: target.into(),
        reason: "举报成立：冒充客服".into(),
        until_ms: None,
        actor: "ops".into(),
        trace: "trace-x".into(),
        verb: None,
        new_owner: None,
    }
}

/// 给用户开两个会话（两台设备），返回两把访问令牌。
fn two_sessions(conn: &rusqlite::Connection, user: &str) -> (String, String) {
    let (a, _) = login(conn, user, DEV1, Audience::User);
    add_device(conn, user, DEV2);
    let b = session::open_default(conn, user, DEV2, Audience::User, "", 1_000)
        .unwrap()
        .access_token;
    (a, b)
}

#[test]
fn suspend_revokes_all_sessions_and_blocks_access() {
    let conn = db();
    let token = admin_token(&conn);
    let (a, b) = two_sessions(&conn, "u1");
    let port = RecordingPort::default();

    let effect = governance::suspend(&conn, &token, &cmd("u1"), &port, 1_100).unwrap();
    assert_eq!(effect.sessions_revoked, 2, "封禁必须连带踢掉全部会话");
    assert!(!effect.already);
    assert_eq!(effect.after["state"], "suspended");
    assert_eq!(effect.before["state"], "active");

    // 两把令牌立即失效（API 面）。
    assert!(session::authenticate(&conn, &a, Audience::User, 1_200).is_err());
    assert!(session::authenticate(&conn, &b, Audience::User, 1_200).is_err());
    // 登录闸门也挡住（重新登录不行）。
    assert!(matches!(
        governance::ensure_user_active(&conn, "u1", 1_200),
        Err(IdError::Forbidden(_))
    ));

    // 平台镜像收到 suspended=true；事件含 SessionsRevoked。
    let mirrors = port.mirrors.lock().unwrap();
    assert!(mirrors.iter().any(|(u, s, _, _)| u == "u1" && *s));
    let events = port.events.lock().unwrap();
    assert!(events
        .iter()
        .any(|e| matches!(e, GovernanceEvent::SessionsRevoked { count: 2, .. })));
}

#[test]
fn unsuspend_restores_access_and_leaves_trail() {
    let conn = db();
    let token = admin_token(&conn);
    let (a, _) = two_sessions(&conn, "u1");
    let port = RecordingPort::default();

    governance::suspend(&conn, &token, &cmd("u1"), &port, 1_100).unwrap();
    let mut undo = cmd("u1");
    undo.reason = "误封：证据不足".into();
    let effect = governance::unsuspend(&conn, &token, &undo, &port, 1_200).unwrap();
    assert!(!effect.already);
    assert_eq!(effect.after["state"], "active");
    assert!(governance::ensure_user_active(&conn, "u1", 1_300).is_ok());

    // 留痕：suspend 与 unsuspend 两条审计都在，理由可读。
    let rows: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT action, reason FROM identity_governance_audit \
                 WHERE target = 'u1' ORDER BY seq",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
    };
    let actions: Vec<&str> = rows.iter().map(|(a, _)| a.as_str()).collect();
    assert!(actions.contains(&"suspend") && actions.contains(&"unsuspend"));
    assert!(rows.iter().any(|(_, r)| r.contains("误封")));
    // 旧令牌不会因解封复活（会话已删，需重新登录）。
    assert!(session::authenticate(&conn, &a, Audience::User, 1_300).is_err());
}

#[test]
fn user_token_cannot_suspend_and_effect_is_closed() {
    let conn = db();
    let (user_token, _) = login(&conn, "u1", DEV1, Audience::User);
    let port = RecordingPort::default();
    let err = governance::suspend(&conn, &user_token, &cmd("u1"), &port, 1_100).unwrap_err();
    assert!(matches!(err, IdError::Unauthorized(_)));
    // 越权没有产生任何效果：状态仍 active，无镜像、无会话撤销。
    assert_eq!(
        governance::user_state(&conn, "u1", 1_200).unwrap(),
        governance::UserState::Active
    );
    assert!(port.mirrors.lock().unwrap().is_empty());
}

#[test]
fn repeated_suspend_is_idempotent() {
    let conn = db();
    let token = admin_token(&conn);
    two_sessions(&conn, "u1");
    let port = RecordingPort::default();

    let first = governance::suspend(&conn, &token, &cmd("u1"), &port, 1_100).unwrap();
    assert_eq!(first.sessions_revoked, 2);
    let again = governance::suspend(&conn, &token, &cmd("u1"), &port, 1_200).unwrap();
    assert!(again.already, "重复封禁必须幂等命中");
    assert_eq!(again.sessions_revoked, 0, "幂等命中不再二次撤销会话");
    // 状态没有二次变化。
    assert_eq!(
        governance::user_state(&conn, "u1", 1_300).unwrap(),
        governance::UserState::Suspended
    );
}

#[test]
fn extending_suspension_is_a_new_action() {
    let conn = db();
    let token = admin_token(&conn);
    two_sessions(&conn, "u1");
    let port = RecordingPort::default();

    let mut c = cmd("u1");
    c.until_ms = Some(5_000);
    governance::suspend(&conn, &token, &c, &port, 1_100).unwrap();
    // 改期（延长）= 新处置：already=false。
    let mut longer = cmd("u1");
    longer.until_ms = Some(9_000);
    let effect = governance::suspend(&conn, &token, &longer, &port, 1_200).unwrap();
    assert!(!effect.already);
    assert_eq!(effect.after["suspended_until_ms"], 9_000);
}

#[test]
fn temporary_suspension_expires_lazily() {
    let conn = db();
    let token = admin_token(&conn);
    let (a, _) = two_sessions(&conn, "u1");
    let port = RecordingPort::default();

    let mut c = cmd("u1");
    c.until_ms = Some(2_000);
    governance::suspend(&conn, &token, &c, &port, 1_100).unwrap();
    // 期内：封禁生效。
    assert!(matches!(
        governance::ensure_user_active(&conn, "u1", 1_500),
        Err(IdError::Forbidden(_))
    ));
    // 到期：惰性判定立即放行（不等后台清扫）。
    assert!(governance::ensure_user_active(&conn, "u1", 2_000).is_ok());
    assert_eq!(
        governance::user_state(&conn, "u1", 2_000).unwrap(),
        governance::UserState::Expired
    );
    // 旧会话不会因到期复活（已删，需重新登录）。
    assert!(session::authenticate(&conn, &a, Audience::User, 2_100).is_err());

    // 物理结算：落 suspend.expire 审计并镜像平台。
    let n = governance::settle_expired(&conn, &port, 2_500).unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        governance::user_state(&conn, "u1", 2_600).unwrap(),
        governance::UserState::Active
    );
    let expired_audits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM identity_governance_audit WHERE action = 'suspend.expire'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(expired_audits, 1);
}

#[test]
fn suspend_requires_future_until_and_reason() {
    let conn = db();
    let token = admin_token(&conn);
    let port = RecordingPort::default();
    let mut past = cmd("u1");
    past.until_ms = Some(500); // 早于 now=1_100
    assert!(matches!(
        governance::suspend(&conn, &token, &past, &port, 1_100),
        Err(IdError::BadInput(_))
    ));
    let mut blank = cmd("u1");
    blank.reason = "   ".into();
    assert!(matches!(
        governance::suspend(&conn, &token, &blank, &port, 1_100),
        Err(IdError::BadInput(_))
    ));
}

#[test]
fn disable_is_independent_of_suspension() {
    let conn = db();
    let token = admin_token(&conn);
    two_sessions(&conn, "u1");
    let port = RecordingPort::default();

    let effect = governance::set_disabled(&conn, &token, &cmd("u1"), true, &port, 1_100).unwrap();
    assert_eq!(effect.sessions_revoked, 2);
    assert_eq!(effect.after["state"], "disabled");
    // 解封不清停用（独立处置）。
    governance::unsuspend(&conn, &token, &cmd("u1"), &port, 1_200).unwrap();
    assert_eq!(
        governance::user_state(&conn, "u1", 1_300).unwrap(),
        governance::UserState::Disabled
    );
    // 启用恢复。
    governance::set_disabled(&conn, &token, &cmd("u1"), false, &port, 1_400).unwrap();
    assert!(governance::ensure_user_active(&conn, "u1", 1_500).is_ok());
}

/// 跨域计数端口 fixture（群数/举报数由平台装配注入）。
struct FixedCounts;
impl UserMetadataPort for FixedCounts {
    fn group_count(&self, _user_id: &str) -> wxlike_identity::Result<i64> {
        Ok(4)
    }
    fn report_count(&self, _user_id: &str) -> wxlike_identity::Result<i64> {
        Ok(1)
    }
}

#[test]
fn admin_view_is_metadata_only_and_reflects_state() {
    let conn = db();
    let token = admin_token(&conn);
    add_device(&conn, "u1", DEV1);
    let port = RecordingPort::default();

    let mut c = cmd("u1");
    c.until_ms = Some(9_000);
    governance::suspend(&conn, &token, &c, &port, 1_100).unwrap();
    governance::add_label(&conn, &token, "u1", "高风险", 1_150).unwrap();
    governance::add_label(&conn, &token, "u1", "高风险", 1_160).unwrap(); // 幂等

    let view = governance::user_view(&conn, &token, "u1", &FixedCounts, 1_200).unwrap();
    assert_eq!(view.user_id, "u1");
    assert_eq!(view.state, "suspended");
    assert_eq!(view.suspended_until_ms, Some(9_000));
    assert_eq!(view.device_count, 1);
    assert_eq!(view.group_count, 4);
    assert_eq!(view.report_count, 1);
    assert_eq!(view.labels, vec!["高风险".to_string()]);

    // 序列化后只有白名单字段，绝无正文。
    let text = serde_json::to_string(&view).unwrap();
    for leak in ["message", "ciphertext", "session_key", "content", "body"] {
        assert!(!text.contains(leak), "视图泄漏正文相关字段 {leak}: {text}");
    }
    let value = serde_json::to_value(&view).unwrap();
    let obj = value.as_object().unwrap();
    for key in obj.keys() {
        assert!(
            governance::METADATA_FIELDS.contains(&key.as_str()),
            "白名单外字段 {key}"
        );
    }
}

#[test]
fn device_revoke_via_governance_kills_device_sessions() {
    let conn = db();
    let token = admin_token(&conn);
    let (a, _) = login(&conn, "u1", DEV1, Audience::User);
    let port = RecordingPort::default();

    let mut c = cmd(DEV1);
    c.reason = "设备丢失".into();
    let effect = governance::revoke_device(&conn, &token, &c, &port, 1_100).unwrap();
    assert_eq!(effect.sessions_revoked, 1);
    assert!(!effect.already);
    assert!(session::authenticate(&conn, &a, Audience::User, 1_200).is_err());
    // 幂等。
    let again = governance::revoke_device(&conn, &token, &c, &port, 1_300).unwrap();
    assert!(again.already);
    assert_eq!(again.sessions_revoked, 0);
}
