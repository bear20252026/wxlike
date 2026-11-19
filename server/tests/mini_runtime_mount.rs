//! G08：小程序**真实域**挂进组合根之后的验收（收口第四轮审计「测而未接」）。
//!
//! 打的是 `Stack::app` —— 与监听端口上跑的是同一个 Router。域侧用例
//! （miniapps/container/dpage crate 内）已钉死包供应链/宿主/编解码内核；这里证明
//! 的是「路由真的挂上了、dispatch 真的进了三个域 crate、鉴权与幂等在真实组合根上
//! 仍然成立」。
//!
//! 底线：
//! 1. 匿名一律 401（不是 404 —— 404 说明压根没挂）；
//! 2. 空目录搜索/推荐 → 空数组（不拿静态假数据填充）；
//! 3. 包体检视走 dpage::peek + miniapps::open（真实编解码与逐资源摘要校验）；
//! 4. 沙箱启动走 container::MiniHost（真实 Boa 生命周期），不在目录里的包拒绝；
//! 5. 同包重复启动幂等（同 app/version/digest）；
//! 6. 验签缺根信任 → 诚实 501；配了根信任 → 真实 verify 通过、篡改包拒绝；
//! 7. 安装诚实 501（无流式核心提供者，不伪造安装成功）。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_container::host::profile::DeviceTarget;
use wxlike_container::host::MiniHost;
use wxlike_dpage::{self as dpage, Page};
use wxlike_miniapps::catalog::source::{CatalogEntry, Category};
use wxlike_miniapps::packages::{
    build_signed, generate_key, issue_cert, open, Channel, Manifest, PublisherCert, TrustStore,
};
use wxlike_platform::{Platform, PlatformOptions};
use wxlike_server::mini_runtime::{self, AppState, CatalogStore};
use wxlike_server::{start_stack, StackOptions};

const HOST: &str = "0.1.0";
const PUBLISHER: &str = "wxlike-labs";
const KEY_ID: &str = "pub-1";
const APP_ID: &str = "com.wxlike.runtime";

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes).unwrap_or(json!({}))
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

async fn register(app: &Router, name: &str) -> String {
    let (status, reply) = send(
        app,
        request(
            "POST",
            "/api/register",
            None,
            Some(json!({"name": name, "password": "pw"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register {name}: {reply}");
    reply["token"].as_str().unwrap().to_string()
}

fn unique_dir(name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "wxlike-mini-rt-{name}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&path);
    path
}

fn entry() -> CatalogEntry {
    CatalogEntry {
        app_id: APP_ID.to_string(),
        name: "运行时演示".to_string(),
        version: "1.0.0".to_string(),
        published_versions: vec!["1.0.0".to_string()],
        category: Category::Tools,
        summary: "一个可启动的演示页".to_string(),
        publisher: PUBLISHER.to_string(),
        capabilities: vec!["storage".to_string()],
        featured: true,
    }
}

/// 构造一个预置审核目录的真实域路由（落盘到唯一临时目录）。
fn runtime_app(platform: &wxlike_platform::PlatformState, trust_path: Option<PathBuf>) -> Router {
    let dir = unique_dir("catalog");
    let store = Arc::new(CatalogStore::open(dir.join("mini-catalog.json")).unwrap());
    store.upsert_entry(entry()).unwrap();
    mini_runtime::router(Arc::new(AppState {
        catalog: store,
        platform: platform.clone(),
        host_version: HOST.to_string(),
        trust_path,
    }))
}

fn platform() -> wxlike_platform::PlatformState {
    Platform::in_memory_with(PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        ..Default::default()
    })
    .unwrap()
    .shared()
}

/// 合并平台注册路由 + 真实域路由，token 来自真 sessions 表。
fn merged_app(trust_path: Option<PathBuf>) -> (wxlike_platform::PlatformState, Router) {
    let plat = platform();
    let app = Platform::router(plat.clone()).merge(runtime_app(&plat, trust_path));
    (plat, app)
}

/// 写一个演示页源码目录，返回签名包与根信任快照路径。
struct PackageFixture {
    blob_hex: String,
    key_id: String,
    sig_hex: String,
    trust_path: PathBuf,
    digest: String,
}

fn package_fixture() -> PackageFixture {
    let dir = unique_dir("pkg");
    let source = dir.join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("index.js"),
        "globalThis.onLoad = function(){ wxhost.setTitle('Runtime Demo'); }; \
         globalThis.onShow = function(){};",
    )
    .unwrap();

    let root = generate_key();
    let signer = generate_key();
    let cert = PublisherCert {
        publisher: PUBLISHER.to_string(),
        key_id: KEY_ID.to_string(),
        public_key: hex::encode(signer.verifying_key().as_bytes()),
        not_before: 0,
        not_after: u64::MAX,
    };
    let signed_cert = issue_cert("root-1", &root, cert).unwrap();
    let mut trust = TrustStore::new("root-1", root.verifying_key());
    trust.enroll(&signed_cert, wxlike_miniapps::packages::trust::now_seconds()).unwrap();

    let template = Manifest {
        app_id: APP_ID.to_string(),
        name: "运行时演示".to_string(),
        version: "1.0.0".to_string(),
        min_host: "0.1.0".to_string(),
        entry: "index.js".to_string(),
        channel: Channel::Prod,
        publisher: PUBLISHER.to_string(),
        capabilities: vec!["storage".to_string()],
        resources: BTreeMap::new(),
    };
    let package = build_signed(&source, &template, HOST, &signer, KEY_ID).unwrap();
    let digest = open(&package.blob, HOST).unwrap().digest;

    // 根信任快照（只含公开材料）：验签面经 toolchain::load_trust 读它。
    let snapshot = json!({
        "root_key_id": "root-1",
        "root_public_key": hex::encode(root.verifying_key().as_bytes()),
        "certs": [signed_cert],
        "revoked_keys": [],
        "revoked_publishers": [],
    });
    let trust_path = dir.join("trust.json");
    std::fs::write(&trust_path, serde_json::to_vec_pretty(&snapshot).unwrap()).unwrap();

    PackageFixture {
        blob_hex: hex::encode(&package.blob),
        key_id: package.signature.key_id.clone(),
        sig_hex: package.signature.sig.clone(),
        trust_path,
        digest,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_runtime_requests_are_rejected() {
    let (_plat, app) = merged_app(None);
    for (method, uri) in [
        ("GET", "/api/mini/catalog/search"),
        ("GET", "/api/mini/catalog/recommend"),
        ("POST", "/api/mini/package/inspect"),
        ("POST", "/api/mini/package/verify"),
        ("POST", "/api/mini/launch"),
        ("POST", "/api/mini/install"),
    ] {
        let (status, _) = send(&app, request(method, uri, None, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} 匿名必须 401");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn catalog_search_and_recommend_dispatch_into_the_domain() {
    let (_plat, app) = merged_app(None);
    let token = register(&app, "mini-rt-search").await;

    // 搜索命中预置目录卡（真实 Catalog::search）。
    let (status, body) = send(
        &app,
        request("GET", "/api/mini/catalog/search?q=演示", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["app_id"], json!(APP_ID));

    // 推荐（真实 Catalog::recommend）。
    let (status, body) = send(
        &app,
        request("GET", "/api/mini/catalog/recommend?limit=5", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);

    // 无匹配 → 空数组（不是错误，也不填充假数据）。
    let (status, body) = send(
        &app,
        request("GET", "/api/mini/catalog/search?q=不存在的应用", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["items"].as_array().unwrap().is_empty());

    // 非法 limit → 400（域校验）。
    let (status, _) = send(
        &app,
        request("GET", "/api/mini/catalog/search?limit=0", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn package_inspect_runs_real_dpage_and_miniapps() {
    let fx = package_fixture();
    let (_plat, app) = merged_app(None);
    let token = register(&app, "mini-rt-inspect").await;

    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/mini/package/inspect",
            Some(&token),
            Some(json!({"package_hex": fx.blob_hex})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["app_id"], json!(APP_ID));
    assert_eq!(body["version"], json!("1.0.0"));
    assert_eq!(body["digest"], json!(fx.digest));
    assert_eq!(body["dpage_version"], json!(1));
    assert_eq!(body["signature"], json!("unverified"));
    assert!(body["files"].as_u64().unwrap() >= 1);

    // 非 hex → 400。
    let (status, _) = send(
        &app,
        request(
            "POST",
            "/api/mini/package/inspect",
            Some(&token),
            Some(json!({"package_hex": "not-hex!"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn launch_runs_the_real_container_host_and_is_idempotent() {
    let fx = package_fixture();
    let (_plat, app) = merged_app(None);
    let token = register(&app, "mini-rt-launch").await;

    let body = json!({"package_hex": fx.blob_hex, "target": "desktop", "payload": "{}"});
    let (status, first) = send(
        &app,
        request("POST", "/api/mini/launch", Some(&token), Some(body.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["app_id"], json!(APP_ID));
    assert_eq!(first["title"], json!("Runtime Demo"));
    assert_eq!(first["state"], json!("running"));
    assert_eq!(first["digest"], json!(fx.digest));
    assert_eq!(first["verified"], json!(false));

    // 同包重复启动：app/version/digest 不变（幂等），会话 id 每次新生。
    let (status, second) = send(
        &app,
        request("POST", "/api/mini/launch", Some(&token), Some(body)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["app_id"], first["app_id"]);
    assert_eq!(second["version"], first["version"]);
    assert_eq!(second["digest"], first["digest"]);
    assert_eq!(second["title"], first["title"]);
    assert_ne!(second["session_id"], first["session_id"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn launch_refuses_a_package_outside_the_reviewed_catalog() {
    // 目录里只有 APP_ID；构造另一个 app_id 的包，启动必须 404。
    let dir = unique_dir("foreign");
    let source = dir.join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("index.js"), "globalThis.onLoad=function(){};globalThis.onShow=function(){};").unwrap();
    let signer = generate_key();
    let template = Manifest {
        app_id: "com.wxlike.foreign".to_string(),
        name: "目录外".to_string(),
        version: "1.0.0".to_string(),
        min_host: "0.1.0".to_string(),
        entry: "index.js".to_string(),
        channel: Channel::Prod,
        publisher: PUBLISHER.to_string(),
        capabilities: vec![],
        resources: BTreeMap::new(),
    };
    let package = build_signed(&source, &template, HOST, &signer, KEY_ID).unwrap();

    let (_plat, app) = merged_app(None);
    let token = register(&app, "mini-rt-foreign").await;
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/mini/launch",
            Some(&token),
            Some(json!({"package_hex": hex::encode(&package.blob)})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn verify_is_honest_501_without_root_trust() {
    let fx = package_fixture();
    let (_plat, app) = merged_app(None); // 不配根信任
    let token = register(&app, "mini-rt-verify-none").await;
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/mini/package/verify",
            Some(&token),
            Some(json!({"package_hex": fx.blob_hex, "key_id": fx.key_id, "sig": fx.sig_hex})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(body["error"].as_str().unwrap_or_default().contains("WXLIKE_MINI_TRUST"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn verify_with_root_trust_accepts_signed_and_rejects_tampered() {
    let fx = package_fixture();
    let (_plat, app) = merged_app(Some(fx.trust_path.clone()));
    let token = register(&app, "mini-rt-verify").await;

    // 合法签名包 → 真实 verify 通过。
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/mini/package/verify",
            Some(&token),
            Some(json!({"package_hex": fx.blob_hex, "key_id": fx.key_id, "sig": fx.sig_hex})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["verified"], json!(true));
    assert_eq!(body["publisher"], json!(PUBLISHER));
    assert_eq!(body["digest"], json!(fx.digest));

    // 篡改资源后重打包（签名不再匹配清单）→ 拒绝。
    let blob = hex::decode(&fx.blob_hex).unwrap();
    let mut files = dpage::unpack_raw(&blob).unwrap().files;
    files.insert("index.js".to_string(), b"tampered".to_vec());
    let map = dpage::FileMap(
        files.into_iter().map(|(path, data)| dpage::FileEntry { path, data }).collect(),
    );
    let tampered = dpage::pack_files(map).unwrap();
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/mini/package/verify",
            Some(&token),
            Some(json!({"package_hex": hex::encode(&tampered), "key_id": fx.key_id, "sig": fx.sig_hex})),
        ),
    )
    .await;
    assert_ne!(status, StatusCode::OK, "篡改包不得验签通过：{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_is_honest_501() {
    let (_plat, app) = merged_app(None);
    let token = register(&app, "mini-rt-install").await;
    let (status, body) = send(
        &app,
        request(
            "POST",
            "/api/mini/install",
            Some(&token),
            Some(json!({"app_id": APP_ID, "version": "1.0.0"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(body["error"].as_str().unwrap_or_default().contains("流式核心提供者"));
}

/// 真实组合根（start_stack）上这些路由确实挂上了：匿名 401 而非 404。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_routes_are_mounted_in_the_real_stack() {
    let stack = start_stack(StackOptions::default()).await.unwrap();
    for (method, uri) in [
        ("GET", "/api/mini/catalog/search"),
        ("POST", "/api/mini/package/inspect"),
        ("POST", "/api/mini/launch"),
        ("POST", "/api/mini/install"),
    ] {
        let (status, _) = send(&stack.app, request(method, uri, None, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} 应已挂载（401 而非 404）");
    }
    // 登录后可达真实域面。
    let token = register(&stack.app, "mini-rt-stack").await;
    let (status, body) = send(
        &stack.app,
        request("GET", "/api/mini/catalog/search", Some(&token), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["items"].as_array().unwrap().is_empty(), "默认空目录");
}

/// 直接驱动域 crate 的对照基准：证明挂载行为与域内核一致（非 mock）。
#[test]
fn domain_baseline_launch_matches_the_mount() {
    let page = Page {
        id: 1,
        manifest: dpage::Manifest {
            name: "demo".to_string(),
            main_js: "index.js".to_string(),
            title: "demo".to_string(),
        },
        files: BTreeMap::from([(
            "index.js".to_string(),
            b"globalThis.onLoad=function(){wxhost.setTitle('Baseline');};globalThis.onShow=function(){};"
                .to_vec(),
        )]),
    };
    let mut host = MiniHost::launch_default(DeviceTarget::Desktop, APP_ID).unwrap();
    assert_eq!(host.run(&page, "{}").unwrap(), "Baseline");
}
