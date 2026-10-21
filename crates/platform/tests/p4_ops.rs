//! P4 运营面验收：对账、找回/人机校验、设备管理、秘密聊天、审核/举报。
//!
//! 这些用例刻意走**真实路由**（tower::oneshot）或真实 DB 路径，不用 mock：
//! 合同 §0.3 明确"禁止以 mock 交差"，而这些功能的缺陷恰恰藏在 HTTP 边界
//! （谁拿到 428、谁拿到 403）与事务边界（重置密码是否同时吊销会话）上。

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};

use common::{app, call, platform, req};
use wxlike_core::e2ee::Device;
use wxlike_platform::Platform;

/// 带自定义头的请求（风控/管理面需要）。
fn req_with(
    method: &str,
    path: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    b.body(match body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    })
    .unwrap()
}

/// 从 "7 + 3 = ?" 里算出答案（测试专用：模拟用户读题）。
fn solve(question: &str) -> String {
    let cleaned: String = question
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect();
    let mut parts = cleaned.split('+');
    let a: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let b: u32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    (a + b).to_string()
}

// ---------------------------------------------------------------------------
// 对账
// ---------------------------------------------------------------------------

#[test]
fn reconcile_clean_after_wallet_ops() {
    let p = Platform::in_memory().unwrap();
    let a = p.register("alice", "pw").unwrap();
    let b = p.register("bob", "pw").unwrap();
    p.fund(&a, 10_000, "seed").unwrap();
    p.transfer(&a, &b, 1_000, "lunch").unwrap();
    let rp = p.create_redpacket(&a, 300, 3, "包", Some("d-a~b")).unwrap();
    p.claim_redpacket(&rp, &b).unwrap();

    let report = p.reconcile().unwrap();
    assert!(
        report.ok,
        "clean ledger must reconcile: {:?}",
        report.mismatches
    );
    assert!(report.users_checked >= 2);
    assert_eq!(report.supply, 10_000, "供给恒等于发行量");
    assert_eq!(report.supply, report.funded_total);
    assert!(report.escrow > 0, "未领完的红包要计入 escrow");
}

#[test]
fn reconcile_flags_balance_tampering() {
    let p = Platform::in_memory().unwrap();
    let a = p.register("alice", "pw").unwrap();
    p.fund(&a, 5_000, "seed").unwrap();

    // 绕过账本直接改余额（模拟手工改库/半提交）
    {
        let c = p.db().lock();
        c.execute(
            "UPDATE wallets SET balance = balance + 1 WHERE user_id = ?1",
            rusqlite::params![a],
        )
        .unwrap();
    }

    let report = p.reconcile().unwrap();
    assert!(!report.ok);
    assert!(report
        .mismatches
        .iter()
        .any(|m| matches!(m, wxlike_platform::Mismatch::Wallet { .. })));
    // 供给也随之偏离
    assert!(report
        .mismatches
        .iter()
        .any(|m| matches!(m, wxlike_platform::Mismatch::Supply { .. })));
}

#[test]
fn reconcile_flags_redpacket_drift() {
    let p = Platform::in_memory().unwrap();
    let a = p.register("alice", "pw").unwrap();
    p.fund(&a, 5_000, "seed").unwrap();
    let rp = p.create_redpacket(&a, 300, 3, "包", None).unwrap();
    {
        let c = p.db().lock();
        c.execute(
            "UPDATE redpackets SET claimed = 9 WHERE id = ?1",
            rusqlite::params![rp],
        )
        .unwrap();
    }
    let report = p.reconcile().unwrap();
    assert!(!report.ok);
    assert!(report
        .mismatches
        .iter()
        .any(|m| matches!(m, wxlike_platform::Mismatch::Redpacket { .. })));
}

#[test]
fn reconcile_history_is_recorded() {
    let p = Platform::in_memory().unwrap();
    p.reconcile().unwrap();
    p.reconcile().unwrap();
    let runs = p.reconcile_history(10).unwrap();
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|(_, ok, n)| *ok && *n == 0));
    assert!(p.last_reconcile().unwrap().is_some());
}

// ---------------------------------------------------------------------------
// 找回 + 人机校验
// ---------------------------------------------------------------------------

#[test]
fn recovery_code_resets_password_and_revokes_sessions() {
    let p = Platform::in_memory().unwrap();
    let uid = p.register("alice", "pw").unwrap();
    let token = p.create_session(&uid, 60_000).unwrap();
    assert!(p.lookup_token(&token).unwrap().is_some());

    let code = p.issue_recovery_code(&uid).unwrap();
    assert!(p.has_recovery_code(&uid).unwrap());

    // 错误恢复码不能重置
    assert!(p
        .reset_password_with_code("alice", "0000-0000-0000-0000", "newpw")
        .is_err());

    let reset_uid = p.reset_password_with_code("alice", &code, "newpw").unwrap();
    assert_eq!(reset_uid, uid);

    // 旧会话被吊销、旧密码失效、新密码可用
    assert!(p.lookup_token(&token).unwrap().is_none());
    assert_eq!(p.verify_credentials("alice", "pw").unwrap(), None);
    assert_eq!(
        p.verify_credentials("alice", "newpw").unwrap(),
        Some(uid.clone())
    );
    // 恢复码已作废
    assert!(p.reset_password_with_code("alice", &code, "third").is_err());
    assert!(!p.has_recovery_code(&uid).unwrap());
}

#[test]
fn captcha_is_single_use_and_wrong_answer_rejected() {
    let p = Platform::in_memory().unwrap();
    let (id, question) = p.issue_captcha().unwrap();
    let answer = solve(&question);

    assert!(p.solve_captcha(&id, "99999").is_err(), "错答案必须拒绝");
    assert!(p.solve_captcha(&id, &answer).is_ok());
    assert!(p.solve_captcha(&id, &answer).is_err(), "同一题不能二次使用");
}

/// issue #19：**默认部署下转发头无效**。
///
/// 直连时 `x-forwarded-for` 是客户端自己填的，信它等于"换个头 = 换个限流桶"，
/// 注册/登录失败计数形同虚设。这里用同一个 peer 地址、每次换一个头值：
/// 桶必须还是同一个（第 3 次照样 428）。
#[tokio::test]
async fn forwarded_headers_cannot_reset_the_register_bucket() {
    let state = platform(); // trust_proxy 默认 false
    let router = app(state.clone());

    let peer: std::net::SocketAddr = "10.9.9.9:5555".parse().unwrap();
    for (i, name) in ["v1", "v2", "v3"].iter().enumerate() {
        let mut r = req_with(
            "POST",
            "/api/register",
            Some(json!({"name": name, "password": "pw"})),
            // 每次都换一个头：如果它被采信，就会换到一个新桶
            &[("x-forwarded-for", &format!("203.0.113.{i}"))],
        );
        r.extensions_mut().insert(axum::extract::ConnectInfo(peer));
        let (st, body) = call(&router, r).await;
        if i < 2 {
            assert_eq!(st, StatusCode::OK, "{name}: {body}");
        } else {
            assert_eq!(
                st,
                StatusCode::PRECONDITION_REQUIRED,
                "换转发头不该换到新桶（来源应当只认 peer）：{body}"
            );
        }
    }
}

#[tokio::test]
async fn register_challenge_requires_captcha() {
    // 这条验证的是**受信代理**部署下的行为：`x-forwarded-for` 就是来源。
    // 默认（trust_proxy=false）下这个头根本不读——见下面那条测试（issue #19）。
    let state = wxlike_platform::Platform::in_memory_with(wxlike_platform::PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        trust_proxy: true,
        ..Default::default()
    })
    .unwrap()
    .shared();
    let router = app(state.clone());

    // 默认阈值：同来源第 3 次注册起要求人机校验
    for name in ["u1", "u2"] {
        let (st, _) = call(
            &router,
            req_with(
                "POST",
                "/api/register",
                Some(json!({"name": name, "password": "pw"})),
                &[("x-forwarded-for", "10.0.0.9")],
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{name}");
    }

    let (st, _) = call(
        &router,
        req_with(
            "POST",
            "/api/register",
            Some(json!({"name": "u3", "password": "pw"})),
            &[("x-forwarded-for", "10.0.0.9")],
        ),
    )
    .await;
    assert_eq!(st, StatusCode::PRECONDITION_REQUIRED, "应要求人机校验");

    let (st, body) = call(&router, req_with("POST", "/api/captcha", None, &[])).await;
    assert_eq!(st, StatusCode::OK);
    let id = body["challenge_id"].as_str().unwrap().to_string();
    let answer = solve(body["question"].as_str().unwrap());

    let (st, _) = call(
        &router,
        req_with(
            "POST",
            "/api/register",
            Some(json!({
                "name": "u3", "password": "pw",
                "captcha_id": id, "captcha_answer": answer,
            })),
            &[("x-forwarded-for", "10.0.0.9")],
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "过人机校验后应注册成功");
}

#[tokio::test]
async fn login_lockout_requires_captcha_then_clears() {
    let state = platform();
    let router = app(state.clone());
    let (st, _) = call(
        &router,
        req_with(
            "POST",
            "/api/register",
            Some(json!({"name": "alice", "password": "pw"})),
            &[("x-forwarded-for", "10.0.0.7")],
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // 前 4 次错误密码：正常 401（阈值 login_fail_challenge_at = 5，
    // 第 5 次尝试本身计入，所以从第 5 次起先要人机校验）
    for _ in 0..4 {
        let (st, _) = call(
            &router,
            req_with(
                "POST",
                "/api/login",
                Some(json!({"name": "alice", "password": "bad"})),
                &[("x-forwarded-for", "10.0.0.7")],
            ),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }

    // 第 5 次：即使密码正确也必须先过人机校验
    let (st, _) = call(
        &router,
        req_with(
            "POST",
            "/api/login",
            Some(json!({"name": "alice", "password": "pw"})),
            &[("x-forwarded-for", "10.0.0.7")],
        ),
    )
    .await;
    assert_eq!(st, StatusCode::PRECONDITION_REQUIRED);

    let (_, body) = call(&router, req_with("POST", "/api/captcha", None, &[])).await;
    let id = body["challenge_id"].as_str().unwrap().to_string();
    let answer = solve(body["question"].as_str().unwrap());

    let (st, _) = call(
        &router,
        req_with(
            "POST",
            "/api/login",
            Some(json!({
                "name": "alice", "password": "pw",
                "captcha_id": id, "captcha_answer": answer,
            })),
            &[("x-forwarded-for", "10.0.0.7")],
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "正确密码 + 正确验证码应放行");

    // 成功一次后失败计数清零：再登录不再要求验证码
    let (st, _) = call(
        &router,
        req_with(
            "POST",
            "/api/login",
            Some(json!({"name": "alice", "password": "pw"})),
            &[("x-forwarded-for", "10.0.0.7")],
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// 设备管理
// ---------------------------------------------------------------------------

#[test]
fn sessions_list_marks_current_and_revokes_other() {
    let p = Platform::in_memory().unwrap();
    let uid = p.register("alice", "pw").unwrap();
    let phone = p
        .create_session_for_device(&uid, 60_000, "dev-phone", "Pixel", "10.0.0.1")
        .unwrap();
    let desk = p
        .create_session_for_device(&uid, 60_000, "dev-desk", "ThinkPad", "10.0.0.2")
        .unwrap();

    let list = p.sessions_of(&uid, &phone).unwrap();
    assert_eq!(list.len(), 2);
    let current: Vec<_> = list.iter().filter(|s| s.current).collect();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].device_name, "Pixel");

    // 下线桌面端
    let sid = list
        .iter()
        .find(|s| s.device_id == "dev-desk")
        .map(|s| s.sid.clone())
        .unwrap();
    assert!(p.revoke_session_by_sid(&uid, &sid).unwrap());
    assert!(p.lookup_token(&desk).unwrap().is_none());
    assert!(p.lookup_token(&phone).unwrap().is_some());
    // 再踢一次不报错（幂等语义：不存在即未命中）
    assert!(!p.revoke_session_by_sid(&uid, &sid).unwrap());
}

#[test]
fn revoke_others_keeps_current_session() {
    let p = Platform::in_memory().unwrap();
    let uid = p.register("alice", "pw").unwrap();
    let keep = p.create_session(&uid, 60_000).unwrap();
    let a = p.create_session(&uid, 60_000).unwrap();
    let b = p.create_session(&uid, 60_000).unwrap();

    let n = p.revoke_other_sessions(&uid, &keep).unwrap();
    assert_eq!(n, 2);
    assert!(p.lookup_token(&keep).unwrap().is_some());
    assert!(p.lookup_token(&a).unwrap().is_none());
    assert!(p.lookup_token(&b).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// 秘密聊天
// ---------------------------------------------------------------------------

#[test]
fn safety_number_is_symmetric_and_changes_with_identity() {
    let p = Platform::in_memory().unwrap();
    let a = p.register("alice", "pw").unwrap();
    let b = p.register("bob", "pw").unwrap();

    let dev_a = Device::new();
    let dev_b = Device::new();
    p.publish_keys(&a, &dev_a.bundle(&a)).unwrap();
    p.publish_keys(&b, &dev_b.bundle(&b)).unwrap();

    let ab = p.safety_number(&a, &b).unwrap();
    let ba = p.safety_number(&b, &a).unwrap();
    assert_eq!(ab, ba, "安全码必须顺序无关");
    assert_eq!(ab.len(), 60);
    assert!(ab.chars().all(|c| c.is_ascii_digit()));

    // 核对一次
    let identity = p.public_identity(&b).unwrap().unwrap();
    p.verify_peer(&a, &b, &identity).unwrap();
    assert!(p.verification_of(&a, &b).unwrap().is_some());

    // bob 换设备 → 安全码变化，旧核对自动失效
    let dev_b2 = Device::new();
    p.publish_keys(&b, &dev_b2.bundle(&b)).unwrap();
    let ab2 = p.safety_number(&a, &b).unwrap();
    assert_ne!(ab, ab2);
    assert!(p.verification_of(&a, &b).unwrap().is_none());
    // 拿旧公钥冒充核对必须被拒
    assert!(p.verify_peer(&a, &b, &identity).is_err());
}

#[test]
fn secret_chat_ttl_validation_and_membership() {
    let p = Platform::in_memory().unwrap();
    let a = p.register("alice", "pw").unwrap();
    let b = p.register("bob", "pw").unwrap();
    let conv = wxlike_platform::direct_conv_id(&a, &b);

    let st = p.set_secret_chat(&a, &conv, true, 60 * 60 * 1000).unwrap();
    assert!(st.enabled);
    assert_eq!(p.secret_state(&conv).unwrap().ttl_ms, 60 * 60 * 1000);
    assert!(p.is_secret_conv(&conv).unwrap());

    // 非法 TTL
    assert!(p.set_secret_chat(&a, &conv, true, 1234).is_err());
    // 非参与者
    let c = p.register("carol", "pw").unwrap();
    assert!(p.set_secret_chat(&c, &conv, true, 0).is_err());
    // 群会话不允许开秘密模式
    assert!(p.set_secret_chat(&a, "g-123", true, 0).is_err());

    p.set_secret_chat(&a, &conv, false, 0).unwrap();
    assert!(!p.is_secret_conv(&conv).unwrap());
}

// ---------------------------------------------------------------------------
// 审核 / 举报
// ---------------------------------------------------------------------------

#[test]
fn pre_review_holds_and_release_returns_text() {
    let p = Platform::in_memory().unwrap();
    let v = p
        .moderate_for_send("alice", "d-a~b", "加我微信 带你刷单")
        .unwrap();
    assert_eq!(v.decision.outcome, wxlike_moderation::Outcome::Hold);
    let queued = v.queued_id.expect("挂起必须入队");

    let queue = p.moderation_queue(10).unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].sender, "alice");

    let released = p.release_moderated(&queued, "mod1").unwrap().unwrap();
    assert_eq!(released.text, "加我微信 带你刷单");
    assert!(p.moderation_queue(10).unwrap().is_empty());
}

#[test]
fn post_review_publishes_flagged_and_hard_block_never_publishes() {
    let p = Platform::in_memory().unwrap();
    p.set_moderation_mode(wxlike_moderation::Mode::PostReview);

    let soft = p.moderate_for_send("alice", "d-a~b", "傻逼").unwrap();
    assert_eq!(
        soft.decision.outcome,
        wxlike_moderation::Outcome::PublishFlagged
    );
    assert!(soft.queued_id.is_none(), "先发后审不进挂起队列");

    let hard = p.moderate_for_send("alice", "d-a~b", "裸聊").unwrap();
    assert_eq!(hard.decision.outcome, wxlike_moderation::Outcome::Reject);
}

#[test]
fn report_lifecycle_and_illegal_transition() {
    let p = Platform::in_memory().unwrap();
    let a = p.register("alice", "pw").unwrap();
    let target = p.register("bob", "pw").unwrap();

    let t = p
        .create_report(&a, "user", &target, wxlike_report::Reason::Spam, "刷屏")
        .unwrap();
    assert_eq!(t.state, wxlike_report::State::Open);

    // 未分诊直接处置必须失败
    assert!(p
        .act_on_report(&t.id, "mod1", wxlike_report::Action::Warn, "直接处罚")
        .is_err());

    let t = p.triage_report(&t.id, "mod1", "受理").unwrap();
    assert_eq!(t.state, wxlike_report::State::Triaged);
    let t = p
        .act_on_report(
            &t.id,
            "mod1",
            wxlike_report::Action::Mute {
                duration_ms: 3_600_000,
            },
            "禁言 1 小时",
        )
        .unwrap();
    assert_eq!(t.state, wxlike_report::State::Actioned);
    let t = p.close_report(&t.id, "mod1", "已通知").unwrap();
    assert_eq!(t.state, wxlike_report::State::Closed);
    assert_eq!(t.history.len(), 3);

    // 终态后不可再流转
    assert!(p.reject_report(&t.id, "mod1", "改判").is_err());
    // 举报人自查能看到
    assert_eq!(p.reports_by(&a, 10).unwrap().len(), 1);
    // 队列只剩未终结工单
    assert!(p.report_queue(None, 10).unwrap().is_empty());
}

#[tokio::test]
async fn admin_endpoints_reject_without_token() {
    let state = platform();
    let router = app(state.clone());

    let (st, _) = call(&router, req("GET", "/api/admin/reconcile", None, None)).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "未配置令牌时管理面必须关闭");

    // 配置令牌后：错令牌 403，对令牌 200
    let with_admin = {
        let opts = wxlike_platform::PlatformOptions {
            session_ttl_ms: 60_000,
            pbkdf2_rounds: 1_000,
            admin_token: Some("s3cret".into()),
            ..Default::default()
        };
        Platform::in_memory_with(opts).unwrap().shared()
    };
    let router2 = app(with_admin);

    let (st, _) = call(
        &router2,
        req_with(
            "GET",
            "/api/admin/reconcile",
            None,
            &[("x-admin-token", "nope")],
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (st, body) = call(
        &router2,
        req_with(
            "GET",
            "/api/admin/reconcile",
            None,
            &[("x-admin-token", "s3cret")],
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(body.get("history").is_some());

    let (st, _) = call(
        &router2,
        req_with(
            "POST",
            "/api/admin/reconcile",
            None,
            &[("x-admin-token", "s3cret")],
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "带令牌可触发对账");
}
