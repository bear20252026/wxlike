//! 视频控制面的准入（issue #7）与回调目标（issue #8）的验收用例。
//!
//! 这两条是审计里唯一"外部可直接命中"的入口，所以断言写的是**行为**：
//! - 匿名不能上传、不能建作业、不能查/重试作业（401）；
//! - A 不能查/重试 B 的作业（404，连"存在"都不确认）；
//! - 超出并发/磁盘预算当场拒绝，不是等 worker 转完再说；
//! - 客户端给的回调地址默认一律拒绝，白名单之外一律拒绝。

mod support;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use http_body_util::BodyExt as _;
use std::sync::Arc;
use tower::ServiceExt as _;
use wxlike_video::{
    deliver_callback_signed, routes, CallbackBody, CallbackPolicy, Delivery, JobState, RejectKind,
    Rejected, VideoAuth, VideoPolicy, VideoState, VideoStore,
};

/// 测试用认证器：`x-test-user` 头就是身份。
struct HeaderUser;

impl VideoAuth for HeaderUser {
    fn caller(&self, headers: &HeaderMap) -> Option<String> {
        headers
            .get("x-test-user")?
            .to_str()
            .ok()
            .map(str::to_string)
            .filter(|u| !u.is_empty())
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "wxlike-video-access-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn store_with(policy: VideoPolicy, tag: &str) -> VideoStore {
    VideoStore::new(temp_dir(tag)).with_policy(policy)
}

fn auth_policy() -> VideoPolicy {
    VideoPolicy::default().with_auth(Arc::new(HeaderUser))
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(user) = user {
        req = req.header("x-test-user", HeaderValue::from_str(user).unwrap());
    }
    let req = req.body(Body::empty()).unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

/// 没有认证器 = 控制面整体拒绝（fail closed），而不是"不校验"。
#[tokio::test]
async fn control_plane_without_authenticator_is_closed() {
    let store = store_with(VideoPolicy::default(), "closed");
    let app = routes(VideoState { store });
    let (status, body) = call(&app, "GET", "/api/video/jobs/whatever", Some("alice")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("未启用"),
        "要说清为什么调不通：{body}"
    );
}

/// 匿名一律 401；登录后才有机会 404。
#[tokio::test]
async fn anonymous_cannot_touch_jobs() {
    let store = store_with(auth_policy(), "anon");
    let job_id = store
        .submit_job(b"anon-source", "alice", None)
        .unwrap()
        .job
        .job_id;
    let app = routes(VideoState {
        store: store.clone(),
    });

    let (status, _) = call(&app, "GET", &format!("/api/video/jobs/{job_id}"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "匿名查作业");
    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/video/jobs/{job_id}/retry"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "匿名重试");
    let (status, _) = call(&app, "POST", "/api/video/jobs", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "匿名建作业");
    let (status, _) = call(&app, "POST", "/api/video", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "匿名上传");
}

/// A 的作业，B 查不到也重试不了——而且是 404（不确认存在性）。
#[tokio::test]
async fn a_cannot_operate_on_bs_job() {
    let store = store_with(auth_policy(), "owner");
    let job_id = store
        .submit_job(b"bobs-source", "bob", None)
        .unwrap()
        .job
        .job_id;
    let app = routes(VideoState {
        store: store.clone(),
    });

    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/video/jobs/{job_id}"),
        Some("alice"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "别人的作业要 404：{body}");

    let (status, _) = call(
        &app,
        "POST",
        &format!("/api/video/jobs/{job_id}/retry"),
        Some("alice"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "别人的作业不能重试");

    // 本人可以查
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/video/jobs/{job_id}"),
        Some("bob"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "本人应当看得到：{body}");
    assert_eq!(body["owner"], serde_json::json!("bob"));
}

/// 配额：排队作业数到顶就当场拒绝（`Rejected{Quota}`，HTTP 层映射成 429）。
#[tokio::test]
async fn queue_quota_rejects_before_the_worker_runs() {
    let mut policy = auth_policy();
    policy.max_user_jobs = 2;
    let store = store_with(policy, "quota");
    for i in 0..2 {
        store
            .submit_job(format!("quota-{i}").as_bytes(), "alice", None)
            .unwrap();
    }
    let err = store
        .submit_job(b"quota-overflow", "alice", None)
        .expect_err("第 3 个作业必须被拒");
    let rejected = err
        .downcast_ref::<Rejected>()
        .unwrap_or_else(|| panic!("必须是策略拒绝而不是内部错误：{err:#}"));
    assert_eq!(rejected.kind, RejectKind::Quota);

    // 另一个用户不受影响（配额是按人算的）
    store.submit_job(b"quota-other-user", "bob", None).unwrap();
}

/// 磁盘预算：单用户排队字节到顶也拒绝。
#[tokio::test]
async fn byte_budget_rejects_oversized_queue() {
    let mut policy = auth_policy();
    policy.max_user_bytes = 64;
    let store = store_with(policy, "bytes");
    store.submit_job(&[7u8; 40], "alice", None).unwrap();
    let err = store
        .submit_job(&[9u8; 40], "alice", None)
        .expect_err("超出个人字节额度必须被拒");
    assert_eq!(
        err.downcast_ref::<Rejected>().map(|r| r.kind),
        Some(RejectKind::Quota)
    );
}

/// 回调地址（issue #8）：默认策略下客户端给什么都不行。
#[tokio::test]
async fn client_callback_url_is_refused_by_default() {
    let store = store_with(auth_policy(), "cb-default");
    let err = store
        .submit_job(b"cb-default-src", "alice", Some("https://example.com/hook"))
        .expect_err("默认策略不接受客户端回调地址");
    assert_eq!(
        err.downcast_ref::<Rejected>().map(|r| r.kind),
        Some(RejectKind::BadCallback)
    );
}

/// 白名单内才放行；白名单外的公网地址同样拒绝；私网即便在白名单里也拒绝。
#[tokio::test]
async fn callback_allowlist_and_private_address_rules() {
    let policy = auth_policy().with_callbacks(CallbackPolicy::allow_hosts([
        "hooks.example.com".to_string(),
        "127.0.0.1".to_string(),
    ]));
    let store = store_with(policy, "cb-allow");

    // 白名单内的公网域名：放行（不真的发，只看提交成功）
    let ok = store
        .submit_job(b"cb-ok", "alice", Some("https://hooks.example.com/x"))
        .expect("白名单内的地址应当接受");
    assert_eq!(
        ok.job.callback_url.as_deref(),
        Some("https://hooks.example.com/x")
    );

    // 白名单外
    let err = store
        .submit_job(b"cb-evil", "alice", Some("https://evil.example.com/x"))
        .expect_err("白名单外必须拒绝");
    assert_eq!(
        err.downcast_ref::<Rejected>().map(|r| r.kind),
        Some(RejectKind::BadCallback)
    );

    // 白名单内的 IP 字面量，但指向回环：默认仍拒绝
    let err = store
        .submit_job(b"cb-loop", "alice", Some("http://127.0.0.1:9000/x"))
        .expect_err("回环地址即便在白名单里也要拒绝");
    assert_eq!(
        err.downcast_ref::<Rejected>().map(|r| r.kind),
        Some(RejectKind::BadCallback)
    );

    // 显式放开私网（本地联调）后才允许
    let store = store_with(
        auth_policy().with_callbacks(
            CallbackPolicy::allow_hosts(["127.0.0.1".to_string()]).allow_private(true),
        ),
        "cb-loop-ok",
    );
    store
        .submit_job(b"cb-loop-ok", "alice", Some("http://127.0.0.1:9000/x"))
        .expect("显式允许私网时应当接受");
}

/// 回调带上服务间共享令牌：平台侧配了 `WXLIKE_VIDEO_CALLBACK_TOKEN` 时，
/// 不带这个头就是 401，回调等于永远送不到。
#[tokio::test]
async fn callback_carries_the_shared_token() {
    let (url, received) = support::spawn_sink(200, "OK");
    let body = CallbackBody {
        job_id: "job-token".to_string(),
        state: JobState::Succeeded,
        result: None,
        attempts: 0,
        updated_at: 0,
    };
    let delivery = deliver_callback_signed(&url, &body, Some("s3cret-token")).await;
    assert_eq!(delivery, Delivery::Delivered);
    let request = received
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .first()
        .cloned()
        .expect("sink 收到一次请求");
    assert!(
        request
            .to_lowercase()
            .contains("x-wxlike-video-token: s3cret-token"),
        "必须带共享令牌：{request}"
    );

    // 没配令牌时不带这个头（平台侧不校验的场景）
    let (url, received) = support::spawn_sink(200, "OK");
    let delivery = deliver_callback_signed(&url, &body, None).await;
    assert_eq!(delivery, Delivery::Delivered);
    let request = received
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .first()
        .cloned()
        .expect("sink 收到一次请求");
    assert!(
        !request.to_lowercase().contains("x-wxlike-video-token"),
        "没配令牌就不该带：{request}"
    );
}
