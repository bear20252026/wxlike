//! `CoreHandle` 的生命周期契约（G01-04）。
//!
//! 三条必须成立的性质，否则 Flutter 侧会踩到"用了已销毁的句柄"或"卡住
//! worker isolate"：
//!
//! 1. `dispose()` 之后 `dispatch` 报 `disposed`，`poll` 立刻返回空数组；
//! 2. `cancel()` 之后该请求不会再产出结果（产出的是 `cancelled`）；
//! 3. 非法 JSON / 缺字段一律 `invalid`，不会 panic 穿过 FFI。

use std::path::PathBuf;

use wxlike_ffi::{CoreHandle, WxlikeError};

fn config(dir: &PathBuf) -> String {
    serde_json::json!({
        "base_url": "http://127.0.0.1:1",
        "data_dir": dir.to_string_lossy(),
    })
    .to_string()
}

#[test]
fn invalid_config_is_rejected_without_panicking() {
    let err = CoreHandle::open("not json".to_string()).expect_err("必须失败");
    assert!(matches!(err, WxlikeError::Invalid { .. }), "{err:?}");
    let err = CoreHandle::open("{}".to_string()).expect_err("缺字段");
    assert!(matches!(err, WxlikeError::Invalid { .. }), "{err:?}");
}

#[test]
fn dispose_rejects_late_calls_and_poll_does_not_block() {
    let dir = tempfile::tempdir().expect("临时目录");
    let handle = CoreHandle::open(config(&dir.path().to_path_buf())).expect("打开");
    handle.dispose();
    let err = handle
        .dispatch("{}".to_string())
        .expect_err("销毁后必须拒绝");
    assert!(matches!(err, WxlikeError::Disposed { .. }), "{err:?}");
    let started = std::time::Instant::now();
    let batch = handle.poll(5_000);
    assert_eq!(batch, "[]");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "销毁后 poll 不该等满超时"
    );
}

#[test]
fn dispatch_returns_a_request_id_immediately() {
    let dir = tempfile::tempdir().expect("临时目录");
    let handle = CoreHandle::open(config(&dir.path().to_path_buf())).expect("打开");
    let request = serde_json::json!({
        "action_id": "me.info",
        "params": {},
        "trace_id": "t-1",
        "contract": "1.0",
    })
    .to_string();
    let request_id = handle.dispatch(request).expect("投递");
    assert!(request_id.starts_with("req-"), "{request_id}");
    // 网络不可达：最终会拿到一条 unavailable 的错误结果
    let batch = handle.poll(3_000);
    assert!(batch.contains("\"kind\""), "{batch}");
    handle.dispose();
}

#[test]
fn cancel_before_the_worker_runs_yields_cancelled() {
    let dir = tempfile::tempdir().expect("临时目录");
    let handle = CoreHandle::open(config(&dir.path().to_path_buf())).expect("打开");
    let request = serde_json::json!({
        "action_id": "me.info",
        "params": {},
        "trace_id": "t-2",
        "contract": "1.0",
    })
    .to_string();
    let request_id = handle.dispatch(request).expect("投递");
    handle.cancel(request_id.clone());
    let batch = handle.poll(3_000);
    assert!(batch.contains("cancelled"), "{batch}");
    handle.dispose();
}

#[test]
fn dispose_is_idempotent() {
    let dir = tempfile::tempdir().expect("临时目录");
    let handle = CoreHandle::open(config(&dir.path().to_path_buf())).expect("打开");
    handle.dispose();
    handle.dispose();
    assert_eq!(handle.pending(), 0);
}
