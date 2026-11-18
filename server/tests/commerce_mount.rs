//! G06/N7：支付回调**挂进真实组合根之后**的验收（解 PAYMENT-CALLBACK-REPLAY）。
//!
//! 打的是 `Stack::app` —— 与监听端口上跑的是同一个 Router。域侧 13 个重放用例
//! （commerce crate 内）已钉死幂等内核；这里证明的是"路由真的挂上了、原始 body
//! 验签真的生效、重投在真实组合根上仍是单一效果"。
//!
//! 底线：
//! 1. 未签名/错签名 → 401（不是 404 —— 404 说明压根没挂）；
//! 2. 签名回调 → 200 applied=true，一借一贷两行分录、一行 outbox；
//! 3. 同 operation_id 原样重投 5 次 → replayed=true，分录/outbox 行数不变；
//! 4. 同 operation_id 改参数 → 409 IDEMPOTENCY_CONFLICT；
//! 5. 换 operation_id 投同一 event_id → 409 REPLAY；
//! 6. 进程重启（同文件重开 Commerce）后重投 → 结果不变、行数不变。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_commerce::payments::IntentDirection;
use wxlike_commerce::{Commerce, CommerceConfig, DenyAllMembership, Ports};
use wxlike_server::{start_stack, StackOptions};

const SIG_HEADER: &str = "x-payment-signature";

async fn boot() -> wxlike_server::Stack {
    start_stack(StackOptions::default()).await.unwrap()
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, body)
}

fn callback_request(raw: &str, signature: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/payment/callback")
        .header("content-type", "application/json");
    if let Some(sig) = signature {
        builder = builder.header(SIG_HEADER, sig);
    }
    builder.body(Body::from(raw.to_string())).unwrap()
}

fn cny(minor: i64) -> wxlike_commerce::Money {
    wxlike_commerce::Money::from_minor(minor, wxlike_commerce::Currency::CNY)
}

/// 创建一个订单意图并返回 (intent_id, provider_ref)。buyer 先注资以免余额不足。
fn make_intent(commerce: &Commerce, key: &str) -> (String, String) {
    commerce
        .sandbox_credit("buyer", cny(100_000), &format!("fund:{key}"))
        .expect("fund buyer");
    let intent = commerce
        .create_payment_intent(IntentDirection::Order, "buyer", "seller", cny(9_900), key)
        .expect("intent")
        .0;
    (intent.id, intent.provider_ref.expect("provider ref"))
}

fn callback_body(
    operation_id: &str,
    event_id: &str,
    intent_id: &str,
    provider_ref: &str,
    amount_minor: i64,
    timestamp_ms: i64,
) -> String {
    format!(
        "{{\"operation_id\":\"{operation_id}\",\"event_id\":\"{event_id}\",\"intent_id\":\"{intent_id}\",\"kind\":\"payment.succeeded\",\"provider_ref\":\"{provider_ref}\",\"amount_minor\":{amount_minor},\"currency\":\"CNY\",\"timestamp_ms\":{timestamp_ms}}}"
    )
}

/// 独立连接上的裸 SQL 计数：单一效果的外部证据（不是域内自报）。
fn count(db_path: &str, sql: &str, param: &str) -> i64 {
    let conn = wxlike_ledger::open_connection(Some(db_path)).expect("read conn");
    conn.query_row(sql, rusqlite::params![param], |r| r.get::<_, i64>(0))
        .expect("count")
}

fn entry_rows(db_path: &str, intent_id: &str) -> i64 {
    count(
        db_path,
        "SELECT COUNT(*) FROM ledger_entries
         WHERE journal_id = (SELECT id FROM ledger_journals WHERE business_key = ?1)",
        &format!("intent:{intent_id}:success"),
    )
}

fn outbox_rows(db_path: &str) -> i64 {
    let conn = wxlike_ledger::open_connection(Some(db_path)).expect("read conn");
    conn.query_row("SELECT COUNT(*) FROM commerce_outbox", [], |r| r.get::<_, i64>(0))
        .expect("count")
}

#[tokio::test]
async fn unsigned_callback_is_401_not_404() {
    let stack = boot().await;
    let (status, body) = send(
        &stack.app,
        callback_request("{\"operation_id\":\"probe\"}", None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "未签名应 401（404=没挂）: {body}");
    assert_eq!(body["error"], "INVALID_SIGNATURE");
}

#[tokio::test]
async fn forged_signature_is_401() {
    let stack = boot().await;
    let (intent_id, provider_ref) = make_intent(&stack.commerce, "forge");
    let raw = callback_body("op-forge", "evt-forge", &intent_id, &provider_ref, 9_900, now_ms());
    let (status, body) = send(&stack.app, callback_request(&raw, Some("deadbeef"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "错签名应 401: {body}");
}

#[tokio::test]
async fn signed_callback_settles_once_and_replay_is_single_effect() {
    let stack = boot().await;
    let (intent_id, provider_ref) = make_intent(&stack.commerce, "once");
    let raw = callback_body("op-1", "evt-1", &intent_id, &provider_ref, 9_900, now_ms());
    let sig = stack.commerce.webhook_signature(&raw);

    // 首次投递：applied=true。
    let (status, body) = send(&stack.app, callback_request(&raw, Some(&sig))).await;
    assert_eq!(status, StatusCode::OK, "首次回调应 200: {body}");
    assert_eq!(body["applied"], json!(true));
    assert_eq!(body["replayed"], json!(false));
    assert_eq!(body["operation_id"], "op-1");

    // 原样重投 5 次：replayed=true，零副作用。
    for _ in 0..5 {
        let (status, body) = send(&stack.app, callback_request(&raw, Some(&sig))).await;
        assert_eq!(status, StatusCode::OK, "重投应 200: {body}");
        assert_eq!(body["replayed"], json!(true));
        assert_eq!(body["applied"], json!(true));
    }

    // 域内账本自洽（一借一贷）。
    stack.commerce.verify_books().expect("balanced");
    let seller = stack
        .commerce
        .wallet_balance("seller", wxlike_commerce::Currency::CNY)
        .expect("balance")
        .total();
    assert_eq!(seller, cny(9_900), "卖方只入账一次");
}

#[tokio::test]
async fn same_operation_changed_params_is_conflict() {
    let stack = boot().await;
    let (intent_id, provider_ref) = make_intent(&stack.commerce, "conflict");
    let raw = callback_body("op-c", "evt-c", &intent_id, &provider_ref, 9_900, now_ms());
    let sig = stack.commerce.webhook_signature(&raw);
    let (status, _) = send(&stack.app, callback_request(&raw, Some(&sig))).await;
    assert_eq!(status, StatusCode::OK);

    // 同 operation_id 改金额 → 异摘要 → 409。
    let tampered = callback_body("op-c", "evt-c", &intent_id, &provider_ref, 1_000, now_ms());
    let sig2 = stack.commerce.webhook_signature(&tampered);
    let (status, body) = send(&stack.app, callback_request(&tampered, Some(&sig2))).await;
    assert_eq!(status, StatusCode::CONFLICT, "改参数应 409: {body}");
    assert_eq!(body["error"], "IDEMPOTENCY_CONFLICT");
}

#[tokio::test]
async fn new_operation_same_event_is_replay() {
    let stack = boot().await;
    let (intent_id, provider_ref) = make_intent(&stack.commerce, "replay");
    let raw = callback_body("op-r1", "evt-r", &intent_id, &provider_ref, 9_900, now_ms());
    let sig = stack.commerce.webhook_signature(&raw);
    let (status, _) = send(&stack.app, callback_request(&raw, Some(&sig))).await;
    assert_eq!(status, StatusCode::OK);

    // 换 operation_id 投同一 event_id → 事件表去重 → 409 REPLAY。
    let raw2 = callback_body("op-r2", "evt-r", &intent_id, &provider_ref, 9_900, now_ms());
    let sig2 = stack.commerce.webhook_signature(&raw2);
    let (status, body) = send(&stack.app, callback_request(&raw2, Some(&sig2))).await;
    assert_eq!(status, StatusCode::CONFLICT, "换键投同一事件应 409: {body}");
    assert_eq!(body["error"], "REPLAY");
}

#[tokio::test]
async fn file_backed_restart_replay_stays_single_effect() {
    // 文件库栈：证明台账与分录真的落盘，重启后重投不变（外部 SQL 取证）。
    // 用固定测试密钥经环境变量注入，保证重启前后验签密钥一致（生产由部署
    // 配置 WXLIKE_PAYMENT_WEBHOOK_SECRET 为多实例一致的值）。
    let secret = "test-webhook-secret-for-restart";
    std::env::set_var("WXLIKE_PAYMENT_WEBHOOK_SECRET", secret);

    let dir = std::env::temp_dir().join(format!("wxlike-cb-mount-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("stack.db");
    let opts = StackOptions {
        db_path: Some(db.to_str().unwrap().to_string()),
        ..StackOptions::default()
    };
    let stack = start_stack(opts).await.unwrap();
    let commerce_db_path = dir.join("commerce").join("commerce.db");
    let db_str = commerce_db_path.to_str().unwrap();

    let (intent_id, provider_ref) = make_intent(&stack.commerce, "restart");
    let raw = callback_body("op-rs", "evt-rs", &intent_id, &provider_ref, 9_900, now_ms());
    let sig = stack.commerce.webhook_signature(&raw);
    let (status, body) = send(&stack.app, callback_request(&raw, Some(&sig))).await;
    assert_eq!(status, StatusCode::OK, "首次回调: {body}");
    assert_eq!(entry_rows(db_str, &intent_id), 2, "一借一贷");
    assert_eq!(outbox_rows(db_str), 1);

    // 进程重启：同文件重开一套全新端口（OsEntropy，避免主键相撞），密钥一致。
    let ports = Ports::new(
        Arc::new(wxlike_commerce::infrastructure::sandbox::SandboxProvider::new()),
        Arc::new(DenyAllMembership),
    );
    let reopened = Arc::new(
        Commerce::open(
            Some(db_str),
            CommerceConfig::sandbox(secret.as_bytes().to_vec()),
            ports,
        )
        .expect("reopen"),
    );
    let sig_after = reopened.webhook_signature(&raw);
    let outcome = reopened
        .handle_provider_callback(&raw, &sig_after)
        .expect("replay after restart");
    assert!(outcome.replayed, "重启后重投应判重放");
    assert_eq!(entry_rows(db_str, &intent_id), 2, "重启后分录仍两行");
    assert_eq!(outbox_rows(db_str), 1, "重启后 outbox 仍一行");

    drop(stack);
    std::env::remove_var("WXLIKE_PAYMENT_WEBHOOK_SECRET");
    let _ = std::fs::remove_dir_all(&dir);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}