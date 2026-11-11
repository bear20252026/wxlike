//! 扫码登录、验证码登录与协议同意（#115 / #112 的失败面与越权面）。
//!
//! 三组共同的主张是**失败封闭**：任何"看起来像授权"的中间态（已扫码、已绑定、
//! 已下发验证码）都不等于会话；越权尝试一律拿不到比"不行"更多的信息。

use wxlike_identity::{
    consent, device, login_challenge as lc, qr_login, session, step_up, throttle, Audience, IdError,
};

mod common;
use common::db;

const DEV: &str = "device-qr0001";
const PEPPER: &[u8] = b"test-pepper-value";
const VERIFIER: &str = "verifier-abcdefghijklmnopqrstuvwxyzABCDEFGHIJ0123456789";

/// 走完"手机已批准"之前的全部步骤，返回 (challenge id, secret)。
///
/// 桌面在扫码之前**没有账号**，所以这里故意不注册设备行：设备归属是兑换时才
/// 对齐到批准者的（见 `qr_login::adopt_device`）。提前用别的账号注册同一台设备，
/// 属于下面 `another_accounts_device_id_is_not_hijacked` 那条攻击用例。
fn to_approved(conn: &rusqlite::Connection, scanner: &str) -> (String, String) {
    let t = qr_login::create(conn, 1_000).expect("create");
    qr_login::bind(
        conn, &t.id, VERIFIER, DEV, "desktop", "本机", "1.2.3.4", 1_000,
    )
    .expect("bind");
    qr_login::scan(conn, &t.secret, scanner, 1_100).expect("scan");
    qr_login::approve(conn, &t.id, scanner, 1_200).expect("approve");
    (t.id, t.secret)
}

/// 批准之后设备必须以批准者的名义存在，并且会话挂在这台设备上。
#[test]
fn exchange_adopts_the_device_for_the_account_that_approved_it() {
    let conn = db();
    let (id, _) = to_approved(&conn, "phone-1");
    assert!(
        device::get(&conn, DEV).unwrap().is_none(),
        "批准之前不该有设备行"
    );

    let grant = qr_login::exchange(&conn, &id, VERIFIER, Audience::User, "1.2.3.4", 1_400)
        .expect("exchange");
    let row = device::get(&conn, DEV).unwrap().expect("兑换要登记设备");
    assert_eq!(row.user_id, "phone-1", "设备归属必须是批准这次登录的账号");
    assert_eq!(row.platform, "desktop", "bind 上报的平台要如实带过去");
    let ctx = session::authenticate(&conn, &grant.access_token, Audience::User, 1_500).unwrap();
    assert_eq!(
        (ctx.user_id.as_str(), ctx.device_id.as_str()),
        ("phone-1", DEV)
    );
}

/// 攻击路径：攻击者先拿同一个 device_id 注册成自己的设备，等受害者扫码批准。
/// 绝不能因此把受害者的会话挂到攻击者的设备行上。
#[test]
fn another_accounts_device_id_is_not_hijacked() {
    let conn = db();
    device::register(
        &conn,
        "attacker",
        DEV,
        &device::Registration {
            platform: "desktop".to_string(),
            ..Default::default()
        },
        1_000,
    )
    .expect("attacker registers the id first");

    let (id, _) = to_approved(&conn, "victim");
    let err = qr_login::exchange(&conn, &id, VERIFIER, Audience::User, "ip", 1_400)
        .expect_err("已属于别人的 device_id 不能被抢用");
    assert!(matches!(err, IdError::NotFound(_)), "实际: {err}");

    // 归属没有被动过，也没有产生任何会话。
    assert_eq!(
        device::get(&conn, DEV).unwrap().unwrap().user_id,
        "attacker"
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    // 挑战已被消费：这条路不能重试到成功。
    assert!(matches!(
        qr_login::exchange(&conn, &id, VERIFIER, Audience::User, "ip", 1_500),
        Err(IdError::Conflict(_))
    ));
}

#[test]
fn qr_happy_path_issues_exactly_one_session() {
    let conn = db();
    let (id, _) = to_approved(&conn, "phone-1");
    assert_eq!(
        qr_login::poll(&conn, &id, VERIFIER, 1_300).unwrap(),
        qr_login::PollState::Approved
    );

    let grant = qr_login::exchange(&conn, &id, VERIFIER, Audience::User, "1.2.3.4", 1_400)
        .expect("exchange");
    let ctx = session::authenticate(&conn, &grant.access_token, Audience::User, 1_500).unwrap();
    assert_eq!(
        ctx.user_id, "phone-1",
        "会话属于批准它的那个账号，不是扫码提示里的人"
    );

    // 兑换之后码就死了。
    assert!(matches!(
        qr_login::exchange(&conn, &id, VERIFIER, Audience::User, "ip", 1_600),
        Err(IdError::Conflict(_))
    ));
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1,
        "一次批准只许产生一条会话"
    );
}

#[test]
fn qr_intermediate_states_are_not_a_session() {
    let conn = db();
    let t = qr_login::create(&conn, 1_000).unwrap();
    device::register(
        &conn,
        "u",
        DEV,
        &device::Registration {
            platform: "desktop".to_string(),
            ..Default::default()
        },
        1_000,
    )
    .unwrap();

    // 没绑定设备的码：扫不了。
    assert!(matches!(
        qr_login::scan(&conn, &t.secret, "phone-1", 1_100),
        Err(IdError::Conflict(_))
    ));
    qr_login::bind(&conn, &t.id, VERIFIER, DEV, "desktop", "本机", "ip", 1_000).unwrap();

    // 已绑定：仍然扫不了批准、换不了会话。
    assert!(matches!(
        qr_login::approve(&conn, &t.id, "phone-1", 1_100),
        Err(IdError::Conflict(_))
    ));
    assert!(matches!(
        qr_login::exchange(&conn, &t.id, VERIFIER, Audience::User, "ip", 1_100),
        Err(IdError::Conflict(_))
    ));

    // 已扫码、未批准：这是最关键的一格——"手机看了一眼"不等于用户同意。
    qr_login::scan(&conn, &t.secret, "phone-1", 1_200).unwrap();
    let err = qr_login::exchange(&conn, &t.id, VERIFIER, Audience::User, "ip", 1_300)
        .expect_err("scanned 不等于 approved");
    assert!(matches!(err, IdError::Conflict(_)), "实际: {err}");
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn qr_authorization_boundaries() {
    let conn = db();
    let (id, _) = to_approved(&conn, "phone-1");

    // 没扫码的账号想替别人批准。
    assert!(matches!(
        qr_login::approve(&conn, &id, "phone-2", 1_300),
        Err(IdError::Conflict(_)) | Err(IdError::Forbidden(_))
    ));
    // 拿不到 verifier 的人：拍二维码的人只有 secret。
    let err = qr_login::exchange(
        &conn,
        &id,
        "wrong-verifier-value-padded-to-the-min-length-xx",
        Audience::User,
        "ip",
        1_300,
    )
    .expect_err("verifier 不对就不能兑换");
    assert!(
        matches!(err, IdError::Unauthorized(_) | IdError::BadInput(_)),
        "实际: {err}"
    );
    let err = qr_login::poll(&conn, &id, "short", 1_300).expect_err("过短的 verifier 要拒");
    assert!(matches!(err, IdError::BadInput(_)), "实际: {err}");

    // 兑换成功之前，会话一条也没有；批准者之外的人拿不到任何 sid。
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn qr_expiry_and_refresh_rotate_the_challenge() {
    let conn = db();
    let t = qr_login::create(&conn, 1_000).unwrap();
    device::register(
        &conn,
        "u",
        DEV,
        &device::Registration {
            platform: "desktop".to_string(),
            ..Default::default()
        },
        1_000,
    )
    .unwrap();
    qr_login::bind(&conn, &t.id, VERIFIER, DEV, "desktop", "本机", "ip", 1_000).unwrap();

    // 到期之前的正常换码：旧码立刻作废，新码继承同一份设备与 PKCE 绑定。
    let nt = qr_login::refresh(&conn, &t.id, VERIFIER, 1_500).expect("refresh");
    assert_ne!(nt.id, t.id);
    let e = qr_login::scan(&conn, &t.secret, "phone-1", 1_600)
        .err()
        .unwrap_or_else(|| panic!("旧 secret 不能还能扫"));
    assert!(
        matches!(e, IdError::Expired | IdError::Conflict(_)),
        "实际: {e}"
    );
    qr_login::scan(&conn, &nt.secret, "phone-1", 1_600).expect("新码可扫");
    // verifier 不对的人换不了码。
    assert!(matches!(
        qr_login::refresh(
            &conn,
            &nt.id,
            "nope-nope-nope-nope-nope-nope-nope-nope-nope-x",
            1_700
        ),
        Err(IdError::Unauthorized(_)) | Err(IdError::BadInput(_))
    ));

    // 真正过期：轮询给 Expired（好让桌面去换码），扫码直接 Expired，
    // 而且 expired 之后 refresh 也拒——过期的码不能被"续命"。
    let late = nt.expires_at + 1;
    assert_eq!(
        qr_login::poll(&conn, &nt.id, VERIFIER, late).unwrap(),
        qr_login::PollState::Expired,
        "过期轮询返回 Expired 而不是报错"
    );
    assert!(matches!(
        qr_login::scan(&conn, &nt.secret, "phone-2", late),
        Err(IdError::Expired)
    ));
    assert!(matches!(
        qr_login::refresh(&conn, &nt.id, VERIFIER, late),
        Err(IdError::Conflict(_)) | Err(IdError::Expired)
    ));
}

#[test]
fn challenge_code_is_single_use_and_attempt_limited() {
    let conn = db();
    let issued = lc::issue(
        &conn,
        "13800000000",
        "+86",
        "login",
        DEV,
        "9.9.9.1",
        PEPPER,
        lc::DEFAULT_LIMITS,
        1_000,
    )
    .expect("issue");
    assert_eq!(issued.code.len(), lc::CODE_DIGITS as usize);

    // 错码：达到上限后一律拒绝，而且不给"差一位"这类线索。
    let wrong = if issued.code == "000000" {
        "111111"
    } else {
        "000000"
    };
    for i in 0..lc::DEFAULT_LIMITS.max_attempts {
        let e =
            lc::verify(&conn, &issued.id, wrong, PEPPER, 1_100 + i * 10).expect_err("错码必须失败");
        assert!(
            matches!(e, IdError::Unauthorized(_) | IdError::RateLimited { .. }),
            "错码的答复不该更详细: {e}"
        );
    }
    // 上限之后即使给对码也不行。答复必须是"不行"这一类，而且不许区分
    // "码错了"和"次数用完了"——那等于告诉探测者还剩几次机会。
    let e = lc::verify(&conn, &issued.id, &issued.code, PEPPER, 1_200)
        .expect_err("尝试用满后正确码也必须被挡");
    assert!(
        matches!(e, IdError::Unauthorized(_) | IdError::RateLimited { .. }),
        "实际: {e}"
    );

    // 另一个号码的挑战不受影响。
    let other = lc::issue(
        &conn,
        "13900000001",
        "+86",
        "login",
        DEV,
        "9.9.9.2",
        PEPPER,
        lc::DEFAULT_LIMITS,
        1_300,
    )
    .expect("issue 2");
    let v = lc::verify(&conn, &other.id, &other.code, PEPPER, 1_400).expect("verify 2");
    assert!(!v.phone_e164.is_empty());
    // 单次使用：同一码再验一次就没了。
    assert!(matches!(
        lc::verify(&conn, &other.id, &other.code, PEPPER, 1_500),
        Err(IdError::Unauthorized(_))
    ));
}

#[test]
fn challenge_phone_normalization_and_resend_window() {
    // 规范化：只吃 E.164 形状，输出永远是 +CC+数字。
    for good in ["+8613800000000", "0086 138-0000-0000", "13800000000"] {
        let n = lc::normalize_phone(good, "+86").unwrap_or_else(|e| panic!("{good} 应可用: {e}"));
        assert!(n.starts_with("+86") && n.chars().all(|c| c.is_ascii_digit() || c == '+'));
    }
    for bad in [
        "",
        "abc",
        "+86",
        "1",
        "13800000000123456789",
        "+861380000000a",
    ] {
        assert!(
            matches!(
                lc::normalize_phone(bad, "+86"),
                Err(IdError::BadInput(_)) | Err(IdError::Forbidden(_))
            ),
            "{bad:?} 必须被拒"
        );
    }

    let conn = db();
    let issued = lc::issue(
        &conn,
        "13700000000",
        "+86",
        "login",
        DEV,
        "8.8.8.1",
        PEPPER,
        lc::DEFAULT_LIMITS,
        2_000,
    )
    .unwrap();
    // 重发窗口内：RateLimited，并且告诉客户端什么时候能再发。
    let e = lc::resend(
        &conn,
        &issued.id,
        PEPPER,
        lc::DEFAULT_LIMITS,
        issued.resend_after - 1,
    )
    .expect_err("窗口内不许连发");
    assert!(matches!(e, IdError::RateLimited { .. }), "实际: {e}");
    let after = lc::resend(
        &conn,
        &issued.id,
        PEPPER,
        lc::DEFAULT_LIMITS,
        issued.resend_after + 1,
    )
    .expect("窗口后可以重发");
    assert_ne!(after.code, issued.code, "重发要换新码，旧码就此作废");
    let e = lc::verify(
        &conn,
        &issued.id,
        &issued.code,
        PEPPER,
        issued.resend_after + 2,
    )
    .expect_err("旧码在新码发出后不能继续可用");
    assert!(matches!(e, IdError::Unauthorized(_)), "实际: {e}");
    assert!(matches!(
        lc::resend(&conn, "ghost-id", PEPPER, lc::DEFAULT_LIMITS, 3_000),
        Err(IdError::NotFound(_))
    ));
}

#[test]
fn consent_is_versioned_and_purpose_scoped() {
    let conn = db();
    let r = consent::record(
        &conn, "u1", "terms", "1.0.0", "signup", "zh-CN", "iphash", 1_000,
    )
    .expect("record");
    assert_eq!(r.version, "1.0.0");
    assert!(consent::has_accepted(&conn, "u1", "terms", "1.0.0").unwrap());
    assert!(
        consent::has_accepted(&conn, "u1", "terms", "0.9.0").unwrap(),
        "旧版本要求已满足"
    );
    assert!(
        !consent::has_accepted(&conn, "u1", "terms", "2.0.0").unwrap(),
        "新版本必须重新征同意"
    );
    // 另一份文件与另一个人都不算数。
    assert!(!consent::has_accepted(&conn, "u1", "privacy", "1.0.0").unwrap());
    assert!(!consent::has_accepted(&conn, "u2", "terms", "1.0.0").unwrap());

    // 重复提交同一版本不产生第二条回执（同意记录是法律凭证，要能对上）。
    let again = consent::record(
        &conn, "u1", "terms", "1.0.0", "signup", "zh-CN", "iphash2", 2_000,
    )
    .unwrap();
    assert_eq!(again.agreed_at, 1_000, "已存在的同意不许被改写时间");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM identity_consent", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);

    // 非法 doc/purpose/超长版本一律 BadInput，且不落库。
    for (doc, purpose, version) in [
        ("gdpr", "signup", "1.0.0"),
        ("terms", "sell-data", "1.0.0"),
        ("terms", "signup", &"9".repeat(33)),
        ("terms", "signup", ""),
    ] {
        assert!(
            matches!(
                consent::record(&conn, "u1", doc, version, purpose, "zh-CN", "h", 3_000),
                Err(IdError::BadInput(_))
            ),
            "{doc}/{purpose}/{version} 应被拒"
        );
    }
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM identity_consent", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    // 版本号比较本身对奇怪输入要稳。
    assert!(
        consent::version_at_least("1.10.0", "1.9.0").unwrap(),
        "按段比数值，不是比字符串"
    );
    assert!(!consent::version_at_least("1.9.0", "1.10.0").unwrap());
}

#[test]
fn throttle_locks_the_bucket_then_recovers() {
    let conn = db();
    let limit = throttle::LOGIN_IP;
    let key = throttle::key_ip("5.5.5.5");

    // 还没失败过：放行。
    throttle::check(&conn, &key, limit, 1_000).expect("empty bucket is open");
    assert!(
        throttle::status(&conn, &key).unwrap().is_none(),
        "没有记录就不该有状态"
    );

    for i in 0..limit.max_attempts {
        throttle::record_failure(&conn, &key, limit, 2_000 + i * 100).unwrap();
    }
    let e = throttle::check(&conn, &key, limit, 2_600).expect_err("超阈值必须封");
    let retry_after = match e {
        IdError::RateLimited { retry_after_ms } => retry_after_ms,
        other => panic!("期望限速，实际: {other}"),
    };
    assert!(retry_after > 0 && retry_after <= limit.block_ms);

    // 成功一次即清零：不惩罚已经改正的用户。
    throttle::reset(&conn, &key).unwrap();
    throttle::check(&conn, &key, limit, 2_700).expect("清零后放行");
    assert!(throttle::status(&conn, &key).unwrap().is_none());

    // 窗口自然过期同样解锁，而且封一个桶不影响别的桶。
    let other = throttle::key_ip("6.6.6.6");
    throttle::record_failure(&conn, &other, limit, 2_800).unwrap();
    for i in 0..limit.max_attempts {
        throttle::record_failure(&conn, &key, limit, 3_000 + i * 100).unwrap();
    }
    assert!(throttle::check(&conn, &key, limit, 3_500).is_err());
    throttle::check(&conn, &other, limit, 3_500).expect("别的 IP 不该连坐");
    let far = 3_000 + limit.window_ms + limit.block_ms + 1;
    throttle::check(&conn, &key, limit, far).expect("窗口过去后要恢复");

    // step-up 的授权与限速桶彼此独立，别在这里串味。
    assert!(matches!(
        step_up::consume(&conn, "u1", "sid-x", "not-a-purpose", 4_000),
        Err(IdError::BadInput(_))
    ));
}
