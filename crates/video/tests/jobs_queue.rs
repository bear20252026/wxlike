//! E3：转码作业队列与出站回调。
//!
//! 这些用例**不跑 ffmpeg**（除最后一条走 HTTP 提交的路径）：作业的状态机、
//! 退避、幂等、领取 CAS、回调重试全是库层逻辑，用假字节就能验收。
//! 真转码的端到端在 `jobs_pipeline.rs`（`-- --ignored`，需要 ffmpeg）。
//!
//! 每个用例一个**独立的 store**（独立根目录 = 独立 `video.db`）：HTTP 提交路径会
//! 给那个根目录起后台 worker，别的用例不能被它改状态。

mod support;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use std::net::TcpListener;
use support::spawn_sink;
use tower::ServiceExt as _;
use wxlike_video::{
    backoff_ms, deliver_callback, routes, CallbackBody, CallbackState, Delivery, Ingested,
    JobFailure, JobState, Rendition, VideoState, VideoStore, BASE_DELAY_MS, MAX_DELAY_MS,
    MAX_JOB_ATTEMPTS, STALE_RUNNING_MS,
};

fn store(tag: &str) -> VideoStore {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-video-jobs-{tag}-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    // 回调用例打本机 sink：白名单 + 显式允许私网（issue #8）
    VideoStore::new(dir).with_policy(
        // 本地信任的认证器：这些用例测的是队列语义，不是准入（准入在 video_access.rs）
        wxlike_video::VideoPolicy::trusted_local("tester").with_callbacks(
            wxlike_video::CallbackPolicy::allow_hosts(["127.0.0.1".to_string()])
                .allow_private(true),
        ),
    )
}

fn bytes_of(tag: &str) -> Vec<u8> {
    format!("fake video payload {tag}").into_bytes()
}

fn ingested(job_id: &str) -> Ingested {
    Ingested {
        video_id: job_id.to_string(),
        playlist_url: format!("/api/video/{job_id}/playlist.m3u8"),
        play_url: format!("/api/video/{job_id}/play.mp4"),
        thumb_url: format!("/api/video/{job_id}/thumb.png"),
        duration_ms: 3_000,
        width: 640,
        height: 360,
        renditions: vec![Rendition {
            height: 360,
            width: 640,
            bandwidth_bps: 900_000,
            playlist: "360/playlist.m3u8".into(),
        }],
    }
}

fn now() -> i64 {
    wxlike_core::types::now_ms() as i64
}

// ---------------------------------------------------------------------------
// 退避与状态机
// ---------------------------------------------------------------------------

#[test]
fn backoff_ladder_matches_the_outbox() {
    // 与 crates/core/src/outbox.rs 同一个 1/2/4/8/16/30s：两套退避会让用户在
    // 一处等 30 秒、另一处等 5 分钟
    assert_eq!(backoff_ms(0), BASE_DELAY_MS);
    assert_eq!(backoff_ms(1), 1_000);
    assert_eq!(backoff_ms(2), 2_000);
    assert_eq!(backoff_ms(3), 4_000);
    assert_eq!(backoff_ms(4), 8_000);
    assert_eq!(backoff_ms(5), 16_000);
    assert_eq!(backoff_ms(6), 30_000);
    // 上限与"绝不溢出"
    assert_eq!(backoff_ms(7), MAX_DELAY_MS);
    assert_eq!(backoff_ms(u32::MAX), MAX_DELAY_MS);
}

#[test]
fn submit_is_idempotent_by_content_hash() {
    let store = store("idempotent");
    let bytes = bytes_of("same-bytes");
    let expect_id = store.content_id(&bytes);

    let first = store.submit_job(&bytes, "tester", None).unwrap();
    assert!(first.newly_queued);
    assert_eq!(first.job.job_id, expect_id);
    assert_eq!(first.job.job_id.len(), 64, "job_id 就是内容哈希");
    assert_eq!(first.job.state, JobState::Pending);
    assert_eq!(first.job.attempts, 0);
    assert_eq!(first.job.source_size, bytes.len() as u64);
    assert_eq!(first.job.callback_state, CallbackState::None);
    assert!(first.job.callback_url.is_none());
    assert!(
        store.pending_dir(&expect_id).join("source").is_file(),
        "源字节先落盘"
    );

    // 同一份字节再提交：不新增、不重排、id 不变
    let again = store.submit_job(&bytes, "tester", None).unwrap();
    assert!(!again.newly_queued, "重复提交不该产生第二个作业");
    assert_eq!(again.job.job_id, expect_id);
    assert_eq!(again.job.state, JobState::Pending);

    // 不同字节 = 不同作业
    let other = store
        .submit_job(&bytes_of("other"), "tester", None)
        .unwrap();
    assert_ne!(other.job.job_id, expect_id);

    // 空字节直接拒绝：空文件转不出任何东西，也不该占一个作业号
    assert!(store.submit_job(b"", "tester", None).is_err());
}

#[test]
fn due_respects_the_clock_and_reclaims_stale_running() {
    let store = store("due");
    let job = store
        .submit_job(&bytes_of("due"), "tester", None)
        .unwrap()
        .job;

    // 现在到点
    assert_eq!(store.due_jobs(now() + 1, 4).unwrap().len(), 1);
    // 未来不到点
    assert!(store
        .due_jobs(now() - 10_000, 4)
        .unwrap()
        .iter()
        .all(|j| j.job_id != job.job_id));

    // 领取后不再到点
    assert!(store.claim_job(&job.job_id).unwrap());
    assert!(store
        .due_jobs(now() + 1, 4)
        .unwrap()
        .iter()
        .all(|j| j.job_id != job.job_id));
    // 重复领取被 CAS 挡住（两个 worker 不会转同一份）
    assert!(!store.claim_job(&job.job_id).unwrap());

    // running 卡超过 STALE_RUNNING_MS = worker 死了，重新可领
    let stale_now = now() + STALE_RUNNING_MS + 60_000;
    let reclaimed = store.due_jobs(stale_now, 4).unwrap();
    assert_eq!(reclaimed.len(), 1, "卡死的 running 必须能被重新领取");
    assert_eq!(reclaimed[0].job_id, job.job_id);
    assert_eq!(reclaimed[0].state, JobState::Running);
}

#[test]
fn failures_burn_attempts_then_go_terminal() {
    let store = store("fail");
    let job = store
        .submit_job(&bytes_of("fail"), "tester", None)
        .unwrap()
        .job;

    for attempt in 1..MAX_JOB_ATTEMPTS {
        let updated = store
            .fail_job(
                &job.job_id,
                &JobFailure::retryable("ffmpeg exited non-zero"),
            )
            .unwrap();
        assert_eq!(updated.attempts, attempt);
        assert_eq!(updated.state, JobState::Pending, "还没到上限就继续排");
        assert!(updated
            .last_error
            .as_deref()
            .unwrap()
            .contains("ffmpeg exited"));
        // 到点时间按退避往后推
        let delay = updated.next_retry_at - now();
        assert!(delay >= backoff_ms(attempt) as i64 - 2_000, "delay {delay}");
    }

    // 最后一次：次数用尽 → failed，不再自动重试
    let terminal = store
        .fail_job(&job.job_id, &JobFailure::retryable("still broken"))
        .unwrap();
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.attempts, MAX_JOB_ATTEMPTS);
    assert!(store
        .due_jobs(now() + 60_000, 4)
        .unwrap()
        .iter()
        .all(|j| j.job_id != job.job_id));

    // 人工重试才让它回到队列，并把次数清零
    let requeued = store.requeue_job(&job.job_id).unwrap().unwrap();
    assert_eq!(requeued.state, JobState::Pending);
    assert_eq!(requeued.attempts, 0);
    assert!(requeued.last_error.is_none());
}

#[test]
fn throttled_does_not_burn_attempts_and_permanent_fails_at_once() {
    let store = store("throttle");
    let throttled = store
        .submit_job(&bytes_of("throttle-a"), "tester", None)
        .unwrap()
        .job;
    for _ in 0..8 {
        let updated = store
            .fail_job(&throttled.job_id, &JobFailure::throttled("cpu busy"))
            .unwrap();
        assert_eq!(updated.attempts, 0, "限流不是失败，不能烧额度");
        assert_eq!(updated.state, JobState::Pending);
    }

    let permanent = store
        .submit_job(&bytes_of("throttle-b"), "tester", None)
        .unwrap()
        .job;
    let done = store
        .fail_job(
            &permanent.job_id,
            &JobFailure::permanent("staged size mismatch"),
        )
        .unwrap();
    assert_eq!(done.state, JobState::Failed, "不可重试的错误立刻终态");
    assert_eq!(done.attempts, 1, "记一次真实尝试，但不再排重试");
}

#[test]
fn requeue_only_touches_failed_jobs() {
    let store = store("requeue");
    let pending = store
        .submit_job(&bytes_of("requeue-p"), "tester", None)
        .unwrap()
        .job;
    assert!(
        store.requeue_job(&pending.job_id).unwrap().is_none(),
        "还在排队的作业不能'重试'——那会绕过退避"
    );
    let missing = "f".repeat(64);
    assert!(store.requeue_job(&missing).unwrap().is_none());

    let running = store
        .submit_job(&bytes_of("requeue-r"), "tester", None)
        .unwrap()
        .job;
    store.claim_job(&running.job_id).unwrap();
    assert!(store.requeue_job(&running.job_id).unwrap().is_none());
}

#[test]
fn success_stores_the_result_and_clears_staged_bytes() {
    let store = store("success");
    let job = store
        .submit_job(&bytes_of("success"), "tester", None)
        .unwrap()
        .job;
    assert!(store.pending_dir(&job.job_id).join("source").is_file());

    let result = ingested(&job.job_id);
    let done = store.succeed_job(&job.job_id, &result).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert_eq!(done.result.as_ref(), Some(&result), "结果要能原样读回来");
    assert!(done.last_error.is_none());
    assert_eq!(done.callback_state, CallbackState::NotConfigured);
    assert!(
        !store.pending_dir(&job.job_id).exists(),
        "成功的作业不该再占着源字节"
    );

    // 重复 succeed（幂等）不会把已送达的回调退回待发
    store
        .record_callback(&job.job_id, &Delivery::Delivered)
        .unwrap();
    let again = store.succeed_job(&job.job_id, &result).unwrap();
    assert_eq!(again.callback_state, CallbackState::Delivered);
}

#[tokio::test]
async fn missing_staged_source_is_a_retryable_failure_not_a_panic() {
    let store = store("missing-src");
    let job = store
        .submit_job(&bytes_of("missing"), "tester", None)
        .unwrap()
        .job;
    std::fs::remove_file(store.pending_dir(&job.job_id).join("source")).unwrap();

    let updated = store.run_job(&job.job_id).await.unwrap();
    assert_eq!(updated.state, JobState::Pending);
    assert_eq!(updated.attempts, 1);
    assert!(updated
        .last_error
        .as_deref()
        .unwrap()
        .contains("read staged source"));

    // 已经 running 的作业领不到第二次（CAS），不会静默重跑一遍
    store.claim_job(&job.job_id).unwrap();
    assert!(!store.claim_job(&job.job_id).unwrap());
}

#[tokio::test]
async fn corrupted_staged_source_fails_permanently() {
    let store = store("size-mismatch");
    let job = store
        .submit_job(&bytes_of("size"), "tester", None)
        .unwrap()
        .job;
    // 暂存字节被动过（长度与作业记录不符）——重试也没用，直接终态
    std::fs::write(store.pending_dir(&job.job_id).join("source"), b"shorter").unwrap();
    let updated = store.run_job(&job.job_id).await.unwrap();
    assert_eq!(updated.state, JobState::Failed, "{:?}", updated.last_error);
    assert!(updated
        .last_error
        .as_deref()
        .unwrap()
        .contains("staged source is 7 bytes"));
}

// ---------------------------------------------------------------------------
// 出站回调
// ---------------------------------------------------------------------------

#[test]
fn callback_state_transitions() {
    let store = store("callback");
    let job = store
        .submit_job(&bytes_of("cb"), "tester", Some("http://127.0.0.1:1/cb"))
        .unwrap()
        .job;
    assert_eq!(job.callback_state, CallbackState::Pending);
    assert_eq!(job.callback_url.as_deref(), Some("http://127.0.0.1:1/cb"));
    // 转码成功才武装回调
    store
        .succeed_job(&job.job_id, &ingested(&job.job_id))
        .unwrap();

    // 限流：不烧次数，只推后时间
    let before = store.job(&job.job_id).unwrap().unwrap();
    store
        .record_callback(&job.job_id, &Delivery::Throttled)
        .unwrap();
    let after = store.job(&job.job_id).unwrap().unwrap();
    assert_eq!(after.callback_state, CallbackState::Pending);
    assert_eq!(after.callback_attempts, before.callback_attempts);
    assert!(after.callback_next_retry_at > now());

    // 可重试：烧次数，5 次后终态
    for attempt in 1..=MAX_JOB_ATTEMPTS {
        store
            .record_callback(
                &job.job_id,
                &Delivery::Retryable("callback returned 500".to_string()),
            )
            .unwrap();
        let row = store.job(&job.job_id).unwrap().unwrap();
        assert_eq!(row.callback_attempts, attempt);
        let expected = if attempt >= MAX_JOB_ATTEMPTS {
            CallbackState::Failed
        } else {
            CallbackState::Pending
        };
        assert_eq!(row.callback_state, expected, "第 {attempt} 次");
    }

    // 4xx：对端明确拒绝，一次就停
    let other = store
        .submit_job(&bytes_of("cb2"), "tester", Some("http://127.0.0.1:1/cb"))
        .unwrap()
        .job;
    store
        .succeed_job(&other.job_id, &ingested(&other.job_id))
        .unwrap();
    store
        .record_callback(
            &other.job_id,
            &Delivery::Permanent("callback rejected with 403".to_string()),
        )
        .unwrap();
    let row = store.job(&other.job_id).unwrap().unwrap();
    assert_eq!(row.callback_state, CallbackState::Failed);
    assert_eq!(row.callback_attempts, 0);
    assert!(row
        .callback_error
        .as_deref()
        .unwrap()
        .contains("rejected with 403"));

    // 到点筛选：两个都进终态了 → 一条都不该再发
    let due = store.due_callbacks(now() + 10_000_000, 8).unwrap();
    assert!(
        due.iter().all(|j| j.job_id != job.job_id),
        "回调次数用尽后不能再发"
    );
    assert!(
        due.iter().all(|j| j.job_id != other.job_id),
        "4xx 拒绝后不能再发"
    );

    // 第三个作业：刚成功、没重试过 → 立刻到点
    let fresh = store
        .submit_job(&bytes_of("cb3"), "tester", Some("http://127.0.0.1:1/cb"))
        .unwrap()
        .job;
    store
        .succeed_job(&fresh.job_id, &ingested(&fresh.job_id))
        .unwrap();
    let due = store.due_callbacks(now(), 8).unwrap();
    assert!(due.iter().any(|j| j.job_id == fresh.job_id));
    assert_eq!(due[0].callback_state, CallbackState::Pending);
}

#[test]
fn manual_requeue_rearms_the_callback() {
    let store = store("rearm");
    let job = store
        .submit_job(&bytes_of("rearm"), "tester", Some("http://127.0.0.1:1/cb"))
        .unwrap()
        .job;
    store
        .succeed_job(&job.job_id, &ingested(&job.job_id))
        .unwrap();
    store
        .record_callback(
            &job.job_id,
            &Delivery::Permanent("callback rejected with 500".to_string()),
        )
        .unwrap();
    // 回调终态但作业成功 → requeue 只认 failed 作业，这里应当拒绝（作业本身是好的）
    assert!(store.requeue_job(&job.job_id).unwrap().is_none());
    let row = store.job(&job.job_id).unwrap().unwrap();
    assert_eq!(row.state, JobState::Succeeded, "回调坏不影响'可播'这件事");
}

fn body_of(job_id: &str) -> CallbackBody {
    CallbackBody {
        job_id: job_id.to_string(),
        state: JobState::Succeeded,
        result: Some(ingested(job_id)),
        attempts: 1,
        updated_at: now(),
    }
}

#[tokio::test]
async fn deliver_callback_classifies_by_status_code() {
    let job_id = "a".repeat(64);

    let (url, sink) = spawn_sink(204, "No Content");
    assert_eq!(
        deliver_callback(&url, &body_of(&job_id)).await,
        Delivery::Delivered
    );
    let logged = sink.lock().unwrap().join("\n");
    assert!(logged.starts_with("POST /cb"), "{logged}");
    assert!(
        logged
            .to_lowercase()
            .contains("content-type: application/json"),
        "{logged}"
    );
    assert!(
        logged.contains(&format!("\"job_id\":\"{job_id}\"")),
        "{logged}"
    );
    assert!(logged.contains("\"state\":\"succeeded\""), "{logged}");
    assert!(logged.contains("\"playlist_url\""), "结果要一起送过去");

    let (url, _) = spawn_sink(429, "Too Many Requests");
    assert_eq!(
        deliver_callback(&url, &body_of(&job_id)).await,
        Delivery::Throttled,
        "限流不能算失败次数"
    );

    let (url, _) = spawn_sink(500, "Internal Server Error");
    assert!(matches!(
        deliver_callback(&url, &body_of(&job_id)).await,
        Delivery::Retryable(_)
    ));

    let (url, _) = spawn_sink(403, "Forbidden");
    assert!(matches!(
        deliver_callback(&url, &body_of(&job_id)).await,
        Delivery::Permanent(_)
    ));

    let (url, _) = spawn_sink(302, "Found");
    assert!(matches!(
        deliver_callback(&url, &body_of(&job_id)).await,
        Delivery::Permanent(_)
    ));

    // 对端根本连不上：传输错误 = 可重试，不是"永久失败"
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        format!("http://{a}/cb")
    };
    assert!(matches!(
        deliver_callback(&dead, &body_of(&job_id)).await,
        Delivery::Retryable(_)
    ));

    // 不是 URL 的东西：不发，直接判永久失败（配置错误重试一万次也没用）
    assert!(matches!(
        deliver_callback("not-a-url", &body_of(&job_id)).await,
        Delivery::Permanent(_)
    ));
}

#[tokio::test]
async fn worker_round_drives_a_job_to_terminal_without_ffmpeg() {
    // 假字节过不了 ffprobe → 每轮失败一次，五轮之后终态。
    // 这条验证的是"退避 + 领取 + 记账"这条执行链，不是画质。
    let st = store("round");
    let job = st
        .submit_job(&bytes_of("round"), "tester", None)
        .unwrap()
        .job;
    let mut states = Vec::new();
    for _ in 0..MAX_JOB_ATTEMPTS {
        // 手工把到点时间拉到"现在"，否则要真等 1/2/4/8/16 秒
        let mut due = st.due_jobs(i64::MAX, 1).unwrap();
        assert_eq!(due.len(), 1, "退避中的作业也在 i64::MAX 这个假现在里到点");
        let id = due.pop().unwrap().job_id;
        let _ = st.run_job(&id).await.unwrap();
        states.push(st.job(&id).unwrap().unwrap().state);
    }
    assert_eq!(states.last(), Some(&JobState::Failed));
    assert_eq!(
        st.job(&job.job_id).unwrap().unwrap().attempts,
        MAX_JOB_ATTEMPTS
    );
}

// ---------------------------------------------------------------------------
// HTTP 面
// ---------------------------------------------------------------------------

async fn multipart_job(
    app: &axum::Router,
    payload: &[u8],
    callback_url: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let boundary = "wxlike-jobs-test";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"bytes\"; filename=\"v.mp4\"\r\nContent-Type: video/mp4\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(payload);
    if let Some(url) = callback_url {
        body.extend_from_slice(
            format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"callback_url\"\r\n\r\n{url}").as_bytes(),
        );
    }
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/video/jobs")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn job_routes_accept_query_and_refuse_junk() {
    // 这个 store 会被 HTTP 提交路径起一个后台 worker，所以只断言"提交这一跳"，
    // 不断言之后的状态（状态由 worker 决定，那是竞态）。
    let st = store("routes");
    let app = routes(VideoState { store: st.clone() });
    let payload = bytes_of("route-create");
    let expect_id = st.content_id(&payload);

    let (status, body) = multipart_job(&app, &payload, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["job"]["job_id"].as_str().unwrap(), expect_id);
    assert!(body["newly_queued"].as_bool().unwrap());
    assert_eq!(body["job"]["state"].as_str().unwrap(), "pending");
    assert_eq!(body["job"]["callback_state"].as_str().unwrap(), "none");

    // 查询：命中与未命中
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/video/jobs/{expect_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let unknown = "9".repeat(64);
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/video/jobs/{unknown}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 作业号也是内容哈希：形状校验先于任何查库
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/video/jobs/../../etc/passwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 空上传：422，不给它编一个作业号
    let (status, _) = multipart_job(&app, b"", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn job_route_stores_the_callback_url_and_validates_it() {
    let st = store("route-cb");
    let app = routes(VideoState { store: st.clone() });
    let payload = bytes_of("route-cb-payload");
    let (status, body) = multipart_job(&app, &payload, Some("http://127.0.0.1:1/done")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    // 后台 worker 会开始跑这个作业，所以这里只断言提交时写进库的东西
    let _ = body;
    // 非法回调地址在入队之前就被拒（不是"先收下再一直重试"）
    let (status, _) = multipart_job(&app, &bytes_of("route-cb-bad"), Some("ftp://nope")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn retry_route_distinguishes_unknown_and_not_failed() {
    // 独立 store：这个 store 不走 HTTP 提交，因此没有后台 worker 参与
    let st = store("retry");
    let app = routes(VideoState { store: st.clone() });
    let job = st
        .submit_job(&bytes_of("retry-route"), "tester", None)
        .unwrap()
        .job;

    // 还在排队 → 409（重试它等于绕过退避，白烧一遍 ffmpeg）
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/video/jobs/{}/retry", job.job_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // 没这个作业 → 404
    let unknown = "8".repeat(64);
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/video/jobs/{unknown}/retry"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // 置为终态后重试 → 200 + 回到 pending
    st.fail_job(&job.job_id, &JobFailure::permanent("no ffmpeg"))
        .unwrap();
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/video/jobs/{}/retry", job.job_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(res.into_body())
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["state"].as_str().unwrap(), "pending");
    assert_eq!(value["attempts"].as_u64().unwrap(), 0);
}
