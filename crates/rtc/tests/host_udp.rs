//! 真 socket + 真 ICE：两通"客户端"在 127.0.0.1 上互相打一通电话。
//!
//! 这一条覆盖的是**接线**本身，而不是任何一个零件：`CallHost` 把
//! `CallManager`（状态机）、`MediaSession`（str0m）和真实 UDP socket 接在
//! 一起，SDP 里的 candidate 就是 socket 的 `local_addr()`。所以它能证明：
//!
//! * 候选注入发生在 `offer` 之前（否则 SDP 里没有 candidate 行，ICE 永远连不上）；
//! * `tick` 交出的 `Transmit` 目的地是对的，`handle_datagram` 的
//!   `source`/`destination` 也没填反；
//! * 连通之后音频帧能从 A 的 `write_audio` 走到 B 的 `Effect::Audio`。
//!
//! 不依赖任何外部服务：信令在测试里用一条通道代替 HTTP 中继（中继本身由
//! `platform/tests/call_media.rs` 覆盖），媒体走真实 UDP。

use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use wxlike_rtc::host::{CallHost, Effect};

/// 把一批效应落地：信号转交给对端，数据报真发出去。
async fn apply(
    effects: Vec<Effect>,
    sock: &UdpSocket,
    to_peer: &mut Vec<wxlike_rtc::signal::Signal>,
) {
    for e in effects {
        match e {
            Effect::Signal(sig) => to_peer.push(sig),
            Effect::Transmit { to, bytes } => {
                let _ = sock.send_to(&bytes, to).await;
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn two_hosts_connect_over_real_udp_and_carry_audio() {
    let sock_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sock_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let a_addr = sock_a.local_addr().unwrap();
    let b_addr = sock_b.local_addr().unwrap();

    let mut a = CallHost::new("alice", "d-alice~bob", a_addr);
    let mut b = CallHost::new("bob", "d-alice~bob", b_addr);

    // -- 发起：offer 里必须已经带 candidate
    let (call_id, effects) = a.start("bob", false, true);
    let invite = effects
        .iter()
        .find_map(|e| match e {
            Effect::Signal(s) => Some(s.clone()),
            _ => None,
        })
        .expect("caller 必须发出 invite");
    let sdp = serde_json::to_string(&invite.content).unwrap();
    assert!(
        sdp.contains("candidate"),
        "offer 的 SDP 里必须有 candidate 行（候选注入早于 offer）: {sdp}"
    );

    // -- 被叫收 invite → 接听 → 回 accept
    let b_effects = b.handle_signal(invite);
    assert!(b_effects.is_empty(), "收到 invite 时不该主动发信号");
    let accept = b
        .accept(&call_id, Some(a_addr))
        .into_iter()
        .find_map(|e| match e {
            Effect::Signal(s) => Some(s),
            _ => None,
        })
        .expect("callee 必须回 accept");
    let a_effects = a.handle_signal(accept);
    assert!(a_effects.is_empty(), "收到 accept 时不该再发信号");

    // -- 握手：两边交替 tick + 收包，直到 ICE + DTLS 连通
    let mut buf = vec![0u8; 2048];
    let mut a_connected = false;
    let mut b_connected = false;
    let mut delivered = 0usize;
    let deadline = Instant::now() + Duration::from_secs(20);

    while Instant::now() < deadline && !(a_connected && b_connected) {
        let mut sink = Vec::new();
        let ea = a.tick(Instant::now());
        apply(ea, &sock_a, &mut sink).await;
        let eb = b.tick(Instant::now());
        apply(eb, &sock_b, &mut sink).await;

        // 收包（非阻塞，这一轮没到就算了）
        while let Ok(Ok((n, from))) =
            tokio::time::timeout(Duration::from_millis(2), sock_a.recv_from(&mut buf)).await
        {
            delivered += 1;
            let ef = a.handle_datagram(from, a_addr, &buf[..n]);
            apply(ef, &sock_a, &mut sink).await;
        }
        while let Ok(Ok((n, from))) =
            tokio::time::timeout(Duration::from_millis(2), sock_b.recv_from(&mut buf)).await
        {
            delivered += 1;
            let ef = b.handle_datagram(from, b_addr, &buf[..n]);
            apply(ef, &sock_b, &mut sink).await;
        }

        a_connected = a.is_connected(&call_id);
        b_connected = b.is_connected(&call_id);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(delivered > 0, "双方必须真的交换过 UDP 包");
    assert!(a_connected, "caller 侧 ICE/DTLS 未连通");
    assert!(b_connected, "callee 侧 ICE/DTLS 未连通");

    // -- 音频：A 写帧，B 必须收到
    let mut heard = 0usize;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && heard == 0 {
        a.write_audio(&call_id, &[0xC3; 80]).unwrap();
        let mut sink = Vec::new();
        apply(a.tick(Instant::now()), &sock_a, &mut sink).await;

        while let Ok(Ok((n, from))) =
            tokio::time::timeout(Duration::from_millis(2), sock_b.recv_from(&mut buf)).await
        {
            for e in b.handle_datagram(from, b_addr, &buf[..n]) {
                if let Effect::Audio { payload, .. } = e {
                    if payload == vec![0xC3; 80] {
                        heard += 1;
                    }
                }
            }
        }
        let mut sink = Vec::new();
        apply(b.tick(Instant::now()), &sock_b, &mut sink).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(heard, 1, "B 应当且只应当收到一帧 A 的音频");
}

/// 没设候选时，SDP 里不该出现 candidate 行——这条守住了"忘了接线"的退化路径，
/// 免得某天有人把候选注入删了还以为是网络问题。
#[test]
fn no_candidates_means_no_candidate_lines() {
    use wxlike_rtc::call::CallManager;
    use wxlike_rtc::signal::SignalContent;

    let mut mgr = CallManager::new("alice");
    let (_id, actions) = mgr.start("bob", false, true);
    let sdp = actions
        .iter()
        .find_map(|a| match a {
            wxlike_rtc::call::Action::Send(sig) => match &sig.content {
                SignalContent::Invite { sdp, .. } => Some(sdp.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("invite");
    let text = serde_json::to_string(&sdp).unwrap();
    assert!(
        !text.contains("candidate"),
        "未注入候选时 SDP 不该有 candidate: {text}"
    );
}
