//! G02-09 管理端设备撤销：`POST /api/admin/identity/devices/{id}/revoke`。
//!
//! 与 G02-01 用户自助 `device::revoke` 的区别只在**操作者**：这里是管理
//! audience（`authenticate_operator`），不需要设备归属者本人，也不要求 step-up
//! （管理面的两阶段确认在 adminx 侧，域侧只认 admin 令牌）。撤销规则本体复用
//! `device::revoke`：会话 + 刷新链 + step-up 同一事务作废，幂等命中不产生二次
//! 效果。设备密钥作废是进程内效果，经 [`GovernanceEvent::DeviceRevoked`] 交平台。

use rusqlite::Connection;

use super::{
    append_audit, authenticate_operator, check_actor, check_reason, check_target, reference,
    AuditDraft, GovernCommand, GovernEffect, GovernanceEvent, PlatformGovernancePort,
};
use crate::{device, IdError, Result};

/// 管理端撤销设备。`cmd.target` 是 device_id。
pub fn revoke_device(
    conn: &Connection,
    operator_token: &str,
    cmd: &GovernCommand,
    port: &dyn PlatformGovernancePort,
    now: i64,
) -> Result<GovernEffect> {
    let device_id = check_target(&cmd.target)?;
    let reason = check_reason(&cmd.reason)?;
    let actor = check_actor(&cmd.actor)?;
    authenticate_operator(conn, operator_token, now)?;

    let row = device::get(conn, &device_id)?.ok_or(IdError::NotFound("device"))?;
    let user_id = row.user_id.clone();
    let already = !row.is_active();
    let before = serde_json::json!({
        "device_id": device_id,
        "user_id": user_id,
        "state": if already { "revoked" } else { "active" },
    });

    // 复用域撤销：幂等命中返回 sessions=0；未撤销则同事务作废会话/刷新链/step-up。
    let outcome = device::revoke(conn, &user_id, &device_id, now)?;
    let sessions_revoked = outcome.sessions as u64;
    let after = serde_json::json!({
        "device_id": device_id,
        "user_id": user_id,
        "state": "revoked",
    });

    let seq = append_audit(
        conn,
        &AuditDraft {
            action: "device.revoke",
            target: &device_id,
            actor: &actor,
            reason: &reason,
            trace: &cmd.trace,
            before: &before,
            after: &after,
            sessions_revoked,
            already,
            at: now,
        },
    )?;

    let mut events = vec![GovernanceEvent::DeviceRevoked {
        user_id: user_id.clone(),
        device_id: device_id.clone(),
    }];
    if sessions_revoked > 0 {
        events.push(GovernanceEvent::SessionsRevoked {
            user_id: user_id.clone(),
            count: sessions_revoked,
        });
    }
    events.push(GovernanceEvent::CacheClear {
        scope: "device".to_string(),
        key: device_id.clone(),
    });
    events.push(GovernanceEvent::CacheClear {
        scope: "user".to_string(),
        key: user_id.clone(),
    });
    // 设备撤销不改用户治理标志，无需镜像 users；事件交平台作废设备密钥/断 WS。
    port.dispatch(&events);
    Ok(GovernEffect {
        reference: reference("device.revoke", seq),
        before,
        after,
        sessions_revoked,
        already,
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;
    use crate::{device::Registration, Audience};

    struct NoopPort;
    impl PlatformGovernancePort for NoopPort {
        fn mirror_user_state(
            &self,
            _user_id: &str,
            _suspended: bool,
            _disabled: bool,
            _until_ms: Option<i64>,
        ) -> Result<()> {
            Ok(())
        }
        fn dispatch(&self, _events: &[GovernanceEvent]) {}
    }

    fn admin_token(conn: &Connection) -> String {
        let reg = Registration {
            platform: "desktop".into(),
            ..Default::default()
        };
        device::register(conn, "ops", "dev-ops00001", &reg, 1_000).unwrap();
        let grant =
            crate::session::open_default(conn, "ops", "dev-ops00001", Audience::Admin, "", 1_000)
                .unwrap();
        grant.access_token
    }

    fn cmd(target: &str) -> GovernCommand {
        GovernCommand {
            target: target.into(),
            reason: "设备丢失".into(),
            until_ms: None,
            actor: "ops".into(),
            trace: "t1".into(),
            verb: None,
            new_owner: None,
        }
    }

    #[test]
    fn revoke_kills_sessions_and_is_idempotent() {
        let conn = db();
        let token = admin_token(&conn);
        let reg = Registration {
            platform: "android".into(),
            ..Default::default()
        };
        device::register(&conn, "u1", "dev-aaaa1111", &reg, 1_000).unwrap();
        let grant =
            crate::session::open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_000)
                .unwrap();

        let effect = revoke_device(&conn, &token, &cmd("dev-aaaa1111"), &NoopPort, 1_100).unwrap();
        assert_eq!(effect.sessions_revoked, 1);
        assert!(!effect.already);
        assert!(
            crate::session::authenticate(&conn, &grant.access_token, Audience::User, 1_200)
                .is_err()
        );

        let again = revoke_device(&conn, &token, &cmd("dev-aaaa1111"), &NoopPort, 1_300).unwrap();
        assert!(again.already);
        assert_eq!(again.sessions_revoked, 0);
    }

    #[test]
    fn user_token_cannot_revoke_device() {
        let conn = db();
        let reg = Registration {
            platform: "android".into(),
            ..Default::default()
        };
        device::register(&conn, "u1", "dev-aaaa1111", &reg, 1_000).unwrap();
        let user =
            crate::session::open_default(&conn, "u1", "dev-aaaa1111", Audience::User, "", 1_000)
                .unwrap();
        let err = revoke_device(
            &conn,
            &user.access_token,
            &cmd("dev-aaaa1111"),
            &NoopPort,
            1_100,
        )
        .unwrap_err();
        assert!(matches!(err, IdError::Unauthorized(_)));
        // 设备仍然活跃（越权没有产生效果）。
        assert!(device::get(&conn, "dev-aaaa1111")
            .unwrap()
            .unwrap()
            .is_active());
    }

    #[test]
    fn unknown_device_is_not_found() {
        let conn = db();
        let token = admin_token(&conn);
        let err = revoke_device(&conn, &token, &cmd("dev-missing0"), &NoopPort, 1_100).unwrap_err();
        assert!(matches!(err, IdError::NotFound(_)));
    }
}
