//! adminx 集成测试的共享脚手架。
//!
//! 每个 `tests/*.rs` 各编译一份本模块、用其中不同的子集，所以"没用到"是
//! 预期而不是 dead code（与 `crates/platform/tests/common` 同一做法）。
//!
//! 没有 mock：平台库是真的（跑平台自己的迁移）、HTTP 是真的（`tower::oneshot`
//! 驱 adminx 的路由；转发面驱真 socket），留痕是真的落库。合同 §0.3 禁止
//! 以 mock 交差，而后台最容易出问题的恰恰是"看起来通了、其实没到平台"。

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_admin::rbac::Role;
use wxlike_admin::{App, Config, PlatformRead};
use wxlike_platform::{Platform, PlatformOptions, PlatformState};

static SEQ: AtomicU64 = AtomicU64::new(0);

pub const ADMIN_TOKEN: &str = "read-token-0123456789abcdef";
pub const PLATFORM_TOKEN: &str = "platform-admin-token-xyz";
pub const PW: &str = "a-sufficiently-long-pass";

pub struct TempDir {
    pub path: PathBuf,
}

impl TempDir {
    pub fn create(tag: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("wxlike-admin-it-{}-{tag}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 一个跑着真实迁移的平台库。`None` 路径 = 平台用内存库（只服务 HTTP 面）。
pub fn platform(path: &Path) -> PlatformState {
    let opts = PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        admin_token: Some(PLATFORM_TOKEN.to_string()),
        ..Default::default()
    };
    Platform::open_with(Some(path.to_str().unwrap()), opts)
        .unwrap()
        .shared()
}

pub struct Seed {
    pub alice: String,
    pub bob: String,
    pub group: String,
    pub report_id: String,
    pub held_id: String,
}

/// 造一组运营真的会看的数据：用户、群、命令审计、待办工单、挂起内容、钱包。
pub fn seed(p: &PlatformState) -> Seed {
    let alice = p.register("alice", "pw").unwrap();
    let bob = p.register("bob", "pw").unwrap();
    let carol = p.register("carol", "pw").unwrap();
    let dave = p.register("dave", "pw").unwrap();
    let group = p
        .create_group(&alice, "研发大群", &[bob.clone(), carol])
        .unwrap();
    // 命令审计的形状（`audit_mw.rs`）：action = "cmd.<id>"
    p.audit(
        &alice,
        "cmd.group.create",
        "/api/groups",
        "status=200 method=POST",
    );
    p.audit(
        &bob,
        "cmd.msg.send",
        "/api/conv/g-1/send",
        "status=200 method=POST",
    );
    p.audit(
        &bob,
        "cmd.msg.send",
        "/api/conv/g-1/send",
        "status=200 method=POST",
    );
    p.audit(&alice, "user.login", &alice, "ok");
    p.fund(&alice, 5_000, "seed").unwrap();
    p.transfer(&alice, &bob, 1_200, "午饭").unwrap();
    // 举报人用 dave：不自举报，形状更接近真实工单
    let ticket = p
        .create_report(&dave, "user", &bob, wxlike_report::Reason::Spam, "广告轰炸")
        .unwrap();
    // 先审后发：这条会进 moderation_queue（平台的默认模式就是 PreReview）
    let verdict = p
        .moderate_for_send(&bob, &format!("d-{alice}~{bob}"), "加我微信领红包")
        .unwrap();
    let held_id = verdict
        .queued_id
        .clone()
        .expect("软违规必须先审后发，进挂起队列");
    Seed {
        alice,
        bob,
        group,
        report_id: ticket.id,
        held_id,
    }
}

pub fn cfg(platform_db: PathBuf, accounts_db: PathBuf, platform_url: String) -> Config {
    Config {
        platform_db,
        accounts_db,
        token: ADMIN_TOKEN.to_string(),
        platform_token: PLATFORM_TOKEN.to_string(),
        platform_url,
        bind: "127.0.0.1:0".to_string(),
        report_sla_ms: 60_000,
        session_ttl_ms: 60_000,
        shutdown_grace: std::time::Duration::from_secs(1),
    }
}

/// 只用**只读面**的装配（不需要平台进程）。
pub fn readonly_app(platform_db: PathBuf, accounts_db: PathBuf) -> std::sync::Arc<App> {
    let read = PlatformRead::open(&platform_db).expect("只读打开平台库");
    let accounts = wxlike_admin::AccountStore::open(&accounts_db).unwrap();
    App::assemble(
        read,
        accounts,
        cfg(platform_db, accounts_db, "http://127.0.0.1:1".into()),
    )
}

/// 起一个真的平台 HTTP 服务，返回 base URL。
pub async fn serve_platform(p: PlatformState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, Platform::router(p)).await;
    });
    // 等监听真正可用（第一次 connect 成功即可，别用固定 sleep）
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    format!("http://{addr}")
}

pub fn req(method: &str, path: &str, body: Option<Value>, token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b.body(match body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    })
    .unwrap()
}

pub async fn call(app: &Router, r: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(r).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, body)
}

/// 建一个管理员并登录，返回会话令牌。`mfa=false` 时账号不绑 TOTP，
/// 令牌自然没有写权限。
pub fn login(app: &App, name: &str, role: Role) -> String {
    app.accounts().add(name, PW, role).unwrap();
    let (session, token) = app
        .accounts()
        .login(name, PW, None, app.config().session_ttl_ms, "")
        .unwrap();
    assert_eq!(session.role, role);
    token
}

/// 当前动态码。参数必须与平台 `totp_verify` 逐字一致（SHA1 / 6 位 / skew 1 / 30s），
/// 否则"验码通过"是假通过。
pub fn current_totp_code(secret_b32: &str) -> String {
    let totp = totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        6,
        1,
        30,
        totp_rs::Secret::Encoded(secret_b32.to_string())
            .to_bytes()
            .unwrap(),
    )
    .unwrap();
    totp.generate_current().unwrap()
}

/// 建号 → 绑 TOTP → 激活 → 带码登录，返回**有写权限**的会话令牌。
/// 走完真实流程，不是给测试开后门。
pub fn login_mfa(app: &App, name: &str, role: Role) -> String {
    app.accounts().add(name, PW, role).unwrap();
    let secret = app.accounts().totp_enroll(name).unwrap();
    let code = current_totp_code(&secret);
    app.accounts().totp_confirm(name, &code).unwrap();
    let (session, token) = app
        .accounts()
        .login(name, PW, Some(&code), app.config().session_ttl_ms, "")
        .unwrap();
    assert!(session.mfa, "带对了码，会话必须是 mfa=true");
    assert_eq!(session.role, role);
    token
}

/// 装配一个"平台进程活着"的 adminx：返回临时目录、平台状态、adminx、平台 base。
pub async fn live(tag: &str) -> (TempDir, PlatformState, Arc<App>, String) {
    let dir = TempDir::create(tag);
    let db = dir.join("wxlike.db");
    let p = platform(&db);
    seed(&p);
    let base = serve_platform(p.clone()).await;
    let read = PlatformRead::open(&db).expect("平台在线时应能严格只读打开");
    assert_eq!(read.open_mode(), "read_only");
    let accounts = wxlike_admin::AccountStore::open(&dir.join("adminx.db")).unwrap();
    let app = wxlike_admin::App::assemble(
        read,
        accounts,
        cfg(db.clone(), dir.join("adminx.db"), base.clone()),
    );
    (dir, p, app, base)
}

pub fn json_body(fields: Value) -> Value {
    json!(fields)
}

/// 目录里取一条（测试用它按目录声明造请求体，而不是硬编码字段）。
pub fn rbac_get(id: &str) -> &'static wxlike_admin::rbac::Op {
    wxlike_admin::rbac::get(id).unwrap_or_else(|| panic!("目录里没有 {id}"))
}

/// 在**当前 tokio 运行时**里做一次阻塞的 HTTP（平台端点）。
/// 用 `spawn_blocking` 而不是直接调 ureq：`#[tokio::test]` 默认是单线程运行时，
/// 测试任务一阻塞，平台服务就没人应答 —— 表现为一条永远跑不完的用例（假死，不是假绿）。
/// 返回 `(状态码, JSON 正文)`；非 2xx 也读正文，平台的结论就写在那里。
pub async fn http_platform(
    method: &str,
    base: &str,
    path: &str,
    body: Option<Value>,
    bearer_token: Option<&str>,
    command: Option<&str>,
    admin_token: Option<&str>,
) -> (u16, Value) {
    let url = format!("{}{}", base.trim_end_matches('/'), path);
    let method = method.to_string();
    let bearer_token = bearer_token.map(|s| s.to_string());
    let command = command.map(|s| s.to_string());
    let admin_token = admin_token.map(|s| s.to_string());
    tokio::task::spawn_blocking(move || {
        let mut req = match method.as_str() {
            "GET" => ureq::get(&url),
            "POST" => ureq::post(&url),
            "PUT" => ureq::put(&url),
            other => panic!("测试脚手架不支持 {other}"),
        }
        .timeout(std::time::Duration::from_secs(15));
        if let Some(t) = &bearer_token {
            req = req.set("authorization", &format!("Bearer {t}"));
        }
        if let Some(c) = &command {
            req = req.set("x-wxlike-command", c);
        }
        if let Some(a) = &admin_token {
            req = req.set("x-admin-token", a);
        }
        let sent = match body {
            Some(v) => req.send_json(v),
            None => req.call(),
        };
        match sent {
            Ok(resp) => {
                let status = resp.status();
                (status, resp.into_json::<Value>().unwrap_or(Value::Null))
            }
            Err(ureq::Error::Status(status, resp)) => {
                (status, resp.into_json::<Value>().unwrap_or(Value::Null))
            }
            Err(err) => (0, json!({"error": err.to_string()})),
        }
    })
    .await
    .expect("阻塞任务不该 panic")
}

pub async fn post_platform(
    base: &str,
    path: &str,
    body: Value,
    bearer_token: Option<&str>,
    command: Option<&str>,
) -> (u16, Value) {
    http_platform("POST", base, path, Some(body), bearer_token, command, None).await
}

pub async fn ask_platform(base: &str, path: &str, token: &str) -> Value {
    http_platform("GET", base, path, None, None, None, Some(token))
        .await
        .1
}
