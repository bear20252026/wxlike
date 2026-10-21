//! E3 端到端：真 ffmpeg 走完整条流水线（提交 → 阶梯转码 → 清单 → 回调）。
//!
//! 需要 ffmpeg：`cargo test -p wxlike-video --test jobs_pipeline -- --ignored`
//! （仓库里没有二进制 fixture，样片由 ffmpeg 现合成）。

mod support;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use std::time::Duration;
use support::{make_sample_video, require_tools, spawn_sink, temp_root};
use tower::ServiceExt as _;
use wxlike_video::{
    routes, CallbackState, Ingested, Job, JobState, VideoState, VideoStore, MAX_JOB_ATTEMPTS,
};

fn state(tag: &str) -> VideoState {
    VideoState {
        store: VideoStore::new(temp_root(tag)),
    }
}

async fn poll_until_done(store: &VideoStore, job_id: &str, rounds: u32) -> Job {
    for _ in 0..rounds {
        // 自己驱动轮次，不等后台 worker：这样断言的是"这一轮之后作业到哪一步"
        store.run_worker_round().await.unwrap();
        let job = store.job(job_id).unwrap().unwrap();
        if matches!(job.state, JobState::Succeeded | JobState::Failed) {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("job {job_id} never reached a terminal state within {rounds} rounds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs ffmpeg: cargo test -p wxlike-video --test jobs_pipeline -- --ignored"]
async fn submitted_job_transcodes_becomes_playable_and_calls_back() {
    require_tools();
    let st = state("pipeline");
    let src_dir = temp_root("pipeline-src");
    // 720p 源 → 阶梯应产出 360 与 720 两档
    let sample = std::fs::read(make_sample_video(&src_dir, 1280, 720, 3, "hd.mp4")).unwrap();
    let job_id = st.store.content_id(&sample);

    // 提交（异步面）：立刻返回，不举着连接等 ffmpeg
    let (url, sink) = spawn_sink(200, "OK");
    let submit = st.store.submit_job(&sample, Some(url.as_str())).unwrap();
    assert!(submit.newly_queued);
    assert_eq!(submit.job.state, JobState::Pending);

    // 跑完整条链
    let job = poll_until_done(&st.store, &job_id, 40).await;
    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.last_error);
    let result = job.result.clone().expect("succeeded job carries a result");
    let heights: Vec<u32> = result.renditions.iter().map(|r| r.height).collect();
    assert_eq!(heights, vec![360, 720], "阶梯只降不升");
    assert_eq!(result.duration_ms, 3_000);

    // 产物齐不齐：对象目录存在、play.mp4 有、每档都有清单
    assert!(st.store.contains(&job_id));
    let play = st.store.dir_public(&job_id).join("play.mp4");
    assert!(play.is_file());
    assert!(std::fs::metadata(&play).unwrap().len() > 1_000);
    for height in &heights {
        let playlist = st.store.rendition_playlist(&job_id, *height).unwrap();
        assert!(String::from_utf8_lossy(&playlist).contains("seg_"));
    }
    let master = String::from_utf8(st.store.playlist(&job_id).unwrap()).unwrap();
    assert!(master.contains("720/playlist.m3u8"), "{master}");
    // 暂存字节在成功后被清掉
    assert!(!st.store.pending_dir(&job_id).exists());

    // 播放面：`<video>` 拖进度条那一下
    let app = routes(st.clone());
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/video/{job_id}/play.mp4"))
                .header(header::RANGE, "bytes=0-1023")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        res.headers()[header::CONTENT_RANGE].to_str().unwrap(),
        format!("bytes 0-1023/{}", std::fs::metadata(&play).unwrap().len())
    );

    // 回调：作业成功后 worker 的一轮把结果 POST 出去
    st.store.run_worker_round().await.unwrap();
    let delivered = st.store.job(&job_id).unwrap().unwrap();
    assert_eq!(delivered.callback_state, CallbackState::Delivered);
    let logged = sink.lock().unwrap().join("\n");
    assert!(logged.starts_with("POST /cb"), "{logged}");
    assert!(
        logged.contains(&format!("\"job_id\":\"{job_id}\"")),
        "{logged}"
    );
    assert!(
        logged.contains("playlist.m3u8"),
        "回调要带上播放地址: {logged}"
    );

    // 同一份字节再提交：直接可播，不再转一遍
    let again = st.store.submit_job(&sample, None).unwrap();
    assert!(!again.newly_queued);
    assert_eq!(again.job.state, JobState::Succeeded);
    assert_eq!(again.job.result.as_ref(), Some(&result));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs ffmpeg: cargo test -p wxlike-video --test jobs_pipeline -- --ignored"]
async fn garbage_upload_fails_with_a_readable_error_and_can_be_retried() {
    require_tools();
    let st = state("pipeline-garbage");
    let job = st
        .store
        .submit_job(b"this is not a video", None)
        .unwrap()
        .job;

    let mut last = job.clone();
    for _ in 0..MAX_JOB_ATTEMPTS {
        last = st.store.run_job(&job.job_id).await.unwrap();
        if last.state == JobState::Failed {
            break;
        }
        // 退避：把作业的到点时间拉到"未来"，不等真时间
        let due = st.store.due_jobs(i64::MAX, 4).unwrap();
        if let Some(next) = due.first() {
            last = st.store.job(&next.job_id).unwrap().unwrap();
        }
    }
    assert_eq!(last.state, JobState::Failed, "{last:?}");
    let error = last.last_error.clone().unwrap();
    assert!(
        error.contains("ffprobe") || error.contains("ffmpeg"),
        "错误文本要指出是哪一步坏了: {error}"
    );
    assert!(
        !st.store.contains(&job.job_id),
        "失败的作业不能在对象目录里留下半个对象"
    );
    // 终态保留源字节，人工重试不需要客户端重传
    assert!(st.store.pending_dir(&job.job_id).join("source").is_file());
    let requeued = st.store.requeue_job(&job.job_id).unwrap().unwrap();
    assert_eq!(requeued.state, JobState::Pending);
    assert_eq!(requeued.attempts, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs ffmpeg: cargo test -p wxlike-video --test jobs_pipeline -- --ignored"]
async fn job_result_and_sync_upload_agree_on_every_field() {
    require_tools();
    let st = state("pipeline-agree");
    let src_dir = temp_root("pipeline-agree-src");
    let sample = std::fs::read(make_sample_video(&src_dir, 640, 360, 2, "sd.mp4")).unwrap();

    // 同一条字节：同步上传（老路径）与作业（新路径）必须给出完全一致的引用
    let uploaded: Ingested = st.store.ingest(&sample).await.unwrap();
    let submit = st.store.submit_job(&sample, None).unwrap();
    let job = poll_until_done(&st.store, &submit.job.job_id, 10).await;
    assert_eq!(job.state, JobState::Succeeded);
    assert_eq!(
        job.result.as_ref(),
        Some(&uploaded),
        "两条路不能给出两套地址"
    );
}
