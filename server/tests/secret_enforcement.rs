//! 秘密聊天的**服务端**兜底（issue #15）。
//!
//! 桌面端早就有"秘密会话不可转发""消失消息到期不下发"的拦截，但 HTTP 控制面
//! 是另一条入口。客户端自觉不能当安全边界——这里钉住的是服务端自己守的那半边：
//!
//! 1. 秘密会话的消息不能转发出去，普通消息也不能转进秘密会话；
//! 2. 消失消息到期后，历史**和搜索**都不能再返回它（只过滤历史的话，
//!    "消失"只完成了一半）。

use std::time::Duration;

use wxlike_net::Client;
use wxlike_server::{start_stack, StackOptions};

struct TestStack {
    http: String,
    quic: std::net::SocketAddr,
    hub: wxlike_net::HubHandle,
}

async fn boot() -> TestStack {
    let stack = start_stack(StackOptions::default()).await.unwrap();
    TestStack {
        http: format!("http://{}", stack.http_addr),
        quic: stack.quic_addr,
        hub: stack.hub.clone(),
    }
}

fn who() -> String {
    std::thread::current()
        .name()
        .unwrap_or("t")
        .chars()
        .take(12)
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect()
}

fn register(stack: &TestStack, name: &str) -> (String, String) {
    let reply: serde_json::Value = ureq::post(&format!("{}/api/register", stack.http))
        .send_json(serde_json::json!({"name": name, "password": "pw"}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    (
        reply["user_id"].as_str().unwrap().to_string(),
        reply["token"].as_str().unwrap().to_string(),
    )
}

/// ureq 3：状态码不当错误（管理面/风控面大量用 4xx 表达业务结果），
/// 超时集中在这里配。
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .into()
}

fn get(stack: &TestStack, token: &str, path: &str) -> (u16, serde_json::Value) {
    let mut resp = agent()
        .get(&format!("{}{path}", stack.http))
        .header("Authorization", &format!("Bearer {token}"))
        .call()
        .unwrap_or_else(|e| panic!("GET {path}: {e}"));
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_json()
        .unwrap_or(serde_json::Value::Null);
    (status, body)
}

fn put(
    stack: &TestStack,
    token: &str,
    path: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    match agent()
        .put(&format!("{}{path}", stack.http))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(body)
    {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let body = resp
                .body_mut()
                .read_json()
                .unwrap_or(serde_json::Value::Null);
            (status, body)
        }
        Err(err) => panic!("PUT {path}: {err}"),
    }
}

fn post(
    stack: &TestStack,
    token: &str,
    path: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    match agent()
        .post(&format!("{}{path}", stack.http))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(body)
    {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let body = resp
                .body_mut()
                .read_json()
                .unwrap_or(serde_json::Value::Null);
            (status, body)
        }
        Err(err) => panic!("POST {path}: {err}"),
    }
}

/// 同 `post`，但 body 原样返回：这个端点的错误是**纯文本**（`(Status, String)`），
/// 不是统一形状的 JSON，硬解析只会得到 null。
fn post_raw(stack: &TestStack, token: &str, path: &str, body: serde_json::Value) -> (u16, String) {
    match agent()
        .post(&format!("{}{path}", stack.http))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(body)
    {
        Ok(mut resp) => (
            resp.status().as_u16(),
            resp.body_mut().read_to_string().unwrap_or_default(),
        ),
        Err(err) => panic!("POST {path}: {err}"),
    }
}

/// hub 的 Hello 只认**令牌**（issue #3）：传 user_id 会被认证器拒掉，
/// 连接永远绑不上，测试就挂在 wait_bound 里。
async fn connect(stack: &TestStack, token: &str) -> Client {
    Client::connect(stack.quic, "localhost", token)
        .await
        .unwrap()
}

/// P3 陌生人闸门：非好友之间直聊发不出去，所以要先加好友。
async fn befriend(stack: &TestStack, a_tok: &str, b_uid: &str, b_tok: &str) {
    ureq::post(&format!("{}/api/friends/requests", stack.http))
        .header("Authorization", &format!("Bearer {a_tok}"))
        .send_json(serde_json::json!({"to_user_id": b_uid, "message": ""}))
        .unwrap();
    let incoming: serde_json::Value =
        ureq::get(&format!("{}/api/friends/requests/incoming", stack.http))
            .header("Authorization", &format!("Bearer {b_tok}"))
            .call()
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap();
    let id = incoming[0]["id"].as_str().unwrap().to_string();
    ureq::post(&format!("{}/api/friends/requests/{id}/accept", stack.http))
        .header("Authorization", &format!("Bearer {b_tok}"))
        .send_empty()
        .unwrap();
}

async fn wait_bound(stack: &TestStack, n: usize) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while stack.hub.online_users().await < n {
        assert!(
            std::time::Instant::now() < deadline,
            "hub never bound {n} users"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn conv(a: &str, b: &str) -> String {
    wxlike_platform::direct_conv_id(a, b)
}

/// 往一个会话里塞一条内容消息（走 hub 的入站路径，与 QUIC 帧一致）。
async fn send_text(
    stack: &TestStack,
    from: &str,
    from_token: &str,
    conv_id: &str,
    to: Vec<String>,
    text: &str,
) {
    let client = connect(stack, from_token).await;
    wait_bound(stack, 1).await;
    let env = wxlike_core::Envelope::direct(conv_id, from, to, 1, text);
    client.send(&env).await.unwrap();
    // 传输缓冲时间不代表异步索引已提交；查询断言须另等可观察的就绪状态。
    tokio::time::sleep(Duration::from_millis(400)).await;
}

/// 搜索索引由后台线程提交；固定 sleep 在 Windows runner 上不是就绪保证。
async fn wait_for_search_hit(stack: &TestStack, token: &str, path: &str) -> serde_json::Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (status, results) = get(stack, token, path);
        assert_eq!(status, 200, "搜索请求失败：{results}");
        let hits = results["results"]
            .as_array()
            .expect("搜索响应必须包含 results 数组");
        if !hits.is_empty() {
            return results;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "等待搜索索引可见超时：{results}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_secret_message_cannot_be_forwarded_out() {
    let stack = boot().await;
    let (alice, alice_token) = register(&stack, &format!("{}_a", who()));
    let (bob, bob_token) = register(&stack, &format!("{}_b", who()));
    let (carol, _carol_token) = register(&stack, &format!("{}_c", who()));

    let secret = conv(&alice, &bob);
    let plain = conv(&alice, &carol);

    befriend(&stack, &alice_token, &bob, &bob_token).await;
    send_text(
        &stack,
        &alice,
        &alice_token,
        &secret,
        vec![bob.clone()],
        "只在两人之间",
    )
    .await;

    // alice 把秘密会话打开
    let (status, _) = put(
        &stack,
        &alice_token,
        &format!("/api/secret/{secret}"),
        serde_json::json!({"enabled": true, "ttl_ms": 60 * 60 * 1000}),
    );
    assert_eq!(status, 200, "开启秘密会话");

    // 转发出去 → 403
    let (status, body) = post_raw(
        &stack,
        &alice_token,
        &format!("/api/conv/{plain}/forward"),
        serde_json::json!({
            "to_conv": plain,
            "from_conv": secret,
            "msg_id": hex::encode([7u8; 16]),
            "idempotency_key": "fwd-1",
        }),
    );
    assert_eq!(status, 403, "秘密会话的消息不能转发：{body}");
    assert!(body.contains("secret"), "错误要说清原因：{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_normal_message_cannot_be_forwarded_into_a_secret_conv() {
    let stack = boot().await;
    let (alice, alice_token) = register(&stack, &format!("{}_a", who()));
    let (bob, _bob_token) = register(&stack, &format!("{}_b", who()));
    let (carol, carol_token) = register(&stack, &format!("{}_c", who()));

    let secret = conv(&alice, &bob);
    let plain = conv(&alice, &carol);

    befriend(&stack, &alice_token, &carol, &carol_token).await;
    send_text(
        &stack,
        &alice,
        &alice_token,
        &plain,
        vec![carol.clone()],
        "普通会话里的话",
    )
    .await;
    let (status, _) = put(
        &stack,
        &alice_token,
        &format!("/api/secret/{secret}"),
        serde_json::json!({"enabled": true, "ttl_ms": 0}),
    );
    assert_eq!(status, 200, "开启秘密会话");

    let (status, body) = post(
        &stack,
        &alice_token,
        &format!("/api/conv/{secret}/forward"),
        serde_json::json!({
            "to_conv": secret,
            "from_conv": plain,
            "msg_id": hex::encode([9u8; 16]),
            "idempotency_key": "fwd-2",
        }),
    );
    assert_eq!(status, 403, "不能往秘密会话里转发：{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_secret_messages_leave_both_history_and_search() {
    let stack = boot().await;
    let (alice, alice_token) = register(&stack, &format!("{}_a", who()));
    let (bob, bob_token) = register(&stack, &format!("{}_b", who()));
    let secret = conv(&alice, &bob);

    // 会话先开秘密 + 最短 TTL（30 秒）
    let (status, _) = put(
        &stack,
        &alice_token,
        &format!("/api/secret/{secret}"),
        serde_json::json!({"enabled": true, "ttl_ms": 30 * 1000}),
    );
    assert_eq!(status, 200);

    befriend(&stack, &alice_token, &bob, &bob_token).await;
    send_text(
        &stack,
        &alice,
        &alice_token,
        &secret,
        vec![bob.clone()],
        "会被搜到的字",
    )
    .await;

    // 先确认索引可见，再断言到期前后行为；不假设 400ms 足够后台 commit。
    let results = wait_for_search_hit(&stack, &alice_token, "/api/search/messages?q=会被搜到").await;
    assert_eq!(
        results["results"].as_array().map(Vec::len),
        Some(1),
        "到期前应该能搜到：{results}"
    );

    // 到期（等 30 秒 TTL 过去）——这是这条用例唯一慢的地方
    tokio::time::sleep(Duration::from_millis(31_000)).await;

    let (status, results) = get(&stack, &alice_token, "/api/search/messages?q=会被搜到");
    assert_eq!(status, 200);
    assert!(
        results["results"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "到期后搜索不能再返回：{results}"
    );

    let (status, history) = get(
        &stack,
        &alice_token,
        &format!("/api/history/{secret}?limit=50"),
    );
    assert_eq!(status, 200);
    assert!(
        history["messages"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "到期后历史也不能再返回：{history}"
    );
}
