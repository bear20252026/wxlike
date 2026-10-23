//! Redis GCRA 后端的真机测试：两个引擎共享一个 Redis，阈值必须是**全局**的。
//!
//! 默认 `cargo test` 不跑（`#[ignore]`，且需要 feature + 真实 Redis）：
//!
//! ```text
//! REDIS_URL=redis://127.0.0.1:6379 cargo test -p wxlike-risk --features redis \
//!     --test gcra_redis -- --ignored
//! ```
//!
//! 没有 `REDIS_URL` 时测试打印 skip 并返回——和 `wxlike-mq` 的 Redis 用例
//! 同一约定，普通 CI 不需要起服务。

use std::sync::{Arc, Barrier};
use std::thread;

use wxlike_risk::gcra::GcraStore;
use wxlike_risk::{Action, RiskConfig, RiskEngine};

const T0: u64 = 1_700_000_000_000;
const WINDOW_MS: u64 = 10_000;

fn redis_url() -> Option<String> {
    std::env::var("REDIS_URL")
        .ok()
        .or_else(|| std::env::var("WXLIKE_REDIS_URL").ok())
        .filter(|url| !url.trim().is_empty())
}

macro_rules! require_redis {
    () => {
        match redis_url() {
            Some(url) => url,
            None => {
                eprintln!("skip: set REDIS_URL to exercise the GCRA backend");
                return;
            }
        }
    };
}

fn config() -> RiskConfig {
    RiskConfig {
        send_throttle_at: 5,
        send_deny_at: 10,
        ..RiskConfig::default()
    }
}

fn engine(url: &str, namespace: &str) -> RiskEngine {
    let store = GcraStore::connect_with_namespace(url, namespace).unwrap();
    RiskEngine::with_store(config(), Arc::new(store))
}

fn namespace(test: &str) -> String {
    format!("wxlike:test:{test}:{}", std::process::id())
}

/// 两个引擎 = 两个服务进程的计数入口。并发打同一个 key 时，全局只放行
/// `throttle_at` 次——不是每个引擎各放行一次。
#[test]
#[ignore = "requires REDIS_URL; run with: cargo test -p wxlike-risk --features redis --test gcra_redis -- --ignored"]
fn two_engines_share_one_global_window() {
    let url = require_redis!();
    let ns = namespace("two-engines");
    let key = "bob:alice";
    let a = Arc::new(engine(&url, &ns));
    let b = Arc::new(engine(&url, &ns));
    a.clear(Action::Send, key).unwrap();

    // 10 个并发请求，两个引擎交替承担；全部用同一个 now，模拟"同一瞬间"。
    let threads = 10;
    let barrier = Arc::new(Barrier::new(threads + 1));
    let mut handles = Vec::new();
    for i in 0..threads {
        let engine = if i % 2 == 0 { a.clone() } else { b.clone() };
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            barrier.wait();
            engine.record(Action::Send, key, T0).unwrap()
        }));
    }
    barrier.wait();
    let verdicts: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let allowed = verdicts.iter().filter(|v| v.is_allowed()).count();
    assert_eq!(
        allowed,
        config().send_throttle_at,
        "global window must allow exactly throttle_at, got {verdicts:?}"
    );
    // 同一瞬间的第 6..9 次降速、第 10 次撞 deny_at——两个引擎加起来
    // 只允许一条全局时间线，不会各自从 0 开始数。
    let throttled = verdicts
        .iter()
        .filter(|v| matches!(v, wxlike_risk::Verdict::Throttle { .. }))
        .count();
    let denied = verdicts
        .iter()
        .filter(|v| matches!(v, wxlike_risk::Verdict::Deny { .. }))
        .count();
    assert_eq!(
        (throttled, denied),
        (4, 1),
        "expected 5 allow / 4 throttle / 1 deny: {verdicts:?}"
    );
    assert!(
        verdicts.iter().any(|v| matches!(
            v,
            wxlike_risk::Verdict::Throttle { retry_after_ms } if *retry_after_ms > 0
        )),
        "limited calls must carry a positive retry hint: {verdicts:?}"
    );

    // 两个引擎读到的计数相同：没有各算各的。被 Deny 的那次不推进计数。
    assert_eq!(a.count(Action::Send, key, T0).unwrap(), 9);
    assert_eq!(b.count(Action::Send, key, T0).unwrap(), 9);

    // 清空是全局的：另一个引擎立刻也放行。
    a.clear(Action::Send, key).unwrap();
    assert!(b.check(Action::Send, key, T0).unwrap().is_allowed());
    assert_eq!(b.count(Action::Send, key, T0).unwrap(), 0);
}

/// Deny 不能被降速档"饿死"：反复硬闯的降速尝试要计入拒绝档。
#[test]
#[ignore = "requires REDIS_URL"]
fn throttled_attempts_escalate_to_deny() {
    let url = require_redis!();
    let ns = namespace("deny-escalation");
    let key = "u1";
    let e = engine(&url, &ns);
    e.clear(Action::Send, key).unwrap();

    for _ in 0..config().send_throttle_at {
        assert!(e.record(Action::Send, key, T0).unwrap().is_allowed());
    }
    for _ in config().send_throttle_at..config().send_deny_at - 1 {
        assert!(matches!(
            e.record(Action::Send, key, T0).unwrap(),
            wxlike_risk::Verdict::Throttle { .. }
        ));
    }
    assert!(matches!(
        e.check(Action::Send, key, T0).unwrap(),
        wxlike_risk::Verdict::Deny { .. }
    ));
    match e.record(Action::Send, key, T0).unwrap() {
        wxlike_risk::Verdict::Deny { reason } => assert_eq!(reason, "send_rate"),
        other => panic!("expected deny, got {other:?}"),
    }

    // Deny 后按窗口自然消退。
    assert!(e
        .check(Action::Send, key, T0 + WINDOW_MS)
        .unwrap()
        .is_allowed());
    assert_eq!(e.count(Action::Send, key, T0 + WINDOW_MS).unwrap(), 0);

    e.clear(Action::Send, key).unwrap();
}

/// 只读的 check 不推进 TAT；record 才占配额；窗口滑过后恢复。
#[test]
#[ignore = "requires REDIS_URL"]
fn check_is_read_only_and_the_window_slides() {
    let url = require_redis!();
    let ns = namespace("check-readonly");
    let key = "u1";
    let e = engine(&url, &ns);
    e.clear(Action::Send, key).unwrap();

    for _ in 0..100 {
        assert!(e.check(Action::Send, key, T0).unwrap().is_allowed());
    }
    assert_eq!(
        e.count(Action::Send, key, T0).unwrap(),
        0,
        "check must not consume quota"
    );

    for _ in 0..config().send_throttle_at {
        assert!(e.record(Action::Send, key, T0).unwrap().is_allowed());
    }
    assert!(matches!(
        e.record(Action::Send, key, T0).unwrap(),
        wxlike_risk::Verdict::Throttle { .. }
    ));

    // 整个窗口过去后，桶排空。
    assert!(e
        .check(Action::Send, key, T0 + WINDOW_MS)
        .unwrap()
        .is_allowed());
    assert_eq!(e.count(Action::Send, key, T0 + WINDOW_MS).unwrap(), 0);

    // 键与动作隔离：别的 key 不受影响。
    assert!(e.check(Action::Send, "u2", T0).unwrap().is_allowed());
    assert!(e.check(Action::Login, key, T0).unwrap().is_allowed());

    e.clear(Action::Send, key).unwrap();
}
