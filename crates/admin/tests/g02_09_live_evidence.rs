//! G02-09 live evidence（handover 第 2 条）：adminx 的 `platform_token` 打真平台。
//!
//! 两条鉴权路径的关系，这条用例把它钉住：
//!
//! * 平台 `/api/admin/identity/*`（`identity_admin.rs`）用 `x-admin-token`，
//!   值是 identity 域签发的 **Admin audience 会话令牌**——域内
//!   `session::authenticate(.., Audience::Admin)` 做服务端二次鉴权；
//! * adminx `Config::platform_token` 就是发给这个头的值（`Upstream::call`）。
//!
//! 所以"platform_token 换 Admin 会话令牌"不是可选优化，而是这条治理链路的准入
//! 条件：静态 `admin_token`（`/api/admin/reports` 那套 `AdminAuth`）在这里一律
//! 401。下面的负向对照把它证出来。
//!
//! 平台用**文件库**（`tests/common::platform`），因为治理镜像端口的第二条连接
//! 只有文件库才有（`domain::governance`）；adminx 侧复用 `App::assemble`，
//! 它装的 `G14::with_http` 与生产 `App::open` 走同一个 `Upstream::new`。

#[path = "common/mod.rs"]
mod common;

use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use wxlike_admin::rbac::Role;
use wxlike_admin::{AdminError, App, PlatformRead, Upstream};
use wxlike_platform::PlatformState;

/// 在平台库里写一条 **Admin audience** 会话行。
///
/// 生产由 `wxlike_identity::session::open(.., Audience::Admin, ..)` 写这行
/// （`crates/platform/tests/governance_admin.rs::admin_session` 即此路径）。
/// 这里不新增 dev-dependency（不动 Cargo.toml / Cargo.lock），改用平台自己
/// 导出的 `random_hex` / `token_digest` 按同一张 `sessions` 表形状写：token 存
/// 摘要、`audience='admin'`、`sid/device_id` 留空。`authenticate` 对空 sid /
/// 空设备显式跳过令牌侧车与设备校验，所以这行在鉴权上与真实 Admin 会话等效。
fn admin_session_token(db: &Path, user_id: &str) -> String {
    let access = wxlike_platform::random_hex(32);
    let digest = wxlike_platform::token_digest(&access);
    let sid = wxlike_platform::random_hex(16);
    let now = wxlike_core::types::now_ms() as i64;
    let conn = rusqlite::Connection::open(db).expect("打开平台库 sidecar 连接");
    conn.execute_batch("PRAGMA busy_timeout = 5000;").unwrap();
    // H-34：有效的访问会话必须同时有 identity_session_meta 侧车行；只写
    // `sessions` 的老 fixture 会被 fail-closed 挡成 401。serial/auth_level
    // 也要对得上：管理面要求 Root，serial 跟当前令牌序列一致。
    let serial: i64 = conn
        .query_row(
            "SELECT COALESCE((SELECT serial FROM identity_token_serial WHERE id = 1), 0)",
            [],
            |row| row.get(0),
        )
        .expect("读取当前令牌序列");
    conn.execute(
        "INSERT INTO sessions(token, user_id, created_at, expires_at, sid, device_id, \
         device_name, ip, last_seen, audience) \
         VALUES(?1, ?2, ?3, ?4, ?5, '', '', '', ?3, 'admin')",
        rusqlite::params![digest, user_id, now, now + 60_000, sid],
    )
    .expect("写入 Admin audience 会话行");
    conn.execute(
        "INSERT INTO identity_session_meta( \
             sid, user_id, auth_level, features, serial, issued_at, expires_at \
         ) VALUES(?1, ?2, 'root', 'VL', ?3, ?4, ?5)",
        rusqlite::params![sid, user_id, serial, now, now + 60_000],
    )
    .expect("写入 Admin 会话侧车行");
    access
}

/// 跑着真迁移 + 真 HTTP 的文件库平台。
struct Live {
    dir: common::TempDir,
    db: std::path::PathBuf,
    platform: PlatformState,
    base: String,
}

async fn live(tag: &str) -> Live {
    let dir = common::TempDir::create(tag);
    let db = dir.join("wxlike.db");
    let platform = common::platform(&db);
    let base = common::serve_platform(platform.clone()).await;
    Live {
        dir,
        db,
        platform,
        base,
    }
}

/// 注册一个用户并真登录一次，返回它的会话令牌（封禁的验收面就是这条会话被撤掉）。
async fn login(base: &str, name: &str) -> String {
    let (status, body) = common::post_platform(
        base,
        "/api/login",
        json!({"name": name, "password": "pw"}),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "登录 {name}: {body}");
    body["token"].as_str().unwrap().to_string()
}

/// 建号 → 绑 TOTP → 激活 → 带码登录，返回（后台会话令牌, TOTP 密钥）。
/// 与 `tests/common::login_mfa` 同一流程，额外回传密钥供 step-up 用。
fn mfa_session(app: &Arc<App>, name: &str, role: Role) -> (String, String) {
    app.accounts().add(name, common::PW, role).unwrap();
    let secret = app.accounts().totp_enroll(name).unwrap();
    let code = common::current_totp_code(&secret);
    app.accounts().totp_confirm(name, &code).unwrap();
    let (session, token) = app
        .accounts()
        .login(
            name,
            common::PW,
            Some(&code),
            app.config().session_ttl_ms,
            "",
        )
        .unwrap();
    assert!(session.mfa, "{name} 的会话必须 mfa=true");
    (token, secret)
}

/// 平台侧：Admin audience 会话令牌打通 `/api/admin/identity/users/{id}/suspend`，
/// 且静态 `admin_token` 打不通（两条路径互不通用）。
#[tokio::test]
async fn admin_session_token_authorizes_live_platform_suspend() {
    let env = live("g02-09-platform").await;
    let victim = env.platform.register("live-victim", "pw").unwrap();
    let operator = env.platform.register("live-operator", "pw").unwrap();
    let victim_token = login(&env.base, "live-victim").await;

    let admin_token = admin_session_token(&env.db, &operator);
    let path = format!("/api/admin/identity/users/{victim}/suspend");

    // 正向：Admin 会话令牌 → 200 + 回执（sessions_revoked 非零）。
    let (status, receipt) = common::http_platform(
        "POST",
        &env.base,
        &path,
        Some(json!({"reason": "spam", "actor": "ops", "trace": "live-1"})),
        None,
        None,
        Some(&admin_token),
    )
    .await;
    assert_eq!(status, 200, "suspend: {receipt}");
    assert_eq!(receipt["after"]["state"], "suspended", "{receipt}");
    let revoked = receipt["sessions_revoked"].as_u64().unwrap();
    assert!(revoked >= 1, "封禁应撤销至少一个会话: {receipt}");
    assert!(
        receipt["reference"].as_str().unwrap().starts_with("gov:"),
        "{receipt}"
    );

    // 旧会话立即失效（域内删 sessions；平台 token 查找走同一张表）。
    let (status, body) = common::http_platform(
        "GET",
        &env.base,
        "/api/me",
        None,
        Some(&victim_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 401, "封禁后旧令牌应失效: {body}");

    // 负向对照：静态 admin_token 不是会话，治理端点一律 401。
    let (status, body) = common::http_platform(
        "POST",
        &env.base,
        &path,
        Some(json!({"reason": "spam", "actor": "ops"})),
        None,
        None,
        Some(common::PLATFORM_TOKEN),
    )
    .await;
    assert_eq!(status, 401, "静态令牌不该打通治理面: {body}");
}

/// adminx 侧：`Config::platform_token` = Admin 会话令牌时，`Upstream` 与完整
/// G14 HTTP 面都能把处置转发到真平台；静态令牌则被平台拒成 401。
#[tokio::test]
async fn adminx_platform_token_drives_suspend_through_upstream() {
    let env = live("g02-09-adminx").await;
    let victim = env.platform.register("live-victim-2", "pw").unwrap();
    let operator = env.platform.register("live-operator-2", "pw").unwrap();
    login(&env.base, "live-victim-2").await;

    let admin_token = admin_session_token(&env.db, &operator);

    // 装配一个"平台进程活着"的 adminx，platform_token 换成 Admin 会话令牌
    // （生产由 WXLIKE_ADMIN_PLATFORM_TOKEN 配；默认值是静态后台令牌，打不通）。
    let accounts_db = env.dir.join("adminx.db");
    let mut cfg = common::cfg(env.db.clone(), accounts_db.clone(), env.base.clone());
    cfg.platform_token = admin_token.clone();
    let read = PlatformRead::open(&env.db).expect("平台在线时应能严格只读打开");
    let accounts = wxlike_admin::AccountStore::open(&accounts_db).unwrap();
    let app = App::assemble(read, accounts, cfg);

    // (a) 直接复用 adminx 的 Upstream（与 App::open 同一个构造）：
    //     platform_token 作 x-admin-token，打平台 suspend。
    let upstream = Upstream::new(
        app.config().platform_url.clone(),
        app.config().platform_token.clone(),
    );
    let path = format!("/api/admin/identity/users/{victim}/suspend");
    let reply = upstream
        .call(
            "POST",
            &path,
            "ops",
            Some(json!({"reason": "spam", "actor": "ops", "trace": "live-2"})),
        )
        .await
        .expect("adminx 的 platform_token 应打通平台治理端点");
    assert_eq!(reply.status, 200, "{}", reply.body);
    let revoked = reply.body["sessions_revoked"].as_u64().unwrap();
    assert!(revoked >= 1, "adminx 转发应撤掉会话: {}", reply.body);

    // (b) 负向对照：同一条 Upstream 换静态令牌 → 平台 401，adminx 如实报 Upstream。
    let static_up = Upstream::new(
        app.config().platform_url.clone(),
        common::PLATFORM_TOKEN.to_string(),
    );
    let err = static_up
        .call(
            "POST",
            &path,
            "ops",
            Some(json!({"reason": "spam", "actor": "ops"})),
        )
        .await
        .unwrap_err();
    match err {
        AdminError::Upstream { status, .. } => assert_eq!(status, 401, "{err}"),
        other => panic!("应是 Upstream 拒绝，实际 {other:?}"),
    }

    // (c) 完整 adminx HTTP 面：G14 两阶段处置（preview → confirm）也走同一个
    //     platform_token，证明不是只有裸 Upstream 能通。
    let victim3 = env.platform.register("live-victim-3", "pw").unwrap();
    login(&env.base, "live-victim-3").await;
    let router = app.router();
    let (session, secret) = mfa_session(&app, "ops", Role::Moderator);
    let code = common::current_totp_code(&secret);
    let (status, step) = common::call(
        &router,
        common::req(
            "POST",
            "/api/g14/session/stepup",
            Some(json!({"code": code})),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, 200, "step-up: {step}");

    let target = format!("/api/g14/identity/users/{victim3}/suspend");
    let (status, preview) = common::call(
        &router,
        common::req(
            "POST",
            &target,
            Some(json!({"reason": "live evidence"})),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, 200, "preview: {preview}");
    let confirm = preview["result"]["confirmation"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, done) = common::call(
        &router,
        common::req(
            "POST",
            &target,
            Some(json!({"reason": "live evidence", "confirm": confirm})),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, 200, "suspend: {done}");
    let revoked = done["result"]["sessions_revoked"].as_u64().unwrap();
    assert!(revoked >= 1, "adminx 面转发应撤掉会话: {done}");
}
