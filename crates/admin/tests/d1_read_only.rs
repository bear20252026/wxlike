//! D1 验收：只读后台。
//!
//! 卡的验收栏写的是「6 项测试 + `WXLIKE_ADMIN_TOKEN` 缺失拒绝启动」，
//! 外加一条**只读（任何写路径都不存在）**。所以这个文件要同时证明：
//!
//! * 读面对的路由对的从真的平台库里出来；
//! * 没有令牌就什么都读不到；
//! * 读面上**不存在**写路由（405），而不是"有但不推荐用"；
//! * 一轮完整读之后，平台库的字节一个都没变；
//! * 真二进制在没有令牌时**拒绝启动**（不是警告后继续）。

mod common;

use axum::http::StatusCode;
use serde_json::json;

use common::{call, cfg, login, platform, req, seed, TempDir, ADMIN_TOKEN, PW};
use std::sync::Arc;
use tower::ServiceExt as _;
use wxlike_admin::rbac::Role;

fn app_with(tag: &str) -> (TempDir, Arc<wxlike_admin::App>) {
    let dir = TempDir::create(tag);
    let db = dir.join("wxlike.db");
    let p = platform(&db);
    seed(&p);
    // 平台实例必须活到只读打开之后：WAL 的 -shm 还在，才走严格的只读路径
    let read = wxlike_admin::PlatformRead::open(&db).unwrap();
    assert_eq!(read.open_mode(), "read_only", "平台在线时应走严格只读");
    let accounts = wxlike_admin::AccountStore::open(&dir.join("adminx.db")).unwrap();
    drop(p);
    let app = wxlike_admin::App::assemble(
        read,
        accounts,
        cfg(
            db.clone(),
            dir.join("adminx.db"),
            "http://127.0.0.1:1".into(),
        ),
    );
    (dir, app)
}

#[tokio::test]
async fn health_is_public_and_declares_the_read_only_boundary() {
    let (_dir, app) = app_with("d1-health");
    let router = app.router();
    let (status, body) = call(&router, req("GET", "/health", None, None)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], json!(true), "表齐了才该是 ok：{body}");
    assert_eq!(
        body["writes_platform_db"],
        json!(false),
        "这个进程不写平台库"
    );
    assert_eq!(body["db_open_mode"], json!("read_only"));
    assert_eq!(body["missing_tables"].as_array().unwrap().len(), 0);
    // 账号库还没建号：写面不可用，但 /health 不该因此挂掉
    assert_eq!(body["accounts_present"], json!(false));
    assert!(body["uptime_ms"].as_u64().unwrap() < 60_000);
}

#[tokio::test]
async fn reads_require_a_credential_and_reject_a_wrong_one() {
    let (_dir, app) = app_with("d1-auth");
    let router = app.router();
    for path in [
        "/api/stats",
        "/api/users",
        "/api/groups",
        "/api/audit",
        "/api/reports",
        "/api/moderation",
        "/api/oplog",
        "/api/ops",
    ] {
        let (status, _) = call(&router, req("GET", path, None, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} 无令牌必须 401");
        let (status, _) = call(&router, req("GET", path, None, Some("guessed-token"))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} 错令牌必须 401");
    }
    // 带对的令牌就通
    let (status, body) = call(&router, req("GET", "/api/stats", None, Some(ADMIN_TOKEN))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // 会话令牌同样能读：登录后不该再要求填一次静态令牌
    let session = login(&app, "ops-read", Role::Viewer);
    let (status, _) = call(&router, req("GET", "/api/stats", None, Some(&session))).await;
    assert_eq!(status, StatusCode::OK);
    // 但会话不能冒充静态令牌去写（这条在 D3 展开）
    let (status, body) = call(
        &router,
        req(
            "POST",
            "/api/op/wallet.reconcile",
            Some(json!({})),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "viewer 无写权限: {body}");
    assert_eq!(
        call(&router, req("GET", "/", None, None)).await.0,
        StatusCode::OK,
        "页面本身不需要令牌（数据才需要）"
    );
}

#[tokio::test]
async fn dashboard_and_lists_come_from_the_real_platform_db() {
    let (_dir, app) = app_with("d1-data");
    let router = app.router();
    let (_, stats) = call(&router, req("GET", "/api/stats", None, Some(ADMIN_TOKEN))).await;
    assert_eq!(stats["users_total"], json!(4), "alice bob carol dave");
    assert_eq!(stats["groups_total"], json!(1));
    assert_eq!(stats["wallet_sum_minor"], json!(5_000), "转账是零和的");
    assert_eq!(stats["moderation_held"], json!(1));
    assert_eq!(stats["reports_open"], json!(1));
    assert!(stats["users_created_24h"].as_i64().unwrap() >= 4);
    // 审计条数会随平台的落审计行为变（register / report.create / 手工 audit），
    // 所以这里不硬编码，而是拿两个读面互校：看板的窗口计数必须等于检索面的总数。
    let (_, rows) = call(
        &router,
        req("GET", "/api/audit?limit=200", None, Some(ADMIN_TOKEN)),
    )
    .await;
    let total = rows["total"].as_i64().unwrap();
    assert!(total >= 8, "至少 seed 里那 8 条: {rows}");
    assert_eq!(
        stats["audit_in_sample_24h"].as_i64().unwrap(),
        total,
        "看板与检索面对同一批数据的计数必须一致"
    );
    assert_eq!(rows["limit"], json!(200));

    // 一页装下全部：users 按注册时间倒序，alice 是最早的那个，别拿"第一页"赌顺序
    let (_, users) = call(
        &router,
        req("GET", "/api/users?limit=100", None, Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(users["total"], json!(4));
    assert_eq!(users["rows"].as_array().unwrap().len(), 4);
    let alice = users["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["name"] == json!("alice"))
        .expect("alice 在结果里");
    assert_eq!(alice["ledger_net"], json!(3_800), "5000 入账 − 1200 转出");
    assert_eq!(alice["wallet_balance"], json!(3_800));
    // 口令与 TOTP 字段绝不出现在读面上
    let text = alice.to_string();
    for secret in ["pw_hash", "totp_secret", "salt"] {
        assert!(!text.contains(secret), "用户行泄露了 {secret}: {text}");
    }

    let (_, groups) = call(&router, req("GET", "/api/groups", None, Some(ADMIN_TOKEN))).await;
    assert_eq!(groups["total"], json!(1));
    assert_eq!(groups["rows"][0]["name"], json!("研发大群"));
    assert_eq!(groups["rows"][0]["members"], json!(3), "owner + 2 成员");

    let (_, held) = call(
        &router,
        req("GET", "/api/moderation", None, Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(held["total"], json!(1));
    assert!(held["rows"][0]["text"]
        .as_str()
        .unwrap()
        .contains("加我微信"));
    assert!(!held["rows"][0]["labels"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn no_write_route_exists_on_the_read_surface() {
    let (_dir, app) = app_with("d1-no-write");
    let router = app.router();
    // 读面上的每个路径，非 GET 一律 405 —— "只读"是路由表决定的，不是态度
    for (path, methods) in [
        (
            "/api/stats",
            ["POST", "PUT", "PATCH", "DELETE"] as [&str; 4],
        ),
        (
            "/api/users",
            ["POST", "PUT", "PATCH", "DELETE"] as [&str; 4],
        ),
        (
            "/api/audit",
            ["POST", "PUT", "PATCH", "DELETE"] as [&str; 4],
        ),
        (
            "/api/reports",
            ["POST", "PUT", "PATCH", "DELETE"] as [&str; 4],
        ),
    ] {
        for m in methods {
            let (status, _) = call(
                &router,
                req(m, path, Some(json!({"anything": 1})), Some(ADMIN_TOKEN)),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::METHOD_NOT_ALLOWED,
                "{m} {path} 不该存在"
            );
        }
    }
}

#[tokio::test]
async fn a_full_read_battery_leaves_the_platform_db_bytes_untouched() {
    let dir = TempDir::create("d1-bytes");
    let db = dir.join("wxlike.db");
    let wal = dir.join("wxlike.db-wal");
    let p = platform(&db);
    let seed_data = seed(&p);
    // 先让平台彻底关完再取快照：SQLite 在最后一条连接关闭时会把 WAL 检查点
    // 合并回主库，主文件*必然*变。那不是你写进去的，是它自己收的尾——
    // 在平台还开着的时候取基线，会得到一个假的"后台改库了"。
    drop(p);
    let snapshot = || -> (Vec<u8>, Vec<u8>) {
        // 只哈希数据文件。-shm 是 WAL 的协调文件，只读连接也可能碰它，
        // 但它不含业务数据——把它算进来会得到假的"变了"。
        let main = std::fs::read(&db).unwrap_or_default();
        let wal_bytes = std::fs::read(&wal).unwrap_or_default();
        (main, wal_bytes)
    };
    let before = snapshot();

    let app = common::readonly_app(db.clone(), dir.join("adminx.db"));
    let router = app.router();
    for path in [
        "/health",
        "/api/stats",
        "/api/users?limit=100",
        "/api/groups",
        "/api/audit?limit=100",
        "/api/reports",
        "/api/moderation",
        "/api/ops",
        "/",
    ] {
        let (status, _) = call(&router, req("GET", path, None, Some(ADMIN_TOKEN))).await;
        assert_eq!(status, StatusCode::OK, "{path}");
    }
    // 一次被拒的写（viewer）也不许留下任何平台库痕迹
    let session = login(&app, "ops-audit", Role::Viewer);
    let (status, _) = call(
        &router,
        req(
            "POST",
            "/api/op/report.triage",
            Some(json!({"target": seed_data.report_id, "note": "试着分诊"})),
            Some(&session),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let after = snapshot();
    assert_eq!(after.0, before.0, "平台库主文件被改动过——后台必须是只读的");
    assert_eq!(after.1, before.1, "WAL 被改动过");
    // 走哪条打开路径取决于 -shm 在不在（与平台关得多干净、文件系统有关），
    // 两条都必须读得到数据；"两条路径都写不进"由 read.rs 的用例逐条守。
    assert!(
        matches!(app.read().open_mode(), "read_only" | "query_only"),
        "打开方式只能是这两种，实际 {}",
        app.read().open_mode()
    );
    // 数据仍然读得到（回退路径不是"打开成功但读空"）
    let (_, stats) = call(&router, req("GET", "/api/stats", None, Some(ADMIN_TOKEN))).await;
    assert_eq!(stats["users_total"], json!(4));
    assert_eq!(stats["reports_open"], json!(1));
}

#[tokio::test]
async fn every_path_the_page_calls_is_registered() {
    // 页面调的每个路径都真存在。这条防的是"后端改名、前端不知道"——
    // 那种 bug 表现为面板一块空白，而不是报错，是最难发现的一类。
    let (_dir, app) = app_with("d1-wiring");
    let router = app.router();
    let id = {
        let (_, queue) = call(&router, req("GET", "/api/reports", None, Some(ADMIN_TOKEN))).await;
        queue["rows"][0]["id"].as_str().unwrap().to_string()
    };
    let report_path = format!("/api/reports/{id}");
    let op_path = "/api/op/report.triage";
    let paths: Vec<(&str, &str)> = vec![
        ("GET", "/health"),
        ("GET", "/"),
        ("GET", "/api/stats"),
        ("GET", "/api/users"),
        ("GET", "/api/groups"),
        ("GET", "/api/audit"),
        ("GET", "/api/reports"),
        ("GET", "/api/moderation"),
        ("GET", "/api/oplog"),
        ("GET", "/api/ops"),
        ("GET", &report_path),
        ("POST", "/api/login"),
        ("POST", "/api/logout"),
        ("POST", op_path),
    ];
    for (method, path) in paths {
        let body = if method == "POST" && path == "/api/login" {
            Some(json!({"username": "nobody", "password": "no-pass-at-all"}))
        } else if method == "POST" {
            Some(json!({"target": id, "note": "x"}))
        } else {
            None
        };
        let (status, resp) = call(&router, req(method, path, body, Some(ADMIN_TOKEN))).await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {path} 没注册（页面会静默失败）: {resp}"
        );
        assert_ne!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path} 的方法不对: {resp}"
        );
    }
    // 未注册的路径必须是 404（证明上面那条断言不是空转）
    let (status, _) = call(
        &router,
        req(
            "GET",
            "/api/definitely-not-a-route",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn page_is_served_with_the_right_type_and_no_data_leak() {
    let (_dir, app) = app_with("d1-page");
    let resp = app
        .router()
        .oneshot(req("GET", "/", None, None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(html.contains("<!doctype html>"));
    assert!(html.contains("adminx 0.1.0"));
    // 页面是静态壳：不该把令牌或任何库内容编进去
    assert!(!html.contains(ADMIN_TOKEN), "页面泄露了令牌");
    assert!(!html.contains(PW));
}

/// 卡的验收面：**真进程**缺令牌时拒绝启动。
/// 这条必须用子进程测：拒绝发生在配置解析阶段，`oneshot` 那套永远碰不到。
#[test]
fn binary_refuses_to_start_without_the_admin_token() {
    let exe = env!("CARGO_BIN_EXE_wxlike-admin");
    let dir = TempDir::create("d1-no-token");
    let out = std::process::Command::new(exe)
        .env_remove("WXLIKE_ADMIN_TOKEN")
        .env_remove("WXLIKE_ADMIN_PLATFORM_TOKEN")
        .env("WXLIKE_DB", dir.join("wxlike.db"))
        .env("WXLIKE_ADMIN_DB", dir.join("adminx.db"))
        .output()
        .expect("能启动子进程");
    assert!(!out.status.success(), "缺令牌必须非零退出");
    let text = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        text.contains("wxlike_admin_token"),
        "要说清缺的是哪个变量，实际 stderr: {text}"
    );
    assert!(
        text.contains("拒绝启动") || text.contains("configuration"),
        "{text}"
    );
    assert!(!dir.join("adminx.db").exists(), "拒绝启动不该顺手建库");
}

#[test]
fn binary_check_reports_the_resolved_config_without_binding() {
    let exe = env!("CARGO_BIN_EXE_wxlike-admin");
    let dir = TempDir::create("d1-check");
    let db = dir.join("wxlike.db");
    {
        let p = platform(&db);
        seed(&p);
    }
    let out = std::process::Command::new(exe)
        .arg("--check")
        .env("WXLIKE_ADMIN_TOKEN", ADMIN_TOKEN)
        .env("WXLIKE_DB", &db)
        .env("WXLIKE_ADMIN_DB", dir.join("adminx.db"))
        .output()
        .expect("启动子进程");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("配置自检"), "{text}");
    assert!(text.contains("平台库可读"), "{text}");
    assert!(text.contains("表齐全          是"), "{text}");
    assert!(
        !text.contains(ADMIN_TOKEN),
        "--check 不许把令牌打出来：{text}"
    );
    // 自检阶段不建账号库
    assert!(
        text.contains("还没有管理员账号") || text.contains("0 个管理员账号"),
        "{text}"
    );
}

#[test]
fn binary_check_fails_loudly_when_the_platform_db_is_wrong() {
    // WXLIKE_DB 指到一个不存在的文件：必须失败，而不是起一个空看板
    let exe = env!("CARGO_BIN_EXE_wxlike-admin");
    let dir = TempDir::create("d1-check-bad");
    let out = std::process::Command::new(exe)
        .arg("--check")
        .env("WXLIKE_ADMIN_TOKEN", ADMIN_TOKEN)
        .env("WXLIKE_DB", dir.join("nope.db"))
        .env("WXLIKE_ADMIN_DB", dir.join("adminx.db"))
        .output()
        .unwrap();
    assert!(!out.status.success(), "库不存在时 --check 必须失败");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("打不开"),
        "{out:?}"
    );
}

#[tokio::test]
async fn in_memory_platform_db_is_rejected_before_any_binding() {
    // 独立进程读内存库 = 永远读到空。Config 层拦住，别等看板空白才发现。
    let mut env = std::collections::HashMap::new();
    env.insert(
        "WXLIKE_ADMIN_TOKEN".to_string(),
        Some(ADMIN_TOKEN.to_string()),
    );
    env.insert("WXLIKE_DB".to_string(), Some(":memory:".to_string()));
    let get = |k: &str| env.get(k).and_then(|v| v.clone());
    let err = wxlike_admin::Config::resolve(&get, std::path::Path::new("/tmp")).unwrap_err();
    assert!(err.to_string().contains(":memory:"), "{err}");
}
