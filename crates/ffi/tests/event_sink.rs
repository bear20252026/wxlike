//! D3 / ADR-29：`EventSink` 回调推送的契约。
//!
//! 回调是对"刻意无回调"旧设计的显式扩展，必须守住几条性质：
//!
//! 1. **扇出而非替换**：注册 sink 后，结果既推给回调，也仍进 `poll` 队列；
//! 2. **销毁后拒绝晚到**：`dispose` 之后 `subscribe` 报 `disposed`；
//! 3. **多 sink 都收到**：注册多个回调，每条事件每个都拿到；
//! 4. **不反向阻塞**：回调在专用分发线程触发（慢回调不卡 dispatch）。

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wxlike_ffi::{CoreHandle, EventSink, WxlikeError};

fn config(dir: &Path) -> String {
    serde_json::json!({
        "base_url": "http://127.0.0.1:1",
        "data_dir": dir.to_string_lossy(),
    })
    .to_string()
}

fn request(trace: &str) -> String {
    serde_json::json!({
        "action_id": "me.info",
        "params": {},
        "trace_id": trace,
        "contract": "1.0",
    })
    .to_string()
}

/// 收集型回调：把收到的事件 JSON 存起来。
#[derive(Default)]
struct CollectingSink {
    events: Mutex<Vec<String>>,
}

impl EventSink for CollectingSink {
    fn on_event(&self, event_json: String) {
        self.events.lock().unwrap().push(event_json);
    }
}

/// 计数型回调：只数收到几条（验多 sink / 不阻塞）。
#[derive(Default)]
struct CountingSink {
    count: AtomicUsize,
}

impl EventSink for CountingSink {
    fn on_event(&self, _event_json: String) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

/// 把共享实例装进 Box 注册（Box 内是同一个 Arc，断言仍引用原实例）。
struct ArcSink<T: EventSink>(Arc<T>);
impl<T: EventSink> EventSink for ArcSink<T> {
    fn on_event(&self, event_json: String) {
        self.0.on_event(event_json);
    }
}

/// 等到谓词成立或超时（回调在别的线程，需要轮询等待）。
fn wait_until(timeout: Duration, mut pred: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if pred() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    pred()
}

#[test]
fn subscribe_receives_pushed_result_and_poll_still_works() {
    let dir = tempfile::tempdir().expect("临时目录");
    let handle = CoreHandle::open(config(dir.path())).expect("打开");
    let sink = Arc::new(CollectingSink::default());
    handle
        .subscribe(Box::new(ArcSink(sink.clone())))
        .expect("注册回调");

    handle.dispatch(request("d3-1")).expect("投递");

    // 回调应收到一条事件（网络不可达 → unavailable 错误结果）。
    let got = wait_until(Duration::from_secs(5), || {
        !sink.events.lock().unwrap().is_empty()
    });
    assert!(got, "回调应收到推送的事件");
    let events = sink.events.lock().unwrap();
    assert!(events[0].contains("\"kind\""), "{}", events[0]);

    // 扇出而非替换：poll 仍能拿到同一条（兜底）。
    let batch = handle.poll(3_000);
    assert!(batch.contains("\"kind\""), "poll 兜底应仍有结果：{batch}");
    handle.dispose();
}

#[test]
fn subscribe_after_dispose_is_rejected() {
    let dir = tempfile::tempdir().expect("临时目录");
    let handle = CoreHandle::open(config(dir.path())).expect("打开");
    handle.dispose();
    let err = handle
        .subscribe(Box::new(CollectingSink::default()))
        .expect_err("销毁后必须拒绝");
    assert!(matches!(err, WxlikeError::Disposed { .. }), "{err:?}");
}

#[test]
fn multiple_sinks_each_receive_the_event() {
    let dir = tempfile::tempdir().expect("临时目录");
    let handle = CoreHandle::open(config(dir.path())).expect("打开");
    let a = Arc::new(CountingSink::default());
    let b = Arc::new(CountingSink::default());
    handle
        .subscribe(Box::new(ArcSink(a.clone())))
        .expect("注册 a");
    handle
        .subscribe(Box::new(ArcSink(b.clone())))
        .expect("注册 b");

    handle.dispatch(request("d3-multi")).expect("投递");

    let got = wait_until(Duration::from_secs(5), || {
        a.count.load(Ordering::SeqCst) >= 1 && b.count.load(Ordering::SeqCst) >= 1
    });
    assert!(got, "两个回调都应收到事件");
    handle.dispose();
}

#[test]
fn slow_sink_does_not_block_dispatch() {
    // 慢回调（每条 sleep）不该卡住 dispatch 的返回——回调在专用线程触发。
    struct SlowSink;
    impl EventSink for SlowSink {
        fn on_event(&self, _event_json: String) {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    let dir = tempfile::tempdir().expect("临时目录");
    let handle = CoreHandle::open(config(dir.path())).expect("打开");
    handle.subscribe(Box::new(SlowSink)).expect("注册慢回调");

    let start = Instant::now();
    for i in 0..5 {
        handle
            .dispatch(request(&format!("d3-slow-{i}")))
            .expect("投递");
    }
    // dispatch 只是入队，应远快于 5×300ms 的回调耗时。
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "dispatch 不该被慢回调反向阻塞：{:?}",
        start.elapsed()
    );
    handle.dispose();
}
