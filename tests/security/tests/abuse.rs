//! G17-03（滥用面）：风控限速的"该拦的拦住、不该误伤的放行"。
//!
//! 用真实 `wxlike-risk` 引擎与 `wxlike-limits` 的默认阈值，时间由 `now_ms`
//! 注入，所以用例不 sleep、不抖动。断言一律**读配置里的实际阈值**，不硬编码
//! 数字：阈值调整时这些用例不需要改，但会继续保证"边界就在该在的地方"。
//!
//! 覆盖任务单点名的四类滥用：聊天 spam、好友枚举、验证码（登录）爆破，
//! 外加"正常批量同步不误伤"（对应 CALL-01 的 ICE trickle 突发）与
//! "动作之间互相隔离"（发消息刷爆不能把认领密钥一起废掉）。

use std::time::{SystemTime, UNIX_EPOCH};
use wxlike_risk::{Action, RiskEngine, Verdict};

const MIN: u64 = 60_000;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// ABUSE-001：聊天 spam。达到 deny 阈值后必须 `Deny`，且窗口滑过后恢复。
#[test]
fn chat_spam_is_denied_then_recovers() {
    let engine = RiskEngine::with_defaults();
    let cfg = engine.config().clone();
    let t0 = now();
    let mut last = Verdict::Allow;
    for _ in 0..cfg.send_deny_at {
        last = engine.record(Action::Send, "user:spammer", t0).unwrap();
    }
    assert!(
        matches!(last, Verdict::Deny { .. }),
        "连发 {} 条应当被 Deny，实际 {last:?}",
        cfg.send_deny_at
    );

    // 一个完整窗口之后恢复（滑动窗口不该让人永久封死）
    let later = t0 + 11_000;
    let verdict = engine.check(Action::Send, "user:spammer", later).unwrap();
    assert!(
        matches!(verdict, Verdict::Allow),
        "窗口过后应当恢复，实际 {verdict:?}"
    );
}

/// ABUSE-002：正常突发不误伤。低于 throttle 阈值的连发必须是 Allow。
#[test]
fn normal_burst_stays_allowed() {
    let engine = RiskEngine::with_defaults();
    let cfg = engine.config().clone();
    if cfg.send_throttle_at < 2 {
        return; // 配置把阈值压到 1 时这条用例没有意义
    }
    let t0 = now();
    for i in 0..(cfg.send_throttle_at - 1) {
        let v = engine
            .record(Action::Send, "user:normal", t0 + i as u64)
            .unwrap();
        assert!(
            matches!(v, Verdict::Allow),
            "第 {i} 条就限速了（阈值 {}）：正常聊天被误伤",
            cfg.send_throttle_at
        );
    }
}

/// ABUSE-003：好友枚举。批量加好友到阈值后必须限速（Throttle 非 captcha 动作）。
#[test]
fn friend_enumeration_is_throttled() {
    let engine = RiskEngine::with_defaults();
    let cfg = engine.config().clone();
    let t0 = now();
    let mut verdicts = Vec::new();
    for i in 0..cfg.friend_request_deny_at {
        verdicts.push(
            engine
                .record(Action::FriendRequest, "user:enumerator", t0 + i as u64)
                .unwrap(),
        );
    }
    let last = verdicts.last().unwrap();
    assert!(
        matches!(last, Verdict::Deny { .. } | Verdict::Throttle { .. }),
        "加好友 {} 次后应当限速，实际 {last:?}",
        cfg.friend_request_deny_at
    );
    let throttled = verdicts
        .iter()
        .position(|v| !matches!(v, Verdict::Allow))
        .expect("应当出现第一次限速");
    assert!(
        throttled >= cfg.friend_request_throttle_at - 1,
        "第一次限速出现在第 {throttled} 次，早于阈值 {}——正常用户会被误伤",
        cfg.friend_request_throttle_at
    );
}

/// ABUSE-004：验证码/口令爆破。登录失败达到 challenge 阈值必须要求人机校验，
/// 达到 deny 阈值必须拒绝。
#[test]
fn login_bruteforce_escalates_to_captcha_then_deny() {
    let engine = RiskEngine::with_defaults();
    let cfg = engine.config().clone();
    let t0 = now();
    let key = "ip:203.0.113.9";
    let mut saw_challenge = false;
    let mut last = Verdict::Allow;
    for i in 0..cfg.login_fail_deny_at {
        last = engine.record(Action::Login, key, t0 + i as u64).unwrap();
        if last.needs_captcha() {
            saw_challenge = true;
        }
    }
    assert!(saw_challenge, "撞库过程从未要求人机校验");
    assert!(
        matches!(last, Verdict::Deny { .. }),
        "撞库 {} 次后应当 Deny，实际 {last:?}",
        cfg.login_fail_deny_at
    );
}

/// ABUSE-005：通话信令的合法突发。ICE trickle 在 10 s 内会有几十个信令，
/// 配置必须容忍到 throttle 阈值，否则正常通话会被静音。
#[test]
fn call_signal_trickle_is_not_throttled() {
    let engine = RiskEngine::with_defaults();
    let cfg = engine.config().clone();
    let t0 = now();
    let n = cfg.call_signal_throttle_at.saturating_sub(1).min(40);
    for i in 0..n {
        let v = engine
            .record(Action::CallSignal, "call:c-1", t0 + i as u64 * 100)
            .unwrap();
        assert!(
            matches!(v, Verdict::Allow),
            "第 {i} 个 ICE 候选就被限速（阈值 {}）：正常通话会被打断",
            cfg.call_signal_throttle_at
        );
    }
}

/// ABUSE-006：动作之间互相隔离。刷消息不能把"认领预密钥"一起打掉，
/// 否则一条刷屏脚本就能让目标用户无法建立新会话。
#[test]
fn actions_do_not_share_buckets() {
    let engine = RiskEngine::with_defaults();
    let cfg = engine.config().clone();
    let t0 = now();
    let actor = "user:mixed";
    for _ in 0..cfg.send_deny_at {
        let _ = engine.record(Action::Send, actor, t0).unwrap();
    }
    let claim = engine.check(Action::ClaimKeys, actor, t0).unwrap();
    assert!(
        matches!(claim, Verdict::Allow),
        "刷消息把认领预密钥也限了：{claim:?}"
    );
}

/// ABUSE-007：限速状态可以显式清除（运营/客服解封路径存在，且真的生效）。
#[test]
fn clear_resets_the_window() {
    let engine = RiskEngine::with_defaults();
    let cfg = engine.config().clone();
    let t0 = now();
    for _ in 0..cfg.send_deny_at {
        let _ = engine.record(Action::Send, "user:unblock-me", t0).unwrap();
    }
    assert!(matches!(
        engine.check(Action::Send, "user:unblock-me", t0).unwrap(),
        Verdict::Deny { .. }
    ));
    engine.clear(Action::Send, "user:unblock-me").unwrap();
    let after = engine.check(Action::Send, "user:unblock-me", t0).unwrap();
    assert!(
        matches!(after, Verdict::Allow),
        "清除后应当立刻恢复：{after:?}"
    );
}

/// 时间窗常量在文档里写着"10 s / 1 h"，这里确认动作窗口确实不同——
/// 防止有人把所有动作塞进同一个窗口导致"加好友被 10 秒窗口限制"这类错配。
#[test]
fn action_windows_differ_by_action() {
    let engine = RiskEngine::with_defaults();
    let t0 = now();
    // 发消息是短窗（10 s），加好友是长窗（1 h）：同样的 30 次记录，
    // 一小时后发消息早已恢复，加好友仍在窗口内。
    for _ in 0..engine.config().send_deny_at {
        let _ = engine.record(Action::Send, "user:window", t0).unwrap();
    }
    for _ in 0..engine.config().friend_request_throttle_at {
        let _ = engine
            .record(Action::FriendRequest, "user:window", t0)
            .unwrap();
    }
    assert!(matches!(
        engine
            .check(Action::Send, "user:window", t0 + 11_000)
            .unwrap(),
        Verdict::Allow
    ));
    assert!(!matches!(
        engine
            .check(Action::FriendRequest, "user:window", t0 + 11_000)
            .unwrap(),
        Verdict::Allow
    ));
    let _ = MIN;
}
