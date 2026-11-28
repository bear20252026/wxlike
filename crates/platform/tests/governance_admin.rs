//! G02-09 账户治理 HTTP 面的验收测试（集成请求 REQ-G00-governance-http-mount）。
//!
//! 走真实 axum 路由（tower::oneshot）+ **文件库**平台：治理镜像端口的第二条
//! 连接只有文件库才有（`domain::governance`），所以这里的断言覆盖镜像列的
//! 真实落库。Admin 操作者令牌经 sidecar 连接用 identity 域 API 签发
//! （`Audience::Admin`，与生产 adminx 的信任模型一致）。
//!
//! 钉住的验收面：
//! 1. 封禁 → 回执形状/会话撤销非零/users 镜像落库/旧令牌失效/再登录 403；
//! 2. 解封恢复访问，重复解封幂等（already=true）；
//! 3. 普通用户令牌进不来（受众分离）、缺头 401；
//! 4. 临时封禁到期惰性恢复（不依赖后台清扫）；
//! 5. 群治理 freeze/unfreeze 回执 + 幂等重放；
//! 6. 元数据视图白名单投影 + 跨域计数（群数/举报数）。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_identity::{session, Audience};
use wxlike_platform::{Platform, PlatformOptions};

/// 文件库平台（tempfile 目录随测试进程清理）。
fn file_platform() -> (Router, String) {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-gov-test-{}-{}",
        std::process::id(),
        wxlike_platform::random_hex(6)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("wxlike.db");
    let path = db_path.to_str().unwrap().to_string();
    let opts = PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        ..Default::default()
    };
    let platform = Platform::open_with(Some(&path), opts).unwrap().shared();
    (Platform::router(platform), path)
}

/// sidecar 连接：与平台主库同文件（治理端口外的第三条连接，仅测试用）。
fn sidecar(path: &str) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA busy_timeout = 5000;").unwrap();
    conn
}

/// 设备先注册再开会话（identity 域的设备是一等实体，会话挂在设备下）。
fn ensure_device(conn: &rusqlite::Connection, user_id: &str, device_id: &str) {
    let reg = wxlike_identity::device::Registration {
        platform: "desktop".to_string(),
        label: "管理端".to_string(),
        os_version: "win11".to_string(),
        app_version: "1.0.0".to_string(),
        identity_key: format!("key-{device_id}"),
    };
    wxlike_identity::device::register(
        conn,
        user_id,
        device_id,
        &reg,
        wxlike_core::types::now_ms() as i64,
    )
    .unwrap();
}

/// 经 identity 域 API 签发 Admin audience 操作者会话。
fn admin_session(conn: &rusqlite::Connection, user_id: &str) -> String {
    ensure_device(conn, user_id, "device-admin0001");
    let grant = session::open(
        conn,
        user_id,
        "device-admin0001",
        Audience::Admin,
        60_000,
        "127.0.0.1",
        wxlike_core::types::now_ms() as i64,
    )
    .unwrap();
    grant.access_token
}

async fn call(app: &Router, r: Request<Body>) -> (StatusCode, Value) {
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

fn req(method: &str, path: &str, body: Option<Value>, token: Option<&str>) -> Request<Body> {
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

/// 管理端请求：x-admin-token = Admin 会话令牌（非 Bearer）。
fn admin_req(path: &str, body: Value, operator_token: &str) -> Request<Body> {
    let mut r = req("POST", path, Some(body), None);
    r.headers_mut()
        .insert("x-admin-token", operator_token.parse().unwrap());
    r
}

async fn register(app: &Router, name: &str) -> String {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/register",
            Some(json!({"name": name, "password": "pw"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register {name}: {body}");
    body["user_id"].as_str().unwrap().to_string()
}

async fn login(app: &Router, name: &str) -> Result<String, StatusCode> {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/login",
            Some(json!({"name": name, "password": "pw"})),
            None,
        ),
    )
    .await;
    if status == StatusCode::OK {
        Ok(body["token"].as_str().unwrap().to_string())
    } else {
        Err(status)
    }
}

#[tokio::test]
async fn suspend_revokes_sessions_blocks_login_and_mirrors() {
    let (app, path) = file_platform();
    let victim = register(&app, "gov-victim").await;
    let operator_id = register(&app, "gov-operator").await;
    let conn = sidecar(&path);
    let operator_token = admin_session(&conn, &operator_id);

    // 封禁前的正常访问。
    let victim_tok = login(&app, "gov-victim").await.expect("victim login");

    let (status, receipt) = call(
        &app,
        admin_req(
            &format!("/api/admin/identity/users/{victim}/suspend"),
            json!({"reason": "spam", "actor": "ops", "trace": "t-1"}),
            &operator_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suspend: {receipt}");
    assert_eq!(receipt["already"], false, "首次封禁非幂等命中: {receipt}");
    assert_eq!(receipt["after"]["state"], "suspended", "{receipt}");
    let revoked = receipt["sessions_revoked"].as_u64().unwrap();
    assert!(revoked >= 1, "封禁应撤销至少一个会话: {receipt}");
    assert!(
        receipt["reference"].as_str().unwrap().starts_with("gov:"),
        "{receipt}"
    );

    // 旧令牌立即失效（域内删 sessions；平台的 token 查找走同一张表）。
    let (status, body) = call(&app, req("GET", "/api/me", None, Some(&victim_tok))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "旧令牌应失效: {body}");

    // 再登录 403（治理闸门，非 401——凭据是对的，账号不再活跃）。
    let again = login(&app, "gov-victim").await;
    assert_eq!(
        again.err().unwrap(),
        StatusCode::FORBIDDEN,
        "封禁后登录应 403"
    );

    // users 镜像列真实落库（第二条连接读同一文件）。
    let mirrored: (i64, i64, Option<i64>) = conn
        .query_row(
            "SELECT suspended, disabled, suspended_until_ms FROM users WHERE id = ?1",
            [&victim],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(mirrored, (1, 0, None), "永久封禁镜像: {mirrored:?}");
}

#[tokio::test]
async fn unsuspend_restores_access_and_replays_idempotently() {
    let (app, path) = file_platform();
    let victim = register(&app, "gov-unsus").await;
    let operator_id = register(&app, "gov-operator2").await;
    let conn = sidecar(&path);
    let operator_token = admin_session(&conn, &operator_id);

    let target = format!("/api/admin/identity/users/{victim}");
    let (status, r1) = call(
        &app,
        admin_req(
            &format!("{target}/suspend"),
            json!({"reason": "review", "actor": "ops"}),
            &operator_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r1}");

    let (status, r2) = call(
        &app,
        admin_req(
            &format!("{target}/unsuspend"),
            json!({"reason": "cleared", "actor": "ops"}),
            &operator_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unsuspend: {r2}");
    assert_eq!(r2["after"]["state"], "active", "{r2}");

    // 访问恢复。
    login(&app, "gov-unsus").await.expect("解封后应能登录");

    // 重复解封：幂等命中（already=true），不产生二次效果。
    let (status, r3) = call(
        &app,
        admin_req(
            &format!("{target}/unsuspend"),
            json!({"reason": "cleared", "actor": "ops"}),
            &operator_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r3}");
    assert_eq!(r3["already"], true, "重复解封应幂等命中: {r3}");

    // 镜像列收敛到解封态。
    let suspended: i64 = conn
        .query_row(
            "SELECT suspended FROM users WHERE id = ?1",
            [&victim],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(suspended, 0, "解封后镜像应归零");
}

#[tokio::test]
async fn suspend_requires_admin_audience() {
    let (app, path) = file_platform();
    let victim = register(&app, "gov-noadmin-target").await;
    let intruder = register(&app, "gov-intruder").await;
    let conn = sidecar(&path);

    // 普通用户会话（Audience::User）当操作者令牌 → 401。
    ensure_device(&conn, &intruder, "device-intruder1");
    let grant = session::open(
        &conn,
        &intruder,
        "device-intruder1",
        Audience::User,
        60_000,
        "127.0.0.1",
        wxlike_core::types::now_ms() as i64,
    )
    .unwrap();
    let (status, body) = call(
        &app,
        admin_req(
            &format!("/api/admin/identity/users/{victim}/suspend"),
            json!({"reason": "abuse", "actor": "intruder"}),
            &grant.access_token,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "普通用户令牌进不来: {body}"
    );

    // 缺头 → 401，且不暴露 body 契约。
    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/admin/identity/users/{victim}/suspend"),
            Some(json!({"reason": "abuse", "actor": "x"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "缺头应 401: {body}");

    // 目标状态未被上述任何尝试改动。
    let state: String = conn
        .query_row(
            "SELECT state FROM identity_user_governance WHERE user_id = ?1",
            [&victim],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| "absent".to_string());
    assert!(
        matches!(state.as_str(), "absent" | "active"),
        "越权尝试不应改变治理状态: {state}"
    );
}

#[tokio::test]
async fn temporary_suspend_expires_lazily() {
    let (app, path) = file_platform();
    let victim = register(&app, "gov-temp").await;
    let operator_id = register(&app, "gov-operator3").await;
    let conn = sidecar(&path);
    let operator_token = admin_session(&conn, &operator_id);

    let until = wxlike_core::types::now_ms() as i64 + 8_000;
    let (status, receipt) = call(
        &app,
        admin_req(
            &format!("/api/admin/identity/users/{victim}/suspend"),
            json!({"reason": "cooldown", "actor": "ops", "until_ms": until}),
            &operator_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{receipt}");
    assert_eq!(receipt["after"]["suspended_until_ms"], until, "{receipt}");

    // 到期前登录被拒（登录链路自身要几百毫秒，到期窗口留 8s，避免慢机器上
    // 在"到期前"断言之前就已经过期）。
    let before = login(&app, "gov-temp").await;
    assert_eq!(before.err().unwrap(), StatusCode::FORBIDDEN);

    // 直接把到期时间拨到过去：惰性判定只读这一行，不依赖后台清扫，也不用等墙钟。
    conn.execute(
        "UPDATE identity_user_governance SET suspended_until_ms = ?1 WHERE user_id = ?2",
        rusqlite::params![wxlike_core::types::now_ms() as i64 - 1_000, &victim],
    )
    .unwrap();
    login(&app, "gov-temp")
        .await
        .expect("临时封禁到期应自动放行");
}

#[tokio::test]
async fn group_govern_freeze_unfreeze_and_idempotent_replay() {
    let (app, path) = file_platform();
    let _owner = register(&app, "gov-owner").await;
    let operator_id = register(&app, "gov-operator4").await;
    let conn = sidecar(&path);
    let operator_token = admin_session(&conn, &operator_id);

    // 建群（owner 会话经普通登录）。
    let owner_tok = login(&app, "gov-owner").await.unwrap();
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/groups",
            Some(json!({"name": "gov-group", "member_ids": []})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create group: {body}");
    let group_id = body["group_id"].as_str().unwrap().to_string();

    let target = format!("/api/admin/identity/groups/{group_id}/govern");
    let freeze_body = json!({"reason": "raided", "actor": "ops", "action": "freeze"});

    let (status, r1) = call(
        &app,
        admin_req(&target, freeze_body.clone(), &operator_token),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "freeze: {r1}");
    assert_eq!(r1["after"]["state"], "frozen", "{r1}");

    // 幂等重放：同一动作再来一次 → already=true。
    let (status, r2) = call(&app, admin_req(&target, freeze_body, &operator_token)).await;
    assert_eq!(status, StatusCode::OK, "{r2}");
    assert_eq!(r2["already"], true, "重复 freeze 应幂等: {r2}");

    let (status, r3) = call(
        &app,
        admin_req(
            &target,
            json!({"reason": "review done", "actor": "ops", "action": "unfreeze"}),
            &operator_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "unfreeze: {r3}");
    assert_eq!(r3["after"]["state"], "active", "{r3}");

    // 未知动作 → 422，稳定分类。
    let (status, body) = call(
        &app,
        admin_req(
            &target,
            json!({"reason": "x", "actor": "ops", "action": "detonate"}),
            &operator_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "未知动作: {body}");
}

#[tokio::test]
async fn device_and_group_current_views_are_read_from_the_server() {
    let (app, path) = file_platform();
    let victim = register(&app, "gov-current").await;
    let operator_id = register(&app, "gov-operator-view").await;
    let conn = sidecar(&path);
    let operator_token = admin_session(&conn, &operator_id);

    // 设备视图：管理端自己那台设备；不能把 identity_key 带出来。
    let mut get_device = req(
        "GET",
        "/api/admin/identity/devices/device-admin0001",
        None,
        None,
    );
    get_device
        .headers_mut()
        .insert("x-admin-token", operator_token.parse().unwrap());
    let (status, device) = call(&app, get_device).await;
    assert_eq!(status, StatusCode::OK, "device view: {device}");
    assert_eq!(device["device_id"], "device-admin0001", "{device}");
    assert_eq!(device["user_id"], operator_id, "{device}");
    assert!(!device.to_string().contains("identity_key"), "{device}");

    // 群视图：创建后读 active，冻结后同一 GET 必须读到 frozen。
    let owner = register(&app, "gov-current-owner").await;
    let owner_tok = login(&app, "gov-current-owner").await.unwrap();
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/groups",
            Some(json!({"name": "current-group", "member_ids": [victim]})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create group: {body}");
    let group_id = body["group_id"].as_str().unwrap().to_string();

    let mut get_group = req(
        "GET",
        &format!("/api/admin/identity/groups/{group_id}"),
        None,
        None,
    );
    get_group
        .headers_mut()
        .insert("x-admin-token", operator_token.parse().unwrap());
    let (status, group) = call(&app, get_group).await;
    assert_eq!(status, StatusCode::OK, "group view: {group}");
    assert_eq!(group["group_id"], group_id, "{group}");
    assert_eq!(group["state"], "active", "{group}");
    assert_eq!(group["owner_uid"], owner, "{group}");

    let (status, r) = call(
        &app,
        admin_req(
            &format!("/api/admin/identity/groups/{group_id}/govern"),
            json!({"reason": "current-state", "actor": "ops", "action": "freeze"}),
            &operator_token,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "freeze: {r}");
    let mut get_group_after = req(
        "GET",
        &format!("/api/admin/identity/groups/{group_id}"),
        None,
        None,
    );
    get_group_after
        .headers_mut()
        .insert("x-admin-token", operator_token.parse().unwrap());
    let (status, group) = call(&app, get_group_after).await;
    assert_eq!(status, StatusCode::OK, "group view after freeze: {group}");
    assert_eq!(group["state"], "frozen", "{group}");
}

#[tokio::test]
async fn metadata_view_is_whitelist_projection_with_counts() {
    let (app, path) = file_platform();
    let victim = register(&app, "gov-viewed").await;
    let _reporter = register(&app, "gov-reporter").await;
    let _owner = register(&app, "gov-gowner").await;
    let operator_id = register(&app, "gov-operator5").await;
    let conn = sidecar(&path);
    let operator_token = admin_session(&conn, &operator_id);

    // 举报一次（target_kind=user）→ report_count=1。
    let reporter_tok = login(&app, "gov-reporter").await.unwrap();
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/reports",
            Some(json!({"target_kind": "user", "target_id": victim, "reason": "spam"})),
            Some(&reporter_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "report: {body}");

    // 拉群 → group_count=1。
    let owner_tok = login(&app, "gov-gowner").await.unwrap();
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/groups",
            Some(json!({"name": "view-group", "member_ids": [victim]})),
            Some(&owner_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "group: {body}");

    let mut get_view = req(
        "GET",
        &format!("/api/admin/identity/users/{victim}"),
        None,
        None,
    );
    get_view
        .headers_mut()
        .insert("x-admin-token", operator_token.parse().unwrap());
    let (status, view) = call(&app, get_view).await;
    assert_eq!(status, StatusCode::OK, "view: {view}");
    assert_eq!(view["user_id"], victim, "{view}");
    assert_eq!(view["state"], "active", "{view}");
    assert_eq!(view["group_count"], 1, "{view}");
    assert_eq!(view["report_count"], 1, "{view}");
    // 白名单投影：绝不含口令/消息正文类字段。
    let text = view.to_string();
    assert!(!text.contains("pw_hash"), "{view}");
    assert!(!text.contains("body"), "{view}");
}
