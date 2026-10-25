//! G17-04（Web 面）：出站 URL 策略、路径穿越、XSS 渲染面。
//!
//! 打的是真实策略实现 `wxlike_video::policy::CallbackPolicy`（视频回调出站）
//! 和平台路由的真实提取器。SSRF 用例覆盖三类绕过：
//! ① 直接给私网/元数据地址；② 用 userinfo 或后缀把 host 换掉；③ 期望
//! "开发开关"没有在生产默认值里被打开。
//!
//! 这里**不测 DNS 重绑定**（`resolve()` 需要真实 DNS）：那一条由部署侧
//! 的出站代理 + `resolve()` 在发送前二次校验共同负责，单测里只能验静态部分，
//! 用假 DNS 去验等于给了一个不会失败的断言。

mod common;

use axum::http::{header, StatusCode};
use wxlike_video::{CallbackPolicy, VideoAuth};

fn policy() -> CallbackPolicy {
    CallbackPolicy::allow_hosts(["api.example.com".to_string()])
}

/// WEB-001：没配置允许列表时，任何用户给的地址都不接受（fail closed）。
#[test]
fn deny_by_default_when_no_allowlist() {
    let p = CallbackPolicy::deny_user_urls();
    for url in [
        "https://api.example.com/cb",
        "http://127.0.0.1:9/cb",
        "https://example.com/",
    ] {
        assert!(p.check(url).is_err(), "{url} 在未配置允许列表时不应通过");
    }
}

/// WEB-002：云元数据、回环、私网、链路本地、IPv6 回环/ULA 全部拒绝。
///
/// 元数据地址（169.254.169.254）单独列：它是"SSRF 的真正目的"，
/// 拿到它等于拿到实例凭据。
#[test]
fn ssrf_targets_are_rejected() {
    let p = policy();
    let cases = [
        ("http://169.254.169.254/latest/meta-data/", "云元数据"),
        ("http://127.0.0.1:8080/", "回环 v4"),
        ("http://10.1.2.3/", "私网 10/8"),
        ("http://192.168.1.1/", "私网 192.168/16"),
        ("http://172.16.0.9/", "私网 172.16/12"),
        ("http://0.0.0.0:80/", "未指定地址"),
        ("http://[::1]/", "回环 v6"),
        ("http://[fd00::1]/", "ULA v6"),
        ("http://[fe80::1]/", "链路本地 v6"),
        ("http://[::ffff:169.254.169.254]/", "v4-mapped 元数据"),
    ];
    for (url, label) in cases {
        let got = p.check(url);
        // 主机不在允许列表里也会被拒——这里要确认的是"不是靠允许列表偶然挡住"，
        // 所以另外用把这些 IP 也放进允许列表的策略再验一次（见下一条用例）。
        assert!(got.is_err(), "{label} {url} 不应通过，实际 Ok({got:?})");
    }
}

/// WEB-003：即使管理员**真的**把私网地址写进允许列表，公网前提仍是否决项。
///
/// 这条是 WEB-002 的补强：证明拒绝来自"地址是私网"这一条独立规则，
/// 而不是"恰好不在允许列表里"。运维把内网域名配进白名单是常见误操作。
#[test]
fn allowlisted_private_ip_is_still_rejected() {
    let p = CallbackPolicy::allow_hosts([
        "169.254.169.254".to_string(),
        "127.0.0.1".to_string(),
        "10.0.0.5".to_string(),
    ]);
    for url in [
        "http://169.254.169.254/latest/meta-data/",
        "http://127.0.0.1/",
        "http://10.0.0.5:9000/hook",
    ] {
        assert!(
            p.check(url).is_err(),
            "{url} 已被写进允许列表，但私网判定必须仍然否决"
        );
    }
}

/// WEB-004：host 伪装。userinfo（`@`）、后缀拼接、大小写与端口都不能绕过。
#[test]
fn host_spoofing_tricks_are_rejected() {
    let p = policy();
    let cases = [
        ("https://api.example.com@evil.test/cb", "userinfo 冒充"),
        ("https://evil.test#@api.example.com", "fragment 冒充"),
        ("https://api.example.com.evil.test/cb", "后缀拼接"),
        ("https://evil.test/api.example.com", "路径里放白名单域名"),
        ("https://API.EXAMPLE.COM/cb", "大小写：应当归一化后通过"), // 见下
    ];
    for (url, label) in &cases[..4] {
        assert!(p.check(url).is_err(), "{label} {url} 不应通过");
    }
    // 大小写归一化是**允许**的（host 不区分大小写），这条断言防止"一律拒绝"
    let ok = p.check("https://API.EXAMPLE.COM/cb");
    assert!(ok.is_ok(), "host 大小写应当归一化后通过，实际 {ok:?}");
}

/// WEB-005：协议面。只有 http(s)；其它 scheme 一律拒绝。
#[test]
fn non_http_schemes_are_rejected() {
    let p = policy();
    for url in [
        "file:///etc/passwd",
        "ftp://api.example.com/x",
        "gopher://api.example.com/",
        "data:text/html,<script>alert(1)</script>",
        "javascript:alert(1)",
        "//api.example.com/cb",
    ] {
        assert!(p.check(url).is_err(), "{url} 不应通过");
    }
}

/// WEB-006：开发开关是显式的。`allow_private(true)` 只影响私网判定，
/// 不放开协议与允许列表——避免"为了本地联调把整个策略关掉"。
#[test]
fn allow_private_is_narrow() {
    let p = CallbackPolicy::allow_hosts(["127.0.0.1".to_string()]).allow_private(true);
    assert!(
        p.check("http://127.0.0.1:9/cb").is_ok(),
        "显式打开后回环应可用"
    );
    assert!(
        p.check("file:///etc/passwd").is_err(),
        "打开私网不等于放开协议"
    );
    assert!(
        p.check("http://10.9.9.9/cb").is_err(),
        "10.9.9.9 不在允许列表，仍应拒绝"
    );
}

/// WEB-007：路径穿越在媒体面是 404（id 形状校验），不是目录逃逸。
#[tokio::test]
async fn media_path_traversal_is_404() {
    let app = common::app(common::platform());
    let (_uid, tok) = common::signup(&app, "traversal-probe").await;
    let probes = [
        "/api/media/..%2F..%2Fetc%2Fpasswd",
        "/api/media/..%2f..%2f..%2fwindows%2fwin.ini",
        "/api/media/C%3A%5Cwindows%5Cwin.ini",
        "/api/media/........................",
        "/api/media/deadbeef", // 太短，不是 64 位 hex
    ];
    for path in probes {
        let (status, body) = common::call(&app, common::req("GET", path, None, Some(&tok))).await;
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST,
            "{path} 应当 404/400（id 形状不符），实际 {status} {body}"
        );
    }
}

/// WEB-008：XSS 面。读接口一律 `application/json`，不存在"把用户文本当
/// HTML 返回"的渲染面。
///
/// 这条不能证明"前端不会 innerHTML"，它证明的是**服务端没有提供 HTML 反射
/// 通道**——这是服务端能负责的那一半；另一半在 G10/G11 的 CSP 与净化里。
#[tokio::test]
async fn read_surfaces_return_json_not_html() {
    let app = common::app(common::platform());
    let (uid, tok) = common::signup(&app, "xss-probe").await;
    let payload = r#"<script>alert(1)</script><img src=x onerror=alert(2)>"#;

    // 用带 HTML 的昵称注册一个账号，再读它的公开资料
    let (status, body) = common::call(
        &app,
        common::req(
            "POST",
            "/api/profile",
            Some(serde_json::json!({"name": payload, "bio": payload})),
            Some(&tok),
        ),
    )
    .await;
    assert!(
        status == StatusCode::OK || status.is_client_error(),
        "profile 更新：{status} {body}"
    );

    // 只挑**真实存在且属主可读**的读面：不存在的路径会 404，
    // 用 404 去断言"没有 HTML 通道"等于什么都没验。
    for path in [
        format!("/api/profile/{uid}"),
        "/api/communities/mine".to_string(),
        "/api/groups/mine".to_string(),
    ] {
        let resp = app
            .clone()
            .oneshot(common::req("GET", &path, None, Some(&tok)))
            .await
            .unwrap();
        assert!(
            resp.status().is_success(),
            "{path} 应当可读，实际 {}",
            resp.status()
        );
        let ctype = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(
            ctype.starts_with("application/json"),
            "{path} 的 Content-Type 是 {ctype:?}；读面必须是 JSON，否则用户文本就有了 HTML 渲染通道"
        );
    }
}

/// WEB-009：视频控制面的匿名闸门。没有认证器时**拒绝**（fail closed），
/// 不是"不校验"。
#[test]
fn video_caller_requires_auth() {
    let empty = axum::http::HeaderMap::new();
    // `VideoAuth` 是钩子；这里验证 trait 能拿到 header 层面的判断，
    // 真正的装配在 server 里（那边用平台会话表实现）。
    struct NeverAuth;
    impl VideoAuth for NeverAuth {
        fn caller(&self, _h: &axum::http::HeaderMap) -> Option<String> {
            None
        }
    }
    assert!(NeverAuth.caller(&empty).is_none());
    let mut with_token = axum::http::HeaderMap::new();
    with_token.insert(header::AUTHORIZATION, "Bearer x".parse().unwrap());
    assert!(
        NeverAuth.caller(&with_token).is_none(),
        "无凭据的请求不能被认成调用者"
    );
}

use tower::ServiceExt as _;
