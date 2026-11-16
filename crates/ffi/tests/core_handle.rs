//! `CoreHandle` 的生命周期契约（G01-04）。
//!
//! 三条必须成立的性质，否则 Flutter 侧会踩到"用了已销毁的句柄"或"卡住
//! worker isolate"：
//!
//! 1. `dispose()` 之后 `dispatch` 报 `disposed`，`poll` 立刻返回空数组；
//! 2. `cancel()` 之后该请求不会再产出结果（产出的是 `cancelled`）；
//! 3. 非法 JSON / 缺字段一律 `invalid`，不会 panic 穿过 FFI。

use std::path::Path;

use wxlike_ffi::{CoreHandle, WxlikeError};

fn config(dir: &Path) -> String {
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
    let handle = CoreHandle::open(config(dir.path())).expect("打开");
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
    let handle = CoreHandle::open(config(dir.path())).expect("打开");
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
    let handle = CoreHandle::open(config(dir.path())).expect("打开");
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
    let handle = CoreHandle::open(config(dir.path())).expect("打开");
    handle.dispose();
    handle.dispose();
    assert_eq!(handle.pending(), 0);
}

/// S2：帧是类型化 serde 产物，不是旧的手写 json!。
/// 网络不可达 → 传输层错误走 kind=error 帧，error 是规范 DomainError
/// （code 为 serde tag，含 message），retryable 独立标注。旧形状把 error
/// 拍平成顶层 code/message——这里 error 必须是嵌套对象。
#[test]
fn error_frame_carries_canonical_domain_error() {
    let dir = tempfile::tempdir().expect("临时目录");
    let handle = CoreHandle::open(config(dir.path())).expect("打开");
    let request = serde_json::json!({
        "action_id": "me.info",
        "params": {},
        "trace_id": "t-frame",
        "contract": "1.0",
    })
    .to_string();
    handle.dispatch(request).expect("投递");
    let batch = handle.poll(5_000);
    let frames: Vec<serde_json::Value> = serde_json::from_str(&batch).expect("帧数组");
    assert!(!frames.is_empty(), "应至少有一帧");
    let frame = &frames[0];
    // 传输层字段。
    assert_eq!(frame["kind"], "error", "{frame}");
    assert!(
        frame["request_id"].as_str().unwrap().starts_with("req-"),
        "{frame}"
    );
    assert!(frame["retryable"].is_boolean(), "{frame}");
    // 规范 DomainError：嵌套对象，code 为 tag，含 message。
    let error = &frame["error"];
    assert!(
        error.is_object(),
        "error 必须是规范 DomainError 对象：{frame}"
    );
    assert!(error["code"].is_string(), "error.code 缺失：{frame}");
    assert!(error["message"].is_string(), "error.message 缺失：{frame}");
    // 旧手写形状把 code/message 拍平到顶层——必须消失。
    assert!(frame.get("code").is_none(), "不应再有顶层 code：{frame}");
    assert!(
        frame.get("payload").is_none(),
        "不应再有 payload 包裹：{frame}"
    );
    handle.dispose();
}
