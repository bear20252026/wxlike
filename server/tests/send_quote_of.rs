//! #331 第 1 件：`POST /api/conv/{conv}/send` 透传 `quote_of`（引用回复）。
//!
//! 打的是 `Stack::app` —— 与监听端口上跑的是同一个 Router（同一条
//! `HubHandle::submit` 入站路径），所以"引用真的落库"不是 fixture 里的
//! 自说自话，而是真实写路径的结论。读回用 `GET /api/history/{conv}`，
//! 走的是信箱 + 归档那条真读路径。
//!
//! 四条底线：
//! 1. 合法引用 ⇒ 从 history 能读回 `quote_of` 指向被引消息；
//! 2. 非法引用（形状不对 / 不存在 / 别的会话 / 已撤回）⇒ 明确 4xx，
//!    **而且一条消息也不落库**（校验在 `hub.submit` 之前）；
//! 3. 不带引用 ⇒ 与今天完全一致：`quote_of` 为 null，同键重放仍是同一条；
//! 4. 同一幂等键换引用重放 ⇒ 409（对齐钱包侧"同键异参"），库里仍是第一次那条。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_server::{start_stack, StackOptions};

async fn boot() -> wxlike_server::Stack {
    // StackOptions::default() 用内存库；sidecar 目录名带 pid+序号自增，
    // 并行测试不会撞 Tantivy 的索引锁。
    start_stack(StackOptions::default())
        .await
        .expect("stack 起不来")
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.expect("router");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, body)
}

fn request(method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    match body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .expect("request"),
        None => builder.body(Body::empty()).expect("request"),
    }
}

/// 真注册：走平台 `/api/register`，token 来自 `sessions` 表。
async fn register(app: &Router, name: &str) -> (String, String) {
    let (status, reply) = send(
        app,
        request(
            "POST",
            "/api/register",
            None,
            Some(json!({ "name": name, "password": "pw" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register {name}: {reply}");
    (
        reply["user_id"].as_str().expect("user_id").to_string(),
        reply["token"].as_str().expect("token").to_string(),
    )
}

/// 加好友：直聊的投递策略按关系过滤，先把关系做真。
async fn befriend(app: &Router, a_tok: &str, b_uid: &str, b_tok: &str) {
    let (status, body) = send(
        app,
        request(
            "POST",
            "/api/friends/requests",
            Some(a_tok),
            Some(json!({ "to_user_id": b_uid, "message": "" })),
        ),
    )
    .await;
    assert!(status.is_success(), "friend request: {status} {body}");
    let (status, incoming) = send(
        app,
        request("GET", "/api/friends/requests/incoming", Some(b_tok), None),
    )
    .await;
    assert!(status.is_success(), "incoming: {status} {incoming}");
    let id = incoming[0]["id"].as_str().expect("request id").to_string();
    let (status, body) = send(
        app,
        request(
            "POST",
            &format!("/api/friends/requests/{id}/accept"),
            Some(b_tok),
            None,
        ),
    )
    .await;
    assert!(status.is_success(), "accept: {status} {body}");
}

fn direct(a: &str, b: &str) -> String {
    wxlike_platform::direct_conv_id(a, b)
}

/// 一次 `POST /api/conv/{conv}/send`。
async fn post_send(app: &Router, token: &str, conv: &str, body: Value) -> (StatusCode, Value) {
    send(
        app,
        request(
            "POST",
            &format!("/api/conv/{conv}/send"),
            Some(token),
            Some(body),
        ),
    )
    .await
}

/// 会话现状（history 的真实读路径）。
async fn history(app: &Router, token: &str, conv: &str) -> Vec<Value> {
    let (status, page) = send(
        app,
        request(
            "GET",
            &format!("/api/history/{conv}?limit=50"),
            Some(token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "history: {page}");
    page["messages"].as_array().expect("messages 数组").clone()
}

/// 轮询等 N 条消息可见：镜像信箱与归档写是异步的，不能假设同步。
async fn wait_rows(app: &Router, token: &str, conv: &str, n: usize) -> Vec<Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let rows = history(app, token, conv).await;
        if rows.len() >= n {
            return rows;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "会话 {conv} 始终只有 {} 条（期望 >= {n}）",
            rows.len()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn find_by_text(rows: &[Value], text: &str) -> Value {
    for row in rows {
        if row["text"] == text {
            return row.clone();
        }
    }
    panic!("history 里没有正文为 {text} 的消息：{rows:?}");
}

/// 一对已加好友的用户 + 他们的直聊会话。
async fn pair(app: &Router, tag: &str) -> (String, String, String) {
    let (a, ta) = register(app, &format!("{tag}-a")).await;
    let (b, tb) = register(app, &format!("{tag}-b")).await;
    befriend(app, &ta, &b, &tb).await;
    (direct(&a, &b), ta, tb)
}

// ---------------------------------------------------------------------------

/// 合法引用 ⇒ 落库并且能从 history 读回引用关系。
///
/// 证明的是"引用真的写进了线格式 + 归档"，不是 handler 自己把请求体回显
/// 一遍：断言读的是另一条路径（信箱排序 + 归档正文 → `message_json`）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legal_quote_of_round_trips_through_the_store() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (conv, ta, tb) = pair(&app, "quote-ok").await;

    let (status, first) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "原话", "idempotency_key": "q1-src"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "send: {first}");
    let src_id = first["msg_id"].as_str().expect("msg_id").to_string();

    let (status, reply) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "引用一句", "idempotency_key": "q1-reply", "quote_of": src_id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "send with quote: {reply}");

    // 读回方用收件人：两条都要在
    let rows = wait_rows(&app, &tb, &conv, 2).await;
    let quoted = find_by_text(&rows, "引用一句");
    assert_eq!(
        quoted["quote_of"], src_id,
        "引用关系没落库：{quoted}（全体 {rows:?}）"
    );
    assert_eq!(
        find_by_text(&rows, "原话")["quote_of"],
        Value::Null,
        "被引用的原话不该被顺手写上引用"
    );

    // 单条读接口同样看得见这条引用（同一个 `message_json`，但走的是 msg_id 主键）
    let (status, one) = send(
        &app,
        request(
            "GET",
            &format!(
                "/api/conv/{conv}/msg/{}",
                reply["msg_id"].as_str().expect("id")
            ),
            Some(&ta),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "one_message: {one}");
    assert_eq!(one["quote_of"], src_id, "单条读回丢了引用：{one}");

    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 回归：**不带 `quote_of` 时行为与今天一致**。
///
/// 不带引用 ⇒ `quote_of` 为 null、响应体仍是 `{msg_id, conv_id, accepted}`
/// 三个键（没多一个"引用"字段去改变老客户端看到的形状）；同键重放 ⇒ 同
/// msg_id、库里仍只有一条。这条测试是本卡的"不许动老行为"闸门。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_without_quote_of_is_unchanged() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (conv, _ta, tb) = pair(&app, "quote-none").await;

    let (status, first) = post_send(
        &app,
        &tb,
        &conv,
        json!({"text": "老规矩", "idempotency_key": "plain-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "send: {first}");
    let keys = first.as_object().expect("对象").keys().collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec!["accepted", "conv_id", "msg_id"],
        "不带引用的响应体形状变了 = 动到既有客户端：{first}"
    );

    // 空串按"不带引用"处理（口径与 idempotency_key 一致）
    let (status, empty) = post_send(
        &app,
        &tb,
        &conv,
        json!({"text": "空串引用", "idempotency_key": "plain-2", "quote_of": ""}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "quote_of=\"\" 该当没带：{empty}");

    let (status, again) = post_send(
        &app,
        &tb,
        &conv,
        json!({"text": "老规矩", "idempotency_key": "plain-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "同键重放: {again}");
    assert_eq!(
        first["msg_id"], again["msg_id"],
        "同 key 必须同 msg_id（幂等派生不能被改动）"
    );

    let rows = history(&app, &tb, &conv).await;
    assert_eq!(rows.len(), 2, "重放不该变成两条：{rows:?}");
    for row in &rows {
        assert_eq!(
            row["quote_of"],
            Value::Null,
            "不带引用的行必须没有引用：{row}"
        );
    }

    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 形状不对的 `quote_of` ⇒ 400，且不落库。
///
/// 覆盖"字符集不是 hex""太长（32 字节）""奇数个 hex 字符"三种，都不该被
/// 收下来变成一条没有引用的普通消息。另有"缺省 / null / 全空白"三种写法
/// 与"不带引用"同义（口径与本文件 `idempotency_key` 一致），一并钉住。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_quote_of_is_400_and_stores_nothing() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (conv, ta, _tb) = pair(&app, "quote-shape").await;

    // 一条锚点消息：既能当"基线只有一条"的参照，也保证会话是真能写的
    let (status, anchor) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "锚点", "idempotency_key": "shape-anchor"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "anchor: {anchor}");
    wait_rows(&app, &ta, &conv, 1).await;

    let good = anchor["msg_id"].as_str().expect("msg_id").to_string();
    let good_len = good.len();
    let bad_shape = vec![
        json!("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"), // 长度对，字符集不对
        json!(good.repeat(2)),                     // 太长：64 个 hex = 32 字节
        json!(&good[..good_len - 1]),              // 少一个字符（奇数长度）
        json!("0123456789abcdef0123456789abcde"),  // 30 个字符 = 15 字节
    ];
    for case in &bad_shape {
        let (status, body) = post_send(
            &app,
            &ta,
            &conv,
            json!({"text": "坏引用", "idempotency_key": "shape-bad", "quote_of": case}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "形状不对的 quote_of={case} 必须 400：{body}"
        );
    }
    // `null`、全空白与"字段缺省"同义：不带引用，不该被当成形状错误
    for (label, quote) in [("null", json!(null)), ("全空白", json!("  "))] {
        let (status, body) = post_send(
            &app,
            &ta,
            &conv,
            json!({"text": "无引用", "idempotency_key": "shape-none", "quote_of": quote}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "quote_of={label} 该当没带：{body}");
    }
    // 两次"当没带"的共用一个键 ⇒ 同一条消息（顺带钉住幂等派生没被改动）
    let (status, again) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "无引用", "idempotency_key": "shape-none", "quote_of": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "同键重放: {again}");

    // 4 次坏请求一条也没落库：库里只有锚点 + 那条"无引用"
    let rows = wait_rows(&app, &ta, &conv, 2).await;
    assert_eq!(rows.len(), 2, "非法引用不该产生行：{rows:?}");
    assert!(
        rows.iter().all(|r| r["text"] != "坏引用"),
        "坏引用居然落库了：{rows:?}"
    );

    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 被引用的消息不存在 ⇒ 404，且不落库。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_quote_of_is_404_and_stores_nothing() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (conv, ta, _tb) = pair(&app, "quote-missing").await;

    let (status, anchor) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "锚点", "idempotency_key": "missing-anchor"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "anchor: {anchor}");
    wait_rows(&app, &ta, &conv, 1).await;

    // 形状完全合法、但全库没有这个 msg_id
    let ghost = "0123456789abcdef0123456789abcdef";
    let (status, body) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "引用幽灵", "idempotency_key": "missing-1", "quote_of": ghost}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "不存在的 quote_of 必须 404：{body}"
    );

    // `submit` 里"归档 + 镜像信箱"都是同步完成的，所以响应一回来就能判定：
    // 条数没涨 = 确实没落库，不靠 sleep。
    let rows = history(&app, &ta, &conv).await;
    assert_eq!(rows.len(), 1, "404 之后库里必须一条也不多：{rows:?}");

    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 被引用的消息属于**别的会话** ⇒ 404，且不落库。
///
/// 两种都要拒：
/// * 同一个人自己的另一个会话（A 把 B 会话里的 msg_id 引用进 C 会话）；
/// * 调用方根本读不到的会话（别人的直聊）——回 404 而不是 403，
///   否则 msg_id 会变成跨会话的存在性探针（口径与 `one_message` 一致）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quote_of_from_another_conversation_is_404_and_stores_nothing() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (alice, ta) = register(&app, "quote-xconv-a").await;
    let (bob, tb) = register(&app, "quote-xconv-b").await;
    let (carol, tc) = register(&app, "quote-xconv-c").await;
    befriend(&app, &ta, &bob, &tb).await;
    befriend(&app, &ta, &carol, &tc).await;
    let ab = direct(&alice, &bob);
    let ac = direct(&alice, &carol);
    // bob~carol 的会话：alice 不是参与者，读不到
    befriend(&app, &tb, &carol, &tc).await;
    let bc = direct(&bob, &carol);

    // alice 自己另一个会话里的消息
    let (status, in_ac) = post_send(
        &app,
        &ta,
        &ac,
        json!({"text": "另一个会话的原话", "idempotency_key": "x-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "send ac: {in_ac}");
    let foreign_own = in_ac["msg_id"].as_str().expect("msg_id").to_string();
    wait_rows(&app, &ta, &ac, 1).await;

    // 别人会话里、alice 读不到的消息
    let (status, in_bc) = post_send(
        &app,
        &tb,
        &bc,
        json!({"text": "别人的原话", "idempotency_key": "x-2"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "send bc: {in_bc}");
    let foreign_other = in_bc["msg_id"].as_str().expect("msg_id").to_string();

    // 先钉住"alice 读不到别人会话的那条"：读面在鉴权层就 403
    // （`one_message` 先 authorize_conv 再比会话），
    // 而**引用**面不同：目标会话是她的、被引消息在别的会话 ⇒ 404，
    // 不给 403，免得 msg_id 变成跨会话的存在性探针。两条口径各有出处，
    // 这里都钉下来，别让"反正都拒"糊掉差别。
    let (status, body) = send(
        &app,
        request(
            "GET",
            &format!("/api/conv/{bc}/msg/{foreign_other}"),
            Some(&ta),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "读别人会话: {body}");

    for (label, id) in [
        ("自己的另一会话", &foreign_own),
        ("别人的会话", &foreign_other),
    ] {
        let (status, reply) = post_send(
            &app,
            &ta,
            &ab,
            json!({"text": "跨会话引用", "idempotency_key": "x-3", "quote_of": id}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "引用{label}的消息（{id}）必须 404：{reply}"
        );
    }

    let rows = history(&app, &ta, &ab).await;
    assert_eq!(rows.len(), 0, "两次跨会话引用必须一条也不写：{rows:?}");
    // 被引用的两个会话也没被"顺手"改动
    assert_eq!(history(&app, &ta, &ac).await.len(), 1, "ac 会话被改动");
    assert_eq!(history(&app, &tb, &bc).await.len(), 1, "bc 会话被改动");

    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 被引用的消息**已撤回** ⇒ 409（与 `forward` 对源消息同一口径），不落库。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn withdrawn_quote_of_is_409_and_stores_nothing() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (conv, ta, _tb) = pair(&app, "quote-recalled").await;

    let (status, sent) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "将被撤回", "idempotency_key": "rc-src"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "send: {sent}");
    let id = sent["msg_id"].as_str().expect("msg_id").to_string();
    wait_rows(&app, &ta, &conv, 1).await;

    let (status, body) = send(
        &app,
        request(
            "POST",
            &format!("/api/conv/{conv}/recall"),
            Some(&ta),
            Some(json!({"msg_id": id})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "recall: {body}");

    let (status, reply) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "引用一条已撤回", "idempotency_key": "rc-reply", "quote_of": id}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "已撤回的 quote_of 必须 409：{reply}"
    );

    let rows = history(&app, &ta, &conv).await;
    assert_eq!(rows.len(), 1, "撤回后引用它必须不写新行：{rows:?}");

    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 同一幂等键**换引用**重放 ⇒ 409，库里保持第一次那条。
///
/// 书面结论见 `server/src/lib.rs` 的 `check_replay_matches` 文档：msg_id 由
/// `key + user + conv` 派生，同键异参必然撞到同一条消息，而 hub 的去重是
/// "先到先赢、后到静默丢弃"——照旧回 200 就是假成功，所以对齐钱包侧
/// `crates/platform/src/wallet.rs` 的"同键异参 → Conflict"。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_key_with_different_quote_of_is_409() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (conv, ta, tb) = pair(&app, "quote-idem").await;

    let (status, s1) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "原话一", "idempotency_key": "i-src1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "s1: {s1}");
    let (status, s2) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "原话二", "idempotency_key": "i-src2"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "s2: {s2}");
    let q1 = s1["msg_id"].as_str().expect("q1").to_string();
    let q2 = s2["msg_id"].as_str().expect("q2").to_string();

    // 第一次：键 K + 引用 q1 → 成功
    let (status, first) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "重放测试", "idempotency_key": "K", "quote_of": q1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "首次带引用: {first}");
    let row_id = first["msg_id"].as_str().expect("msg_id").to_string();

    // 同键 + 不同引用 → 409
    let (status, body) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "重放测试", "idempotency_key": "K", "quote_of": q2}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "同键换引用必须 409，不是静默吞掉：{body}"
    );

    // 同键 + 换正文（引用不变）→ 同样 409
    let (status, body) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "重放测试改了正文", "idempotency_key": "K", "quote_of": q1}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "同键换正文: {body}");

    // 同键 + 去掉引用 → 也是"异参"
    let (status, body) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "重放测试", "idempotency_key": "K"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "同键去掉引用: {body}");

    // 同键 + 完全相同的参数 → 合法重放，200，msg_id 不变，库里还是一条
    let (status, replay) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "重放测试", "idempotency_key": "K", "quote_of": q1}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "同参重放必须照旧 200: {replay}");
    assert_eq!(
        replay["msg_id"], row_id,
        "同参重放的 msg_id 必须与第一次一致"
    );

    let rows = wait_rows(&app, &tb, &conv, 3).await;
    assert_eq!(
        rows.len(),
        3,
        "三条原话 + 一条引用 = 3 条，重放与 409 都不写行：{rows:?}"
    );
    let quoted = find_by_text(&rows, "重放测试");
    assert_eq!(quoted["msg_id"], row_id, "第一次那条被改写了：{quoted}");
    assert_eq!(quoted["quote_of"], q1, "引用关系必须保持第一次的 q1");

    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 反方向：先用键 K 发一条**不带引用**的，再用 K 带引用重放 ⇒ 409。
///
/// "同键异参"是两个方向的：第一次没引用、第二次有，同样是异参。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adding_a_quote_to_an_existing_idempotent_key_is_409() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (conv, ta, _tb) = pair(&app, "quote-idem2").await;

    let (status, src) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "可被引用的原话", "idempotency_key": "i2-src"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "src: {src}");
    let q = src["msg_id"].as_str().expect("msg_id").to_string();

    let (status, plain) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "先不带引用", "idempotency_key": "K2"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "首次不带引用（老行为）: {plain}");

    let (status, body) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "先不带引用", "idempotency_key": "K2", "quote_of": q}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "同键后来加引用也是异参: {body}"
    );

    let rows = wait_rows(&app, &ta, &conv, 2).await;
    assert_eq!(rows.len(), 2, "409 不该写行：{rows:?}");
    for row in rows.iter().filter(|r| r["text"] == "先不带引用") {
        assert_eq!(row["quote_of"], Value::Null, "老行被改动: {row}");
    }

    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 鉴权先于引用校验：不是会话参与者连自己的引用机会都没有。
///
/// 这条是防"为了校验引用而把 authorize_conv 挪到后面"的回归闸。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outsider_send_with_quote_of_is_still_403() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (alice, ta) = register(&app, "quote-auth-a").await;
    let (bob, tb) = register(&app, "quote-auth-b").await;
    let (_carol, tc) = register(&app, "quote-auth-c").await;
    befriend(&app, &ta, &bob, &tb).await;
    let conv = direct(&alice, &bob);

    let (status, src) = post_send(
        &app,
        &ta,
        &conv,
        json!({"text": "原话", "idempotency_key": "auth-src"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "src: {src}");
    let id = src["msg_id"].as_str().expect("msg_id").to_string();

    // 坏 token 先 401：认证在引用之前，和今天一样
    let (status, body) = post_send(
        &app,
        "obviously-not-a-session",
        &conv,
        json!({"text": "闯入者", "idempotency_key": "auth-bad", "quote_of": id}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "坏 token: {body}");

    // carol 注册了但不在 alice~bob 的会话里：拿她自己的 token 打这个会话
    let (status, body) = post_send(
        &app,
        &tc,
        &conv,
        json!({"text": "闯入者", "idempotency_key": "auth-bad", "quote_of": id}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "非参与者带引用必须还是 403（鉴权先于引用校验）：{body}"
    );

    let rows = wait_rows(&app, &tb, &conv, 1).await;
    assert_eq!(rows.len(), 1, "被拒的请求一条也不该写：{rows:?}");

    stack.shutdown(std::time::Duration::from_secs(2)).await;
}
