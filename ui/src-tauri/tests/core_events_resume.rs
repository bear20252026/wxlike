//! #296 / G01：`core_subscribe` 的游标恢复（resume）。
//!
//! 基线上的 `core_subscribe` 只把入参游标原样回显（`commands.rs:26`），断线重连
//! 的客户端一条漏掉的事件都拿不到。这里把补发行为钉死：
//!
//! * 不丢：同进程内卸载重订（webview 刷新/断线）期间产生的事件全部补回；
//! * 不重：同一游标第二次订阅不再二次投递；`event_id` 带进程 epoch，重启后
//!   也不会与上一个进程发过的撞号；
//! * 不装：跨重启（环形日志随进程消失）与日志被挤掉的窗口，如实报 `gap`
//!   + `reason` + 该重拉的 action 列表，而不是回一个"订阅成功"糊过去。
//!
//! 事件由**真实生产者** [`forward_inbound`]（收包泵就是调它）产生，不是手搓
//! 的信封；因此不需要服务端与登录态。

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{Listener as _, Manager as _};
use wxlike_ui_lib::core_bridge::{forward_inbound, subscribe, CoreState};

type MockApp = tauri::App<tauri::test::MockRuntime>;
type MockHandle = tauri::AppHandle<tauri::test::MockRuntime>;

fn boot() -> (MockApp, MockHandle) {
    let app = tauri::test::mock_app();
    app.manage(CoreState::new());
    let handle = app.handle().clone();
    (app, handle)
}

/// 收包泵的一条入站消息（形状取自 render.rs 的 payload）。
fn inbound(msg: &str, seq: u64, conv: &str, sender: &str) -> Value {
    json!({
        "conv_id": conv, "kind": "text", "text": msg, "seq": seq,
        "sent_at_ms": 1_700_000_000_000i64 + seq as i64,
        "msg_id": format!("m-{seq}"), "sender": sender,
    })
}

/// 已收到的信封（webview 侧视角）。
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Value>>>);

impl Capture {
    /// 订阅 `core://event`，返回 unlisten 用的 id。
    fn attach(&self, handle: &MockHandle) -> tauri::EventId {
        let sink = self.0.clone();
        handle.listen("core://event", move |event| {
            if let Ok(envelope) = serde_json::from_str::<Value>(event.payload()) {
                sink.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(envelope);
            }
        })
    }

    fn len(&self) -> usize {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    fn ids(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|e| e["event_id"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    fn tail_from(&self, skip: usize) -> Vec<Value> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .skip(skip)
            .cloned()
            .collect()
    }

    fn last_cursor(&self) -> String {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last()
            .and_then(|e| e["cursor"]["cursor"].as_str())
            .unwrap_or_else(|| panic!("信封要带 cursor 字段，调用方才有可能续订"))
            .to_string()
    }
}

fn core_of(handle: &MockHandle) -> tauri::State<'_, CoreState> {
    handle.state::<CoreState>()
}

fn resume(handle: &MockHandle, cursor: Option<Value>) -> Value {
    let core = core_of(handle);
    subscribe(handle, &core, cursor)
}

/// 一条游标都还没带 = 冷订阅：不该补发任何东西，但要给出可续订的基线。
#[test]
fn fresh_subscribe_establishes_a_baseline_and_replays_nothing() {
    let (_app, handle) = boot();
    let capture = Capture::default();
    let _id = capture.attach(&handle);
    forward_inbound(&handle, &inbound("第一条", 1, "d-a~b", "b"), Some("a"));

    let reply = resume(&handle, None);
    assert_eq!(reply["subscribed"], json!(true), "回执: {reply}");
    assert_eq!(reply["stream_id"], json!("core"));
    assert_eq!(
        reply["head"],
        json!(2),
        "一条入站消息 = message.created + conversation.upsert"
    );
    assert_eq!(reply["replayed"], json!(0), "冷订阅不该补发");
    assert_eq!(reply["gap"], json!(false), "冷订阅没有洞");
    assert_eq!(capture.len(), 2, "基线不该重复投递");
    assert!(
        reply["cursor"]["cursor"].as_str().is_some(),
        "回执要带可续订的游标"
    );
}

/// 核心用例：断线期间产生的事件，重订后一条不少地补回来。
#[test]
fn resume_replays_everything_missed_while_unsubscribed() {
    let (_app, handle) = boot();
    let capture = Capture::default();
    let id = capture.attach(&handle);
    forward_inbound(
        &handle,
        &inbound("在线时收到的", 1, "d-a~b", "b"),
        Some("a"),
    );
    // 记下"我已经收到这儿了"，然后卸载订阅（webview 刷新）。
    let cursor = capture.last_cursor();
    handle.unlisten(id);

    // 断线期间到的两条：客户端一条都没收到，但桥已经发过并记进了日志。
    forward_inbound(
        &handle,
        &inbound("漏掉的第一条", 2, "d-a~b", "b"),
        Some("a"),
    );
    forward_inbound(
        &handle,
        &inbound("漏掉的第二条", 3, "d-a~b", "b"),
        Some("a"),
    );
    assert_eq!(capture.len(), 2, "重订之前不该有投递");

    let _id2 = capture.attach(&handle);
    let reply = resume(
        &handle,
        Some(json!({ "stream_id": "core", "cursor": cursor })),
    );
    assert_eq!(
        reply["replayed"],
        json!(4),
        "两条漏掉的 × 2 个事件: {reply}"
    );
    assert_eq!(reply["gap"], json!(false), "同进程内不该有洞: {reply}");
    assert_eq!(reply["reason"], Value::Null);
    assert_eq!(capture.len(), 6, "补发要真的 emit 出去");

    let replayed = capture.tail_from(2);
    let texts: Vec<&str> = replayed
        .iter()
        .map(|e| {
            e["payload"]["body"]["text"]
                .as_str()
                .or(e["payload"]["last_preview"].as_str())
                .unwrap_or_default()
        })
        .collect();
    assert!(
        texts.contains(&"漏掉的第一条") && texts.contains(&"漏掉的第二条"),
        "补发的内容要是那两条漏掉的消息，实际: {texts:?}"
    );
    assert!(
        !texts.contains(&"在线时收到的"),
        "游标之前的事件不该再补一遍: {texts:?}"
    );
}

/// 同一游标反复订阅（重连风暴 / 组件反复挂载）不能二次投递。
#[test]
fn resubscribing_with_the_same_cursor_does_not_double_deliver() {
    let (_app, handle) = boot();
    let capture = Capture::default();
    let id = capture.attach(&handle);
    forward_inbound(&handle, &inbound("第一条", 1, "d-a~b", "b"), Some("a"));
    let cursor = capture.last_cursor();
    handle.unlisten(id);

    forward_inbound(&handle, &inbound("第二条", 2, "d-a~b", "b"), Some("a"));
    let _id2 = capture.attach(&handle);
    let first = resume(&handle, Some(json!(cursor.clone())));
    assert_eq!(first["replayed"], json!(2), "第一次订阅补上漏掉的: {first}");
    let after_first = capture.len();

    let second = resume(&handle, Some(json!(cursor.clone())));
    assert_eq!(
        second["replayed"],
        json!(0),
        "同一游标再来一次不该重投: {second}"
    );
    assert_eq!(capture.len(), after_first, "一次订阅都没多出来");

    // 但游标推进后仍然照常补发（去重不能变成"补发被永久关掉"）。
    forward_inbound(&handle, &inbound("第三条", 3, "d-a~b", "b"), Some("a"));
    let third = resume(&handle, Some(json!(cursor)));
    assert_eq!(
        third["replayed"],
        json!(2),
        "游标之后又有新事件仍要补: {third}"
    );
}

/// 重启（另一个进程/CoreState 的游标 + 空日志）：补不出就如实报 gap，
/// 并告诉调用方去重拉哪些投影。这是 #296 允许的"诚实最小实现"。
#[test]
fn a_cursor_from_a_previous_boot_reports_a_gap_instead_of_lying() {
    // "上一个进程"：boot A 发一条事件，拿它的游标。
    let (_app_a, handle_a) = boot();
    let capture_a = Capture::default();
    let _id = capture_a.attach(&handle_a);
    forward_inbound(
        &handle_a,
        &inbound("重启前的消息", 1, "d-a~b", "b"),
        Some("a"),
    );
    let stale_cursor = capture_a.last_cursor();

    // "重启后的进程"：全新 CoreState，日志是空的。
    let (_app_b, handle_b) = boot();
    let reply = resume(
        &handle_b,
        Some(json!({ "stream_id": "core", "cursor": stale_cursor })),
    );
    assert_eq!(
        reply["subscribed"],
        json!(true),
        "仍然订阅成功（此后的事件照发）"
    );
    assert_eq!(
        reply["replayed"],
        json!(0),
        "重启前的日志已经不在了，补不出任何东西"
    );
    assert_eq!(reply["gap"], json!(true), "必须承认有洞");
    assert_eq!(reply["reason"], json!("stream_reset"), "洞的原因: {reply}");
    let refetch = reply["refetch"].as_array().expect("refetch 要是数组");
    assert!(
        refetch.contains(&json!("messaging.conversations.list"))
            && refetch.contains(&json!("messaging.messages.page")),
        "要给出可执行的重拉清单: {reply}"
    );

    // 重启后新发的事件仍然正常投递（gap 不等于订阅失效）。
    let capture_b = Capture::default();
    let _id_b = capture_b.attach(&handle_b);
    forward_inbound(
        &handle_b,
        &inbound("重启后的消息", 1, "d-a~b", "b"),
        Some("a"),
    );
    assert_eq!(capture_b.len(), 2, "重启后新事件照常投递");
}

/// 防重复投递的另一半：序号会在重启后重新从 1 数，所以 `event_id` 必须带
/// epoch，否则客户端的按 id 幂等合并会把新事件误判成旧事件丢掉。
#[test]
fn event_ids_stay_unique_across_boots() {
    let (_app_a, handle_a) = boot();
    forward_inbound(&handle_a, &inbound("重启前", 1, "d-a~b", "b"), Some("a"));
    let capture_a = Capture::default();
    let _id = capture_a.attach(&handle_a);
    forward_inbound(
        &handle_a,
        &inbound("重启前第二条", 2, "d-a~b", "b"),
        Some("a"),
    );
    let ids_a = capture_a.ids();

    let (_app_b, handle_b) = boot();
    let capture_b = Capture::default();
    let _id = capture_b.attach(&handle_b);
    forward_inbound(&handle_b, &inbound("重启后", 1, "d-a~b", "b"), Some("a"));
    let ids_b = capture_b.ids();

    let head_a: &str = ids_a[0].as_str();
    let head_b: &str = ids_b[0].as_str();
    assert!(
        head_a != head_b,
        "两个进程的第 1 号事件撞了 id（{head_a}），重启后客户端会把新事件当重复丢掉"
    );
    // 同一进程内也要唯一（seq 单调）。
    let mut unique = ids_a.clone();
    unique.extend(ids_b.clone());
    let before = unique.len();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), before, "同一批事件里 id 要互不相同");
}

/// 日志被挤满：能补的那段照样补，补不掉的洞要如实报出来。
#[test]
fn a_truncated_journal_still_replays_what_it_kept() {
    let (_app, handle) = boot();
    let capture = Capture::default();
    let _id = capture.attach(&handle);
    forward_inbound(&handle, &inbound("最早的一条", 1, "d-a~b", "b"), Some("a"));
    let oldest_cursor = capture.last_cursor();

    let capacity = resume(&handle, None)["journal_capacity"]
        .as_u64()
        .expect("要报日志容量") as usize;
    // 灌到容量之上（一条入站消息产 2 个事件）。
    for n in 0..(capacity + 4) {
        forward_inbound(
            &handle,
            &inbound("灌满用", 100 + n as u64, "d-a~b", "b"),
            Some("a"),
        );
    }
    let seen_so_far = capture.len();
    let reply = resume(&handle, Some(json!(oldest_cursor)));
    assert_eq!(
        reply["gap"],
        json!(true),
        "最旧那段已经被挤掉，必须报洞: {reply}"
    );
    assert_eq!(reply["reason"], json!("journal_truncated"));
    assert_eq!(
        reply["replayed"].as_u64(),
        Some(capacity as u64),
        "还留在日志里的 {capacity} 条要全部补回: {reply}"
    );
    assert!(
        reply["journal_available_from"].as_u64().unwrap() > 1,
        "日志起点的游标: {reply}"
    );
    assert!(
        capture.len() > seen_so_far,
        "报洞不等于什么都不发——补得到的部分照样要发"
    );
}

/// 畸形/超前游标不能被静默吞掉，也不能假装补发成功。
#[test]
fn unusable_cursors_are_reported_not_swallowed() {
    let (_app, handle) = boot();
    forward_inbound(&handle, &inbound("第一条", 1, "d-a~b", "b"), Some("a"));

    let bogus = resume(
        &handle,
        Some(json!({ "stream_id": "core", "cursor": "不是游标" })),
    );
    assert_eq!(bogus["gap"], json!(true));
    assert_eq!(
        bogus["reason"],
        json!("invalid_cursor"),
        "认不出的游标: {bogus}"
    );
    assert_eq!(bogus["replayed"], json!(0));

    let other_stream = resume(
        &handle,
        Some(json!({ "stream_id": "voice", "cursor": "1" })),
    );
    assert_eq!(
        other_stream["reason"],
        json!("invalid_cursor"),
        "别的流: {other_stream}"
    );

    let ahead = resume(&handle, Some(json!("999999")));
    assert_eq!(ahead["gap"], json!(true));
    assert_eq!(ahead["reason"], json!("cursor_ahead"), "超前游标: {ahead}");

    // 裸序号（不带 epoch）= 按当前进程解释，能正常补发。
    let numeric = resume(&handle, Some(json!(0u64)));
    assert_eq!(numeric["gap"], json!(false));
    assert_eq!(
        numeric["replayed"],
        json!(2),
        "从 0 号补到当前 head: {numeric}"
    );
}
