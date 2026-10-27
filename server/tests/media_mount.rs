//! #295 / G00：media 域**挂进真实组合根之后**的验收。
//!
//! 打的是 `Stack::app` —— 与监听端口上跑的是同一个 Router。这样两条断言才有
//! 意义："路由挂在装配好的 app 上"（不是测试自己拼的近似 app）与"鉴权真的走
//! 平台 sessions 表"（不是 fixture token）。
//!
//! 三条底线：
//! 1. 匿名请求 **401**（不是 404 —— 404 说明压根没挂；200 说明放行了）；
//! 2. 真注册拿到的 token 打到 handler，并跑通一次上传→签名链接→下载；
//! 3. 吊销会话后同一个 token 立刻 401（认证只有一条路，改一张表就全网生效）；
//! 4. 全新的 server 库上 M009 的 9 张表都在，重开（第二次启动）不报错。

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_media::ids;
use wxlike_server::{start_stack, StackOptions};

/// media 域的全部受保护面（抽查 5 条路径 + 1 条写路径）。
const GUARDED: &[(&str, &str)] = &[
    ("GET", "/api/v1/media/capabilities"),
    ("GET", "/api/v1/files"),
    ("POST", "/api/v1/uploads"),
    ("POST", "/api/v1/jobs"),
    ("POST", "/api/v1/cache/execute"),
    ("GET", "/api/v1/platform/audio/indicator"),
];

/// G04 的单对象能力承诺（1 GiB）。
const ONE_GIB: u64 = wxlike_media::GIB;

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

fn request(method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    match body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

/// 真注册：走的是平台 `/api/register`，token 来自 `sessions` 表。
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
        reply["user_id"].as_str().unwrap().to_string(),
        reply["token"].as_str().unwrap().to_string(),
    )
}

/// 匿名一律 401：既不是"没挂路由"（404），也不是"先放行再说"（200）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_media_requests_are_rejected() {
    let stack = boot().await;
    let app = stack.app.clone();
    for (method, uri) in GUARDED {
        let (status, body) = send(&app, request(method, uri, None, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} → {body}");
    }
    // 假 token 也不行：认证问的是库，不是"有没有 Authorization 头"。
    let (status, _) = send(
        &app,
        request(
            "GET",
            "/api/v1/media/capabilities",
            Some("obviously-not-a-session"),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "伪造 bearer 必须 401");
    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 认证请求打到 handler：能力接口回 1 GiB，且主体来自真实会话。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authenticated_request_reaches_the_media_handler() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (_user_id, token) = register(&app, "media-mount-reader").await;

    let (status, caps) = send(
        &app,
        request("GET", "/api/v1/media/capabilities", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "capabilities: {caps}");
    assert_eq!(caps["max_object_bytes"], ONE_GIB, "挂载后仍是 G04 的承诺");
    assert_eq!(caps["max_chunk_bytes"], 8 * 1024 * 1024);

    // 退出登录（吊销 session）→ 同一个 token 立刻失效。
    stack.platform.revoke_session(&token).unwrap();
    let (status, body) = send(
        &app,
        request("GET", "/api/v1/media/capabilities", Some(&token), None),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "吊销后仍放行 = 有第二套鉴权：{body}"
    );
    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 真跑一次上传 → 分片 → 完成 → 签名链接 → 下载（含 Range），
/// 证明挂载点之后整条域服务是通的，而不只是"有个 200 的探活"。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upload_and_signed_download_round_trip_through_the_mount() {
    let stack = boot().await;
    let app = stack.app.clone();
    let (_user_id, token) = register(&app, "media-mount-uploader").await;
    let payload = b"mounted through the real composition root";

    let (status, started) = send(
        &app,
        request(
            "POST",
            "/api/v1/uploads",
            Some(&token),
            Some(json!({
                "purpose": "file",
                "name": "mounted.txt",
                "size": payload.len(),
                "mime": "text/plain",
                "digest": ids::sha256_hex(payload),
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "start upload: {started}");
    let upload_id = started["upload_id"].as_str().unwrap().to_string();

    let chunk = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/uploads/{upload_id}/chunks?offset=0"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("x-chunk-sha256", ids::sha256_hex(payload))
        .body(Body::from(payload.to_vec()))
        .unwrap();
    let (status, progress) = send(&app, chunk).await;
    assert_eq!(status, StatusCode::OK, "chunk: {progress}");

    let (status, done) = send(
        &app,
        request(
            "POST",
            &format!("/api/v1/uploads/{upload_id}/complete"),
            Some(&token),
            Some(json!({ "digest": ids::sha256_hex(payload) })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "complete: {done}");
    let object_id = done["object_id"].as_str().unwrap().to_string();

    let (status, signed) = send(
        &app,
        request(
            "POST",
            &format!("/api/v1/objects/{object_id}/access"),
            Some(&token),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "access: {signed}");
    let url = signed["url"].as_str().unwrap().to_string();

    let response = app
        .clone()
        .oneshot(Request::builder().uri(&url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "签名链接下载");
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/plain",
        "下载要走 media 域的流式面"
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], &payload[..]);

    // 别人的会话拿不到这个对象的签名链接（内容地址不是权限）。
    let (_other_id, other_token) = register(&app, "media-mount-outsider").await;
    let (status, denied) = send(
        &app,
        request(
            "POST",
            &format!("/api/v1/objects/{object_id}/access"),
            Some(&other_token),
            None,
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "外人不该拿到签名链接：{denied}"
    );
    stack.shutdown(std::time::Duration::from_secs(2)).await;
}

/// 迁移：全新的 server 库（真文件，不是 :memory:）要有 M009 的 9 张表，
/// 并且同一份库再装一次不报错、不多出一张表（`IF NOT EXISTS` 可重入）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fresh_server_db_gets_the_media_migration() {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-media-migrate-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("wxlike.db");
    let db_str = db.to_str().unwrap().to_string();

    let stack = start_stack(StackOptions {
        db_path: Some(db_str.clone()),
        ..Default::default()
    })
    .await
    .unwrap();
    let platform = stack.platform.clone();
    let tables = media_tables(&stack);
    for name in EXPECTED_TABLES {
        assert!(
            tables.iter().any(|t| t == name),
            "全新 server 库缺 {name}；实际：{tables:?}"
        );
    }
    // 表在库里，接口就打得通：一次真注册 + 一次能力查询。
    let app = stack.app.clone();
    let (_user_id, token) = register(&app, "media-migration-user").await;
    let (status, caps) = send(
        &app,
        request("GET", "/api/v1/media/capabilities", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "迁移后能力接口: {caps}");
    stack.shutdown(std::time::Duration::from_secs(2)).await;

    // 重入：对同一个文件、用第二个连接再装一次 media 域。
    // 这里**不**调 `start_stack` 第二次——tantivy 搜索索引的写锁按 db 路径独占，
    // 起第二个栈会撞它自己的锁（`Failed to acquire Lockfile`），与 media 迁移无关。
    let _mounted_again = wxlike_server::media_mount::assemble(&platform, Some(&db_str))
        .expect("M009 不可重入 = 重启即挂：media 域第二次装配失败")
        .0;

    let conn = platform.db().lock();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name IN ('media_object','media_ref','media_upload',\
             'media_upload_chunk','media_file','media_share','media_job','media_backup',\
             'media_backup_entry')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count as usize,
        EXPECTED_TABLES.len(),
        "第二次装配不该多出重复的 media 表"
    );
    drop(conn);
    std::fs::remove_dir_all(&dir).ok();
}

/// M009 应当建的 9 张表（`schema/proposals/G04/0001_media.sql`）。
const EXPECTED_TABLES: [&str; 9] = [
    "media_object",
    "media_ref",
    "media_upload",
    "media_upload_chunk",
    "media_file",
    "media_share",
    "media_job",
    "media_backup",
    "media_backup_entry",
];

/// 平台连接读 `sqlite_master`：media 表必须与业务表**同库**（不是第二个文件）。
fn media_tables(stack: &wxlike_server::Stack) -> Vec<String> {
    let conn = stack.platform.db().lock();
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name LIKE 'media_%' ORDER BY name",
        )
        .unwrap();
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    rows
}
