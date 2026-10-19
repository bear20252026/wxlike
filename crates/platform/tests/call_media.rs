//! 端到端：**SDP 走我们的信令中继，媒体走虚拟线路**。
//!
//! 这是"媒体面接通"的真正验收点。`rtc/tests/media_loopback.rs` 证明的是
//! str0m 两端能直连；这里要证明的是：把 offer/answer 塞进
//! `POST /api/call/{conv}/signal` 再从 `GET` 取出来，双方仍然能完成
//! ICE + DTLS + SRTP 并互相收到音频——也就是说**中继的序列化/权限/队列
//! 不会破坏协商**（SDP 是 JSON 透传，最容易在编码层悄悄坏掉）。
//!
//! 数据报仍走 `pump` 的进程内虚拟线路：CI 里不绑真实 UDP 端口，避免
//! 网络策略把测试弄成 flaky。真实 socket 的连通性由 rtc 自己的
//! `media_connects_over_real_loopback_udp` 覆盖，两者互补。

mod common;

use std::time::Duration;

use common::{platform, signup};
use serde_json::json;
use str0m::Event;
use wxlike_platform::Platform;
use wxlike_rtc::media::{pump, MediaSession};
use wxlike_rtc::signal::{Signal, SignalContent};

/// 把一条信号写成 HTTP 能吃的 JSON（模拟客户端的做法）。
/// `invite` 必须带 `video` 字段——`SignalContent::Invite` 的必填项。
fn as_body(
    kind: &str,
    call_id: &str,
    from: &str,
    to: &str,
    sdp: serde_json::Value,
    video: bool,
) -> serde_json::Value {
    let content = if kind == "invite" {
        json!({"type": kind, "sdp": sdp, "video": video})
    } else {
        json!({"type": kind, "sdp": sdp})
    };
    json!({"v": 1, "call_id": call_id, "from": from, "to": to, "content": content})
}

#[tokio::test]
async fn negotiation_through_relay_connects_and_carries_audio() {
    let state = platform();
    let router = Platform::router(state.clone());
    let (alice, _ta) = signup(&router, "alice").await;
    let (bob, _tb) = signup(&router, "bob").await;

    // 直聊会话开房：成员资格按 d-a~b 判定，不依赖群表
    let conv = format!("d-{alice}~{bob}");
    state.start_call(&conv, &alice, 8).unwrap();
    state.join_call(&conv, &bob).unwrap();

    // -- 两个媒体引擎（sans-I/O，没有真 socket）
    let mut caller = MediaSession::new(true);
    let mut callee = MediaSession::new(false);
    caller.add_host_candidate("192.0.2.10:40000".parse().unwrap());
    callee.add_host_candidate("192.0.2.20:40001".parse().unwrap());

    // -- offer：caller → 中继 → callee
    let offer = caller.offer(false).unwrap();
    let invite = as_body("invite", "call-1", &alice, &bob, offer.clone(), false);
    let dropped = state
        .post_call_signal(
            &conv,
            &alice,
            serde_json::from_value::<Signal>(invite).unwrap(),
        )
        .unwrap();
    assert_eq!(dropped, 0);

    let inbox = state.take_call_signals(&conv, &bob).unwrap();
    assert_eq!(inbox.len(), 1, "offer 必须到达对端");
    let SignalContent::Invite { sdp, .. } = &inbox[0].content else {
        panic!("expected invite, got {:?}", inbox[0].content);
    };
    assert_eq!(sdp, &offer, "SDP 经过 JSON 中继后必须逐字节等价");

    // -- answer：callee → 中继 → caller
    let answer = callee.accept_offer(sdp.clone()).unwrap();
    let accept = as_body("accept", "call-1", &bob, &alice, answer.clone(), false);
    state
        .post_call_signal(
            &conv,
            &bob,
            serde_json::from_value::<Signal>(accept).unwrap(),
        )
        .unwrap();
    let inbox = state.take_call_signals(&conv, &alice).unwrap();
    assert_eq!(inbox.len(), 1);
    let SignalContent::Accept { sdp } = &inbox[0].content else {
        panic!("expected accept");
    };
    caller.accept_answer(sdp.clone()).unwrap();

    // -- ICE + DTLS 握手（数据报走虚拟线路）
    for _ in 0..40 {
        pump(&mut caller, &mut callee, Duration::from_millis(250)).unwrap();
        if caller.is_connected() && callee.is_connected() {
            break;
        }
    }
    assert!(caller.is_connected(), "caller 完成 ICE/DTLS");
    assert!(callee.is_connected(), "callee 完成 ICE/DTLS");

    // -- 音频双向到达
    let mut at_caller = 0;
    let mut at_callee = 0;
    for _ in 0..30 {
        caller.write_audio(&[0xA5; 80]).unwrap();
        callee.write_audio(&[0x5A; 80]).unwrap();
        let (ea, eb) = pump(&mut caller, &mut callee, Duration::from_millis(50)).unwrap();
        at_caller += ea
            .iter()
            .filter(|e| matches!(e, Event::MediaData(d) if d.data == vec![0x5A; 80]))
            .count();
        at_callee += eb
            .iter()
            .filter(|e| matches!(e, Event::MediaData(d) if d.data == vec![0xA5; 80]))
            .count();
    }
    assert!(at_callee > 0, "callee 收到 caller 的音频");
    assert!(at_caller > 0, "caller 收到 callee 的音频");
}

/// 中继不该改写 SDP 之外的东西：带 `video: true` 的 invite 到对端仍是 video。
#[tokio::test]
async fn video_flag_survives_relay() {
    let state = platform();
    let router = Platform::router(state.clone());
    let (alice, _ta) = signup(&router, "a2").await;
    let (bob, _tb) = signup(&router, "b2").await;
    let conv = format!("d-{alice}~{bob}");
    state.start_call(&conv, &alice, 8).unwrap();
    state.join_call(&conv, &bob).unwrap();

    let mut caller = MediaSession::new(true);
    caller.add_host_candidate("192.0.2.30:40000".parse().unwrap());
    let offer = caller.offer(true).unwrap();
    let body = json!({
        "v": 1, "call_id": "c-video", "from": alice, "to": bob,
        "content": {"type": "invite", "sdp": offer, "video": true}
    });
    state
        .post_call_signal(&conv, &alice, serde_json::from_value(body).unwrap())
        .unwrap();

    let inbox = state.take_call_signals(&conv, &bob).unwrap();
    match &inbox[0].content {
        SignalContent::Invite { video, .. } => assert!(*video),
        other => panic!("unexpected {other:?}"),
    }
}
