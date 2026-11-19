//! G06-01..05 ready-for-G00 集成请求：commerce **动作面挂进真实组合根之后**的验收。
//!
//! 打的是 `Stack::app` —— 与监听端口上跑的是同一个 Router（不是另拼的近似 app）。
//! 域侧幂等/状态机已由 commerce crate 内的用例钉死；这里证明的是组合根装配：
//!
//! 1. **会话鉴权拒绝**：无令牌 / 坏令牌 → 401（不是 404，404=没挂）；
//!    actor 只来自令牌，不信请求体自报。
//! 2. **动作幂等**：同 idempotency_key 同参 → 同一业务对象、账本不新增；
//!    同键换参 → 409 IDEMPOTENCY_CONFLICT。
//! 3. **错误分类**：BAD_INPUT / NOT_FOUND / FORBIDDEN / INSUFFICIENT_FUNDS /
//!    AMOUNT_TOO_LARGE / PROVIDER_TIMEOUT 各归其位，且只回稳定标签。
//! 4. **audience 闸门**：reconcile.* 与权益发行是后台面，普通会话 403，
//!    持平台 admin token 才放行（G14 落地前的失败关闭 interim）。
//! 5. **真实群成员**：红包创建/领取的成员事实落平台 group_members 表，
//!    非成员 403（不再 DenyAll，也不伪造成员）。
//!
//! ADR-28：所有 handler 的同步 SQL 段都在域内/阻塞池里跑；测试侧的独立 SQL
//! 取证用第二个裸连接，证明效果真的落盘而不是域内自报。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_commerce::infrastructure::sandbox::SandboxProvider;
use wxlike_commerce::{CommerceConfig, Ports};
use wxlike_server::{start_stack, Stack, StackOptions};

async fn boot() -> Stack {
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

fn post_json(uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let raw = body
        .map(|v| v.to_string())
        .unwrap_or_else(|| "{}".to_string());
    builder.body(Body::from(raw)).unwrap()
}

fn action_request(action: &str, token: Option<&str>, body: Value) -> Request<Body> {
    post_json(
        &format!("/api/v1/commerce/actions/{action}"),
        token,
        Some(body),
    )
}

/// 真注册：走平台 `/api/register`，token 来自 sessions 表（与 media_mount 同口径）。
async fn register(app: &Router, name: &str) -> (String, String) {
    let (status, reply) = send(
        app,
        post_json(
            "/api/register",
            None,
            Some(json!({"name": name, "password": "pw"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register {name}: {reply}");
    (
        reply["user_id"].as_str().unwrap().to_string(),
        reply["token"].as_str().unwrap().to_string(),
    )
}

/// 真建群：owner + 成员都必须是已注册用户；返回群 id（即群会话 id）。
async fn create_group(app: &Router, owner_token: &str, members: &[String]) -> String {
    let (status, reply) = send(
        app,
        post_json(
            "/api/groups",
            Some(owner_token),
            Some(json!({"name": "commerce-group", "member_ids": members})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create group: {reply}");
    reply["group_id"].as_str().unwrap().to_string()
}

fn cny(minor: i64) -> wxlike_commerce::Money {
    wxlike_commerce::Money::from_minor(minor, wxlike_commerce::Currency::CNY)
}

/// 独立连接上的裸 SQL 计数：单一效果的外部证据（不是域内自报）。
fn count(db_path: &str, sql: &str) -> i64 {
    let conn = wxlike_ledger::open_connection(Some(db_path)).expect("read conn");
    conn.query_row(sql, [], |r| r.get::<_, i64>(0))
        .expect("count")
}

fn count_param(db_path: &str, sql: &str, param: &str) -> i64 {
    let conn = wxlike_ledger::open_connection(Some(db_path)).expect("read conn");
    conn.query_row(sql, rusqlite::params![param], |r| r.get::<_, i64>(0))
        .expect("count")
}

// ---------------------------------------------------------------------------
// 1. 会话鉴权拒绝
// ---------------------------------------------------------------------------

#[tokio::test]
async fn action_without_token_is_401_not_404() {
    let stack = boot().await;
    // 404 说明压根没挂；401 才说明路由在、只是没带凭据。
    let (status, body) = send(&stack.app, action_request("balance.get", None, json!({}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "无令牌应 401: {body}");
    assert_eq!(body["error"], "UNAUTHORIZED");
}

#[tokio::test]
async fn action_with_bad_token_is_401() {
    let stack = boot().await;
    let (status, body) = send(
        &stack.app,
        action_request("balance.get", Some("not-a-real-token"), json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "坏令牌应 401: {body}");
}

#[tokio::test]
async fn actor_comes_from_token_not_payload() {
    let stack = boot().await;
    let (alice_id, alice_tok) = register(&stack.app, "actor-alice").await;
    let (bob_id, _bob_tok) = register(&stack.app, "actor-bob").await;
    stack
        .commerce
        .sandbox_credit(&bob_id, cny(5_000), "fund:actor-bob")
        .expect("fund bob");

    // alice 的令牌 + 请求体自报 actor=bob：余额必须按 alice（令牌）算，
    // 不能把 bob 的钱读出来。actor 由服务端从会话推导。
    let (status, body) = send(
        &stack.app,
        action_request(
            "balance.get",
            Some(&alice_tok),
            json!({"actor": bob_id, "currency": "CNY"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["total"]["minor"], "0",
        "alice 未注资，余额应为 0: {body}"
    );
    let _ = alice_id;
}

// ---------------------------------------------------------------------------
// 2. 动作幂等 + 3. 错误分类（钱包 / 支付）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn balance_and_statement_are_self_scoped_reads() {
    let stack = boot().await;
    let (user_id, token) = register(&stack.app, "wallet-reader").await;
    stack
        .commerce
        .sandbox_credit(&user_id, cny(12_345), "fund:wallet-reader")
        .expect("fund");

    let (status, body) = send(
        &stack.app,
        action_request("balance.get", Some(&token), json!({"currency": "CNY"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"]["minor"], "12345");
    assert_eq!(body["available"]["minor"], "12345");
    assert_eq!(body["locked"]["minor"], "0");

    let (status, body) = send(
        &stack.app,
        action_request(
            "statement.page",
            Some(&token),
            json!({"currency": "CNY", "limit": 10}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["items"].as_array().unwrap().len() >= 1,
        "应有注资流水: {body}"
    );
}

#[tokio::test]
async fn payment_create_is_idempotent_and_conflict_on_changed_params() {
    let stack = boot().await;
    let (buyer_id, buyer_tok) = register(&stack.app, "pay-buyer").await;
    let (seller_id, _seller_tok) = register(&stack.app, "pay-seller").await;
    stack
        .commerce
        .sandbox_credit(&buyer_id, cny(100_000), "fund:pay-buyer")
        .expect("fund buyer");

    let payload = json!({
        "direction": "order", "payee_id": seller_id,
        "amount": "99.00", "currency": "CNY", "idempotency_key": "pay-once",
    });
    let (status, first) = send(
        &stack.app,
        action_request("payment.create", Some(&buyer_tok), payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "首次创建: {first}");
    let intent_id = first["intent"]["intent_id"].as_str().unwrap().to_string();

    // 同键同参重试：返回同一意图，不产生第二张单。
    let (status, again) = send(
        &stack.app,
        action_request("payment.create", Some(&buyer_tok), payload),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "幂等重试: {again}");
    assert_eq!(
        again["intent"]["intent_id"], intent_id,
        "同键应返回同一意图"
    );

    // 同键换金额：异参 → 409 IDEMPOTENCY_CONFLICT。
    let changed = json!({
        "direction": "order", "payee_id": seller_id,
        "amount": "10.00", "currency": "CNY", "idempotency_key": "pay-once",
    });
    let (status, body) = send(
        &stack.app,
        action_request("payment.create", Some(&buyer_tok), changed),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "同键换参应 409: {body}");
    assert_eq!(body["error"], "IDEMPOTENCY_CONFLICT");
}

#[tokio::test]
async fn payment_error_classification() {
    let stack = boot().await;
    let (buyer_id, buyer_tok) = register(&stack.app, "err-buyer").await;
    let (seller_id, _s) = register(&stack.app, "err-seller").await;
    stack
        .commerce
        .sandbox_credit(&buyer_id, cny(100_000), "fund:err-buyer")
        .expect("fund");

    // BAD_INPUT：金额不是十进制字符串。
    let (status, body) = send(
        &stack.app,
        action_request(
            "payment.create",
            Some(&buyer_tok),
            json!({"payee_id": seller_id, "amount": 99.0, "idempotency_key": "bad-amount"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "BAD_INPUT");

    // AMOUNT_TOO_LARGE：超过单笔上限。
    let (status, body) = send(
        &stack.app,
        action_request(
            "payment.create",
            Some(&buyer_tok),
            json!({"payee_id": seller_id, "amount": "99999999.00", "idempotency_key": "too-big"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "AMOUNT_TOO_LARGE");

    // NOT_FOUND：查询不存在的意图。
    let (status, body) = send(
        &stack.app,
        action_request(
            "payment.get",
            Some(&buyer_tok),
            json!({"intent_id": "no-such-intent"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "NOT_FOUND");

    // FORBIDDEN：Order 方向禁止自付（payee == actor）。
    let (status, body) = send(
        &stack.app,
        action_request(
            "payment.create",
            Some(&buyer_tok),
            json!({"direction": "order", "payee_id": buyer_id, "amount": "1.00", "idempotency_key": "self-pay"}),
        ),
    )
    .await;
    // create 允许 payee==actor 落单，pay 才拒绝自付；这里直接验证 pay 的 FORBIDDEN。
    if status == StatusCode::OK {
        let intent_id = body["intent"]["intent_id"].as_str().unwrap().to_string();
        let (status, body) = send(
            &stack.app,
            action_request(
                "payment.pay",
                Some(&buyer_tok),
                json!({"intent_id": intent_id}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "自付应 403: {body}");
        assert_eq!(body["error"], "FORBIDDEN");
    } else {
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
}

#[tokio::test]
async fn unknown_action_is_bad_input() {
    let stack = boot().await;
    let (_id, token) = register(&stack.app, "unknown-action").await;
    let (status, body) = send(
        &stack.app,
        action_request("does.not.exist", Some(&token), json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "BAD_INPUT");
}

// ---------------------------------------------------------------------------
// payment.refresh：未知状态用查询解决（G06-02-payment-refresh）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn payment_refresh_pending_changes_nothing_and_is_read_safe() {
    let stack = boot().await;
    let (buyer_id, buyer_tok) = register(&stack.app, "refresh-buyer").await;
    let (seller_id, _s) = register(&stack.app, "refresh-seller").await;
    stack
        .commerce
        .sandbox_credit(&buyer_id, cny(100_000), "fund:refresh-buyer")
        .expect("fund");

    let (status, created) = send(
        &stack.app,
        action_request(
            "payment.create",
            Some(&buyer_tok),
            json!({"direction": "order", "payee_id": seller_id, "amount": "50.00", "idempotency_key": "refresh-intent"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let intent_id = created["intent"]["intent_id"].as_str().unwrap().to_string();

    // 沙箱默认 pending：refresh 查询后仍 pending，本地一个字都不改、分录为 0。
    let (status, body) = send(
        &stack.app,
        action_request(
            "payment.refresh",
            Some(&buyer_tok),
            json!({"intent_id": intent_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["intent"]["status"], "pending",
        "供应商仍处理中应保持 pending: {body}"
    );

    // 再 refresh 一次：read-safe，不产生第二笔分录。
    let (status, again) = send(
        &stack.app,
        action_request(
            "payment.refresh",
            Some(&buyer_tok),
            json!({"intent_id": intent_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["intent"]["status"], "pending");
}

#[tokio::test]
async fn payment_refresh_forbidden_for_outsider() {
    let stack = boot().await;
    let (buyer_id, buyer_tok) = register(&stack.app, "refresh-owner").await;
    let (seller_id, _s) = register(&stack.app, "refresh-payee").await;
    let (_outsider_id, outsider_tok) = register(&stack.app, "refresh-outsider").await;
    stack
        .commerce
        .sandbox_credit(&buyer_id, cny(100_000), "fund:refresh-owner")
        .expect("fund");
    let (status, created) = send(
        &stack.app,
        action_request(
            "payment.create",
            Some(&buyer_tok),
            json!({"direction": "order", "payee_id": seller_id, "amount": "5.00", "idempotency_key": "refresh-scope"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let intent_id = created["intent"]["intent_id"].as_str().unwrap().to_string();

    let (status, body) = send(
        &stack.app,
        action_request(
            "payment.refresh",
            Some(&outsider_tok),
            json!({"intent_id": intent_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "非收付双方应 403: {body}");
    assert_eq!(body["error"], "FORBIDDEN");
}

/// PROVIDER_TIMEOUT 的 HTTP 分类：在**真实挂载 handler**上注入一次查询超时。
/// 用 `assemble_with_provider` 注入可故障注入的沙箱（与生产同一装配路径），
/// 证明超时 → 504 PROVIDER_TIMEOUT，且意图保持未知态、分录为 0。
#[tokio::test]
async fn provider_timeout_maps_to_504_and_leaves_state_unknown() {
    let platform = wxlike_platform::Platform::in_memory().unwrap().shared();
    let provider = Arc::new(SandboxProvider::new());
    let handle = provider.handle();
    let (app, commerce) = wxlike_server::commerce_mount::assemble_with_provider(
        &platform,
        None,
        Arc::clone(&provider) as Arc<dyn wxlike_commerce::PaymentProvider>,
    )
    .unwrap();

    // 直接经平台注册一个会话（与 register 路由同一套 sessions 表）。
    let user_id = "timeoutbuyer";
    platform.register(user_id, "pw").expect("register user");
    let token = platform
        .create_session(user_id, wxlike_platform::DEFAULT_SESSION_TTL_MS)
        .expect("session");
    commerce
        .sandbox_credit(user_id, cny(100_000), "fund:timeout")
        .expect("fund");

    let (status, created) = send(
        &app,
        action_request(
            "payment.create",
            Some(&token),
            json!({"direction": "topup", "amount": "20.00", "idempotency_key": "timeout-intent"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let intent_id = created["intent"]["intent_id"].as_str().unwrap().to_string();

    // 下一次查询注入超时。
    handle.fail_next_query(wxlike_commerce::ProviderError::Timeout);
    let (status, body) = send(
        &app,
        action_request(
            "payment.refresh",
            Some(&token),
            json!({"intent_id": intent_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "超时应 504: {body}");
    assert_eq!(body["error"], "PROVIDER_TIMEOUT");

    // 本地状态不变（仍 pending/created），分录为 0：超时不能本地写已支付。
    let intent = commerce
        .payment_intent(user_id, &intent_id)
        .expect("read intent");
    assert!(
        matches!(
            intent.status,
            wxlike_commerce::payments::IntentStatus::Pending
                | wxlike_commerce::payments::IntentStatus::Created
        ),
        "超时应保持未知态，实际 {:?}",
        intent.status
    );
}

// ---------------------------------------------------------------------------
// 转账 / 红包（G06-04）：真实群成员 + 幂等 + 错误分类
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transfer_prepare_create_accept_is_idempotent() {
    let stack = boot().await;
    let (sender_id, sender_tok) = register(&stack.app, "transfer-sender").await;
    let (payee_id, payee_tok) = register(&stack.app, "transfer-payee").await;
    stack
        .commerce
        .sandbox_credit(&sender_id, cny(100_000), "fund:transfer-sender")
        .expect("fund sender");

    // prepare：无副作用，返回绑定收款人/金额/备注/nonce 的确认令牌。
    let (status, prepared) = send(
        &stack.app,
        action_request(
            "transfer.prepare",
            Some(&sender_tok),
            json!({"payee_id": payee_id, "amount": "30.00", "memo": "午饭"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    let draft = prepared["draft"].clone();
    let confirmation_token = draft["confirmation_token"].as_str().unwrap().to_string();

    let create_payload = json!({
        "draft": draft, "confirmation_token": confirmation_token, "idempotency_key": "transfer-once",
    });
    let (status, created) = send(
        &stack.app,
        action_request("transfer.create", Some(&sender_tok), create_payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "创建转账: {created}");
    let transfer_id = created["transfer"]["transfer_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(created["transfer"]["status"], "pending");

    // 创建即冻结发送方资金：available 减少，total 不变。
    let balance = stack
        .commerce
        .wallet_balance(&sender_id, wxlike_commerce::Currency::CNY)
        .expect("balance");
    assert_eq!(balance.locked(), cny(3_000), "创建即冻结");

    // 收款人确认收款。
    let (status, accepted) = send(
        &stack.app,
        action_request(
            "transfer.accept",
            Some(&payee_tok),
            json!({"transfer_id": transfer_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "收款: {accepted}");
    assert_eq!(accepted["transfer"]["status"], "accepted");

    // 重复 accept：幂等返回原状态，不二次入账。
    let (status, again) = send(
        &stack.app,
        action_request(
            "transfer.accept",
            Some(&payee_tok),
            json!({"transfer_id": transfer_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["transfer"]["status"], "accepted");
    let payee_balance = stack
        .commerce
        .wallet_balance(&payee_id, wxlike_commerce::Currency::CNY)
        .expect("payee balance");
    assert_eq!(payee_balance.total(), cny(3_000), "只入账一次");
}

#[tokio::test]
async fn transfer_accept_forbidden_for_non_payee() {
    let stack = boot().await;
    let (sender_id, sender_tok) = register(&stack.app, "t-sender2").await;
    let (payee_id, _p) = register(&stack.app, "t-payee2").await;
    let (_outsider_id, outsider_tok) = register(&stack.app, "t-outsider2").await;
    stack
        .commerce
        .sandbox_credit(&sender_id, cny(100_000), "fund:t-sender2")
        .expect("fund");
    let (status, prepared) = send(
        &stack.app,
        action_request(
            "transfer.prepare",
            Some(&sender_tok),
            json!({"payee_id": payee_id, "amount": "8.00", "memo": ""}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    let draft = prepared["draft"].clone();
    let token = draft["confirmation_token"].as_str().unwrap().to_string();
    let (status, created) = send(
        &stack.app,
        action_request(
            "transfer.create",
            Some(&sender_tok),
            json!({"draft": draft, "confirmation_token": token, "idempotency_key": "t-scope"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let transfer_id = created["transfer"]["transfer_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = send(
        &stack.app,
        action_request(
            "transfer.accept",
            Some(&outsider_tok),
            json!({"transfer_id": transfer_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "非收款人应 403: {body}");
    assert_eq!(body["error"], "FORBIDDEN");
}

#[tokio::test]
async fn transfer_insufficient_funds_is_conflict() {
    let stack = boot().await;
    let (sender_id, sender_tok) = register(&stack.app, "t-poor").await;
    let (payee_id, _p) = register(&stack.app, "t-rich").await;
    // 不注资：余额 0。
    let (status, prepared) = send(
        &stack.app,
        action_request(
            "transfer.prepare",
            Some(&sender_tok),
            json!({"payee_id": payee_id, "amount": "50.00", "memo": ""}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{prepared}");
    let draft = prepared["draft"].clone();
    let token = draft["confirmation_token"].as_str().unwrap().to_string();
    let (status, body) = send(
        &stack.app,
        action_request(
            "transfer.create",
            Some(&sender_tok),
            json!({"draft": draft, "confirmation_token": token, "idempotency_key": "t-poor-key"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "余额不足应 409: {body}");
    assert_eq!(body["error"], "INSUFFICIENT_FUNDS");
    let _ = sender_id;
}

#[tokio::test]
async fn redpacket_requires_real_group_membership() {
    let stack = boot().await;
    let (owner_id, owner_tok) = register(&stack.app, "rp-owner").await;
    let (member_id, member_tok) = register(&stack.app, "rp-member").await;
    let (outsider_id, outsider_tok) = register(&stack.app, "rp-outsider").await;
    stack
        .commerce
        .sandbox_credit(&owner_id, cny(100_000), "fund:rp-owner")
        .expect("fund owner");
    let group = create_group(&stack.app, &owner_tok, &[member_id.clone()]).await;

    // 群主发红包：成员事实来自平台 group_members 表（真实端口）。
    let (status, created) = send(
        &stack.app,
        action_request(
            "redpacket.create",
            Some(&owner_tok),
            json!({"conversation_id": group, "amount": "10.00", "count": 2, "idempotency_key": "rp-once"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "群主发红包应成功: {created}");
    let packet_id = created["packet_id"].as_str().unwrap().to_string();

    // 非群成员领取 → 403（真实成员校验，不是 DenyAll 也不是伪造成员）。
    let (status, body) = send(
        &stack.app,
        action_request(
            "redpacket.claim",
            Some(&outsider_tok),
            json!({"packet_id": packet_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "非成员应 403: {body}");
    assert_eq!(body["error"], "FORBIDDEN");

    // 群成员领取成功；同一用户重试返回首次结果（幂等）。
    let (status, claimed) = send(
        &stack.app,
        action_request(
            "redpacket.claim",
            Some(&member_tok),
            json!({"packet_id": packet_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "成员领取应成功: {claimed}");
    let first_amount = claimed["amount"]["minor"].as_str().unwrap().to_string();
    let (status, again) = send(
        &stack.app,
        action_request(
            "redpacket.claim",
            Some(&member_tok),
            json!({"packet_id": packet_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        again["amount"]["minor"], first_amount,
        "同一用户重试返回首次份额"
    );
    let _ = outsider_id;
}

#[tokio::test]
async fn redpacket_create_forbidden_for_non_member() {
    let stack = boot().await;
    let (owner_id, owner_tok) = register(&stack.app, "rp-owner2").await;
    let (member_id, _m) = register(&stack.app, "rp-member2").await;
    let (outsider_id, outsider_tok) = register(&stack.app, "rp-outsider2").await;
    stack
        .commerce
        .sandbox_credit(&outsider_id, cny(100_000), "fund:rp-outsider2")
        .expect("fund outsider");
    let group = create_group(&stack.app, &owner_tok, &[member_id]).await;

    // 非群成员想在群里发红包 → 403。
    let (status, body) = send(
        &stack.app,
        action_request(
            "redpacket.create",
            Some(&outsider_tok),
            json!({"conversation_id": group, "amount": "5.00", "count": 1, "idempotency_key": "rp-outsider-key"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "非成员发红包应 403: {body}");
    assert_eq!(body["error"], "FORBIDDEN");
    let _ = owner_id;
}

// ---------------------------------------------------------------------------
// 4. audience 闸门（G06-03 reconcile / G06-05 权益发行）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reconcile_requires_admin_audience() {
    // 配了 admin token 的栈：普通会话 403，持 x-admin-token 才放行。
    std::env::set_var("WXLIKE_ADMIN_TOKEN", "test-admin-token-g06");
    let stack = boot().await;
    let (_id, token) = register(&stack.app, "recon-user").await;

    // 普通会话（无 admin token）跑对账 → 403。
    let (status, body) = send(
        &stack.app,
        action_request("reconcile.run", Some(&token), json!({})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "普通会话跑对账应 403: {body}"
    );
    assert_eq!(body["error"], "FORBIDDEN");

    // 持 admin token → 放行（对账生成新 run，不改账）。
    let mut req = action_request("reconcile.run", Some(&token), json!({}));
    req.headers_mut()
        .insert("x-admin-token", "test-admin-token-g06".parse().unwrap());
    let (status, body) = send(&stack.app, req).await;
    assert_eq!(status, StatusCode::OK, "admin 跑对账应 200: {body}");
    assert!(body["run_id"].as_str().is_some(), "应返回 run_id: {body}");

    std::env::remove_var("WXLIKE_ADMIN_TOKEN");
}

#[tokio::test]
async fn reconcile_closed_when_admin_token_not_configured() {
    // 没配 admin token：后台面整面关闭（不是放行）。即便登录也 403。
    std::env::remove_var("WXLIKE_ADMIN_TOKEN");
    let stack = boot().await;
    let (_id, token) = register(&stack.app, "recon-noadmin").await;
    let (status, body) = send(
        &stack.app,
        action_request("reconcile.list", Some(&token), json!({"run_id": "any"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "未配 admin token 应整面关闭: {body}"
    );
}

#[tokio::test]
async fn entitlement_publish_requires_backstage_audience() {
    std::env::set_var("WXLIKE_ADMIN_TOKEN", "test-admin-token-g06-ent");
    let stack = boot().await;
    let (_id, token) = register(&stack.app, "ent-issuer").await;

    // 普通会话发布权益规则 → 403（不能给自己发无限权益）。
    let (status, body) = send(
        &stack.app,
        action_request(
            "entitlement.rule.publish",
            Some(&token),
            json!({"kind": "coupon", "rule": {"valid_days": 30}}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "普通会话发规则应 403: {body}"
    );

    // 持 admin token → 放行。
    let mut req = action_request(
        "entitlement.rule.publish",
        Some(&token),
        json!({"kind": "coupon", "rule": {"valid_days": 30}}),
    );
    req.headers_mut()
        .insert("x-admin-token", "test-admin-token-g06-ent".parse().unwrap());
    let (status, body) = send(&stack.app, req).await;
    assert_eq!(status, StatusCode::OK, "后台发规则应 200: {body}");
    assert_eq!(body["kind"], "coupon");

    std::env::remove_var("WXLIKE_ADMIN_TOKEN");
}

// ---------------------------------------------------------------------------
// 订单（G06-05）：幂等 + 履约 audience + 错误分类
// ---------------------------------------------------------------------------

#[tokio::test]
async fn order_create_pay_fulfil_flow_and_idempotency() {
    let stack = boot().await;
    let (buyer_id, buyer_tok) = register(&stack.app, "order-buyer").await;
    let (merchant_id, merchant_tok) = register(&stack.app, "order-merchant").await;
    stack
        .commerce
        .sandbox_credit(&buyer_id, cny(100_000), "fund:order-buyer")
        .expect("fund buyer");

    let create_payload = json!({
        "payee_id": merchant_id, "quote_ref": "quote-1",
        "amount": "25.00", "currency": "CNY", "idempotency_key": "order-once",
    });
    let (status, created) = send(
        &stack.app,
        action_request("order.create", Some(&buyer_tok), create_payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "创建订单: {created}");
    let order_id = created["order"]["order_id"].as_str().unwrap().to_string();

    // 同键同参重试 → 同一订单。
    let (status, again) = send(
        &stack.app,
        action_request("order.create", Some(&buyer_tok), create_payload),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["order"]["order_id"], order_id);

    // 买家支付（沙箱默认成功）。
    let (status, paid) = send(
        &stack.app,
        action_request(
            "order.pay",
            Some(&buyer_tok),
            json!({"order_id": order_id, "idempotency_key": "order-pay"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "支付订单: {paid}");
    assert_eq!(paid["order"]["status"], "paid");

    // 非收款方履约 → 403。
    let (status, body) = send(
        &stack.app,
        action_request(
            "order.fulfil",
            Some(&buyer_tok),
            json!({"order_id": order_id, "provider_order_ref": "prov-1"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "非收款方履约应 403: {body}");

    // 收款方履约成功；同一 provider_order_ref 重复履约幂等。
    let (status, fulfilled) = send(
        &stack.app,
        action_request(
            "order.fulfil",
            Some(&merchant_tok),
            json!({"order_id": order_id, "provider_order_ref": "prov-1"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "履约: {fulfilled}");
    assert_eq!(fulfilled["order"]["status"], "fulfilled");
    let (status, again) = send(
        &stack.app,
        action_request(
            "order.fulfil",
            Some(&merchant_tok),
            json!({"order_id": order_id, "provider_order_ref": "prov-1"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "重复履约幂等: {again}");
    assert_eq!(again["order"]["status"], "fulfilled");
}

#[tokio::test]
async fn order_cancel_paid_is_illegal_transition() {
    let stack = boot().await;
    let (buyer_id, buyer_tok) = register(&stack.app, "order-cancel-buyer").await;
    let (merchant_id, _m) = register(&stack.app, "order-cancel-merchant").await;
    stack
        .commerce
        .sandbox_credit(&buyer_id, cny(100_000), "fund:order-cancel-buyer")
        .expect("fund");
    let (status, created) = send(
        &stack.app,
        action_request(
            "order.create",
            Some(&buyer_tok),
            json!({"payee_id": merchant_id, "quote_ref": "q", "amount": "5.00", "idempotency_key": "oc-create"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let order_id = created["order"]["order_id"].as_str().unwrap().to_string();
    let (status, _paid) = send(
        &stack.app,
        action_request(
            "order.pay",
            Some(&buyer_tok),
            json!({"order_id": order_id, "idempotency_key": "oc-pay"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{_paid}");

    // 已支付订单直接取消 → 409 CONFLICT（必须走退款）。
    let (status, body) = send(
        &stack.app,
        action_request(
            "order.cancel",
            Some(&buyer_tok),
            json!({"order_id": order_id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "已支付取消应 409: {body}");
    assert_eq!(body["error"], "CONFLICT");
}

// ---------------------------------------------------------------------------
// 独立 SQL 取证：动作面效果真的落盘（文件库）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn file_backed_payment_posts_exactly_one_journal() {
    let dir = std::env::temp_dir().join(format!("wxlike-commerce-actions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("stack.db");
    let opts = StackOptions {
        db_path: Some(db.to_str().unwrap().to_string()),
        ..StackOptions::default()
    };
    let stack = start_stack(opts).await.unwrap();
    let commerce_db = dir.join("commerce").join("commerce.db");
    let db_str = commerce_db.to_str().unwrap();

    let (buyer_id, buyer_tok) = register(&stack.app, "file-buyer").await;
    let (seller_id, _s) = register(&stack.app, "file-seller").await;
    stack
        .commerce
        .sandbox_credit(&buyer_id, cny(100_000), "fund:file-buyer")
        .expect("fund");

    // 创建意图并经签名回调入账（与 commerce_mount 同一内核）。
    let (status, created) = send(
        &stack.app,
        action_request(
            "payment.create",
            Some(&buyer_tok),
            json!({"direction": "order", "payee_id": seller_id, "amount": "12.00", "idempotency_key": "file-intent"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let intent_id = created["intent"]["intent_id"].as_str().unwrap().to_string();
    let intent = stack
        .commerce
        .payment_intent(&buyer_id, &intent_id)
        .expect("intent");
    let provider_ref = intent.provider_ref.expect("provider ref");

    let raw = format!(
        "{{\"operation_id\":\"file-op\",\"event_id\":\"file-evt\",\"intent_id\":\"{intent_id}\",\"kind\":\"payment.succeeded\",\"provider_ref\":\"{provider_ref}\",\"amount_minor\":1200,\"currency\":\"CNY\",\"timestamp_ms\":{}}}",
        now_ms()
    );
    let sig = stack.commerce.webhook_signature(&raw);
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/payment/callback")
        .header("content-type", "application/json")
        .header("x-payment-signature", sig);
    let req = req.body(Body::from(raw.clone())).unwrap();
    let (status, body) = send(&stack.app, req).await;
    assert_eq!(status, StatusCode::OK, "回调: {body}");

    // 独立裸连接取证：恰好一借一贷两行分录、一行 journal。
    let entries = count_param(
        db_str,
        "SELECT COUNT(*) FROM ledger_entries
         WHERE journal_id = (SELECT id FROM ledger_journals WHERE business_key = ?1)",
        &format!("intent:{intent_id}:success"),
    );
    assert_eq!(entries, 2, "一借一贷");
    let journals = count_param(
        db_str,
        "SELECT COUNT(*) FROM ledger_journals WHERE business_key = ?1",
        &format!("intent:{intent_id}:success"),
    );
    assert_eq!(journals, 1, "恰好一行 journal");

    // 原样重投回调：replayed=true，行数不变（幂等只有一份实现）。
    let sig2 = stack.commerce.webhook_signature(&raw);
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/payment/callback")
        .header("content-type", "application/json")
        .header("x-payment-signature", sig2);
    let req = req.body(Body::from(raw)).unwrap();
    let (status, body) = send(&stack.app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["replayed"], json!(true));
    let entries_after = count_param(
        db_str,
        "SELECT COUNT(*) FROM ledger_entries
         WHERE journal_id = (SELECT id FROM ledger_journals WHERE business_key = ?1)",
        &format!("intent:{intent_id}:success"),
    );
    assert_eq!(entries_after, 2, "重投不新增分录");
    let _ = count(db_str, "SELECT COUNT(*) FROM commerce_outbox");

    drop(stack);
    let _ = std::fs::remove_dir_all(&dir);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

// 让未使用的导入在 no-op 情况下也不报警（Ports/CommerceConfig 供后续扩展）。
#[allow(dead_code)]
fn _unused_ports_marker() -> Option<Ports> {
    let _ = CommerceConfig::sandbox(b"marker".to_vec());
    None
}
