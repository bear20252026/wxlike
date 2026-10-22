//! 风控引擎的行为契约：阈值、窗口滑动、成功清零、key 隔离。

use wxlike_risk::{Action, RiskConfig, RiskEngine, Verdict};

const T0: u64 = 1_700_000_000_000;

fn engine() -> RiskEngine {
    RiskEngine::new(RiskConfig {
        register_challenge_at: 3,
        register_deny_at: 5,
        login_fail_challenge_at: 2,
        login_fail_deny_at: 4,
        send_throttle_at: 2,
        send_deny_at: 4,
        report_throttle_at: 2,
        report_deny_at: 3,
        claim_keys_throttle_at: 2,
        claim_keys_deny_at: 4,
    })
}

#[test]
fn claim_keys_throttles_then_denies_without_challenge() {
    // AR-2/H-4：认领无 captcha 兑现面，只应有 Throttle/Deny 两态
    let e = engine();
    assert_eq!(
        e.check(Action::ClaimKeys, "bob:alice", T0).unwrap(),
        Verdict::Allow
    );
    assert_eq!(
        e.check(Action::ClaimKeys, "bob:alice", T0).unwrap(),
        Verdict::Throttle { retry_after_ms: 3_600_000 }
    );
    // 键隔离：别的 (caller, target) 不受影响
    assert_eq!(
        e.check(Action::ClaimKeys, "carol:alice", T0).unwrap(),
        Verdict::Allow
    );
    // 计入成功记录后撞 Deny（record 也计数）
    e.record(Action::ClaimKeys, "bob:alice", T0).unwrap();
    e.record(Action::ClaimKeys, "bob:alice", T0).unwrap();
    assert_eq!(
        e.check(Action::ClaimKeys, "bob:alice", T0).unwrap(),
        Verdict::Deny { reason: "claim_keys_rate".into() }
    );
}

#[test]
fn register_escalates_challenge_then_deny() {
    let e = engine();
    // 前两次放行（计数 0、1）
    assert_eq!(
        e.check(Action::Register, "1.2.3.4", T0).unwrap(),
        Verdict::Allow
    );
    assert!(e
        .record(Action::Register, "1.2.3.4", T0)
        .unwrap()
        .is_allowed());
    assert!(e
        .record(Action::Register, "1.2.3.4", T0)
        .unwrap()
        .is_allowed());

    // 第三次触发挑战
    let third = e.record(Action::Register, "1.2.3.4", T0).unwrap();
    assert!(third.needs_captcha(), "{third:?}");

    // 第五次直接拒绝
    e.record(Action::Register, "1.2.3.4", T0).unwrap();
    match e.record(Action::Register, "1.2.3.4", T0).unwrap() {
        Verdict::Deny { reason } => assert_eq!(reason, "register_rate"),
        other => panic!("expected deny, got {other:?}"),
    }
}

#[test]
fn window_slides_so_old_attempts_stop_counting() {
    let e = engine();
    for _ in 0..4 {
        e.record(Action::Register, "ip", T0).unwrap();
    }
    assert!(!e.check(Action::Register, "ip", T0).unwrap().is_allowed());

    // 一小时后窗口清空（窗口长度 1h，恰好等于边界的记录已过期）
    let later = T0 + 60 * 60 * 1000;
    assert_eq!(
        e.check(Action::Register, "ip", later).unwrap(),
        Verdict::Allow
    );
    assert_eq!(e.count(Action::Register, "ip", later).unwrap(), 0);
}

#[test]
fn login_failures_clear_after_success() {
    let e = engine();
    e.record(Action::Login, "alice", T0).unwrap();
    assert!(e
        .record(Action::Login, "alice", T0)
        .unwrap()
        .needs_captcha());

    e.clear(Action::Login, "alice").unwrap();
    assert_eq!(e.check(Action::Login, "alice", T0).unwrap(), Verdict::Allow);

    // 清空后重新累计到拒绝阈值
    for _ in 0..4 {
        e.record(Action::Login, "alice", T0).unwrap();
    }
    assert!(matches!(
        e.check(Action::Login, "alice", T0).unwrap(),
        Verdict::Deny { .. }
    ));
}

#[test]
fn send_throttles_then_denies_with_retry_hint() {
    let e = engine();
    // 第 1 条放行，第 2 条起降速（throttle_at = 2）
    assert!(e.record(Action::Send, "u1", T0).unwrap().is_allowed());

    match e.record(Action::Send, "u1", T0).unwrap() {
        Verdict::Throttle { retry_after_ms } => {
            assert!(
                retry_after_ms > 0 && retry_after_ms <= 10_000,
                "{retry_after_ms}"
            );
        }
        other => panic!("expected throttle, got {other:?}"),
    }

    // 第 3 条仍降速，第 4 条直接拒绝（deny_at = 4）
    assert!(matches!(
        e.record(Action::Send, "u1", T0).unwrap(),
        Verdict::Throttle { .. }
    ));
    assert!(matches!(
        e.record(Action::Send, "u1", T0).unwrap(),
        Verdict::Deny { .. }
    ));
}

#[test]
fn report_flood_is_denied() {
    let e = engine();
    e.record(Action::Report, "u1", T0).unwrap();
    assert!(matches!(
        e.record(Action::Report, "u1", T0).unwrap(),
        Verdict::Throttle { .. }
    ));
    assert!(matches!(
        e.record(Action::Report, "u1", T0).unwrap(),
        Verdict::Deny { .. }
    ));
}

#[test]
fn keys_and_actions_are_isolated() {
    let e = engine();
    for _ in 0..4 {
        e.record(Action::Send, "u1", T0).unwrap();
    }
    // 另一个用户不受影响
    assert!(e.check(Action::Send, "u2", T0).unwrap().is_allowed());
    // 同一用户的其他动作不受影响
    assert!(e.check(Action::Login, "u1", T0).unwrap().is_allowed());
    // 注册 key 命名空间独立（同一个字符串不串味）
    assert!(e.check(Action::Register, "u1", T0).unwrap().is_allowed());
}

#[test]
fn verdict_serializes_with_a_discriminant() {
    let v = Verdict::Throttle {
        retry_after_ms: 900,
    };
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(json["verdict"], "throttle");
    assert_eq!(json["retry_after_ms"], 900);

    let c = Verdict::Challenge {
        reason: "register_rate".into(),
    };
    assert_eq!(serde_json::to_value(&c).unwrap()["verdict"], "challenge");
}
