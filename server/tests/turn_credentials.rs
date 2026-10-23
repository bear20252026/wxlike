//! T-D1 / #31 客户端半边：TURN 短期凭据（coturn REST）。
//!
//! 这里钉的是桌面端所依赖的服务端契约：
//! * 登录态才能取，凭据带到期时间；
//! * 重新签发 = 新用户名（内含新的到期秒）与新密码；
//! * 没配 `WXLIKE_TURN_SECRET` 时路由不挂载（404），客户端据此走无 TURN 降级；
//! * 配了 secret 但没配 `WXLIKE_TURN_URL` 时 `uris` 为空，客户端同样降级。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use hmac::{Hmac, Mac as _};
use sha1::Sha1;
use wxlike_server::{start_stack, StackOptions};

const SECRET: &str = "t-d1-turn-secret";

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        // 回环测试不依赖宿主机代理设置（Windows 上 ureq 会读注册表系统代理）。
        .proxy(None)
        .build()
        .into()
}

fn register(http: &str, name: &str) -> (String, String) {
    let mut resp = agent()
        .post(&format!("{http}/api/register"))
        .send_json(serde_json::json!({"name": name, "password": "pw"}))
        .unwrap_or_else(|e| panic!("register {name}: {e}"));
    assert_eq!(resp.status().as_u16(), 200);
    let body: serde_json::Value = resp.body_mut().read_json().unwrap();
    (
        body["user_id"].as_str().unwrap().to_string(),
        body["token"].as_str().unwrap().to_string(),
    )
}

fn get_credentials(http: &str, token: Option<&str>) -> (u16, serde_json::Value) {
    let mut req = agent().get(&format!("{http}/api/turn/credentials"));
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let mut resp = req.call().unwrap_or_else(|e| panic!("GET credentials: {e}"));
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_json()
        .unwrap_or(serde_json::Value::Null);
    (status, body)
}

fn expected_password(secret: &str, username: &str) -> String {
    let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes()).expect("HMAC key");
    mac.update(username.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

// 同步的 ureq 调用会阻塞当前线程；单线程 runtime 会把服务端 task 一起卡死。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn turn_credentials_are_short_lived_and_absent_without_secret() {
    std::env::set_var("WXLIKE_TURN_SECRET", SECRET);
    std::env::set_var("WXLIKE_TURN_URL", "turn:127.0.0.1:3478");
    let stack = start_stack(StackOptions::default()).await.unwrap();
    let http = format!("http://{}", stack.http_addr);
    let (user_id, token) = register(&http, "turn_user");

    let (status, _) = get_credentials(&http, None);
    assert_eq!(status, 401, "没有 Bearer 必须拒绝");

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let (status, body) = get_credentials(&http, Some(&token));
    assert_eq!(status, 200);
    let username = body["username"].as_str().unwrap();
    let password = body["password"].as_str().unwrap();
    assert_eq!(body["ttl_seconds"].as_u64(), Some(3_600));
    assert_eq!(body["uris"], serde_json::json!(["turn:127.0.0.1:3478"]));
    assert!(username.ends_with(&format!(":{user_id}")));
    let expiry: u64 = username.split_once(':').unwrap().0.parse().unwrap();
    assert!(
        expiry >= now + 3_598 && expiry <= now + 3_602,
        "用户名里的到期时间应为 now+ttl：expiry={expiry} now={now}"
    );
    assert_eq!(password, expected_password(SECRET, username));

    // 刷新：重新签发会得到新的到期秒与对应密码。
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let (status, refreshed) = get_credentials(&http, Some(&token));
    assert_eq!(status, 200);
    let refreshed_name = refreshed["username"].as_str().unwrap();
    assert_ne!(refreshed_name, username);
    assert_eq!(
        refreshed["password"].as_str().unwrap(),
        expected_password(SECRET, refreshed_name)
    );

    // 没配 secret：路由不挂载，桌面端据此走直连。
    std::env::remove_var("WXLIKE_TURN_SECRET");
    let stack2 = start_stack(StackOptions::default()).await.unwrap();
    let http2 = format!("http://{}", stack2.http_addr);
    let (_, token2) = register(&http2, "turn_user2");
    let (status, _) = get_credentials(&http2, Some(&token2));
    assert_eq!(status, 404);

    // 配了 secret 但没配 URL：能签发，uris 为空，客户端同样降级。
    std::env::set_var("WXLIKE_TURN_SECRET", SECRET);
    std::env::remove_var("WXLIKE_TURN_URL");
    let stack3 = start_stack(StackOptions::default()).await.unwrap();
    let http3 = format!("http://{}", stack3.http_addr);
    let (_, token3) = register(&http3, "turn_user3");
    let (status, body3) = get_credentials(&http3, Some(&token3));
    assert_eq!(status, 200);
    assert_eq!(body3["uris"], serde_json::json!([]));
}
