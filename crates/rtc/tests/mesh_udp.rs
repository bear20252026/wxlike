//! 三方 mesh：一个人同时与两个人通话。
//!
//! 验证的是"多条媒体会话共用一个 socket"这件事——`CallHost` 内部按 call_id
//! 维护多个 `MediaSession`，`tick` 会逐个泵，收到的包按来源匹配候选。
//!
//! 拓扑是星形（A 同时呼叫 B、C），因为对 A 来说这正是三方 mesh 的全部负担：
//! 两条上行、两条下行。B 与 C 之间是否再连一条，不影响 A 侧的正确性，
//! 但会让这个测试多一倍样板代码。
//!
//! 关键断言：**A 的麦克风音频必须同时到达 B 和 C**。这条最容易漏——如果
//! 引擎只往"第一条会话"写音频，一对一听起来完全正常，多人通话里却只有
//! 一个人能听见你。

use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use wxlike_rtc::host::{CallHost, Effect};
use wxlike_rtc::signal::{Signal, SignalContent};

/// 一轮循环：所有宿主各推进一次，用**自己的** socket 发数据报，再各收一轮。
async fn round(hosts: &mut [(&mut CallHost, &UdpSocket)]) {
    let mut buf = vec![0u8; 2048];
    for (host, sock) in hosts.iter_mut() {
        for e in host.tick(Instant::now()) {
            if let Effect::Transmit { to, bytes } = e {
                let _ = sock.send_to(&bytes, to).await;
            }
        }
    }
    for (host, sock) in hosts.iter_mut() {
        let local = host.local_addr();
        while let Ok(Ok((n, from))) =
            tokio::time::timeout(Duration::from_millis(2), sock.recv_from(&mut buf)).await
        {
            for e in host.handle_datagram(from, local, &buf[..n]) {
                if let Effect::Transmit { to, bytes } = e {
                    let _ = sock.send_to(&bytes, to).await;
                }
            }
        }
    }
}

#[tokio::test]
async fn three_way_star_connects_and_audio_reaches_everyone() {
    let sock_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sock_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sock_c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let (a_addr, b_addr, c_addr) = (
        sock_a.local_addr().unwrap(),
        sock_b.local_addr().unwrap(),
        sock_c.local_addr().unwrap(),
    );

    let mut a = CallHost::new("alice", "g-1", a_addr);
    let mut b = CallHost::new("bob", "g-1", b_addr);
    let mut c = CallHost::new("carol", "g-1", c_addr);

    // A 同时呼叫 B 与 C
    let peers = vec!["bob".to_string(), "carol".to_string()];
    let (call_ids, effects) = a.start_many(&peers, false);
    assert_eq!(call_ids.len(), 2, "每个对端一条会话");

    // 把两条 invite 按收件人分发给 B、C
    let mut invites: Vec<Signal> = effects
        .into_iter()
        .filter_map(|e| match e {
            Effect::Signal(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(invites.len(), 2);

    let invite_c = invites
        .iter()
        .position(|s| s.to == "carol")
        .map(|i| invites.remove(i))
        .expect("carol 的 invite");
    let invite_b = invites.remove(0);

    // 各自接听并回 accept
    b.handle_signal(invite_b.clone());
    let accept_b = b
        .accept(&invite_b.call_id, Some(a_addr))
        .into_iter()
        .find_map(|e| match e {
            Effect::Signal(s) => Some(s),
            _ => None,
        })
        .expect("bob accept");
    c.handle_signal(invite_c.clone());
    let accept_c = c
        .accept(&invite_c.call_id, Some(a_addr))
        .into_iter()
        .find_map(|e| match e {
            Effect::Signal(s) => Some(s),
            _ => None,
        })
        .expect("carol accept");
    a.handle_signal(accept_b);
    a.handle_signal(accept_c);

    // 握手
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline && a.connected_count() < 2 {
        round(&mut [(&mut a, &sock_a), (&mut b, &sock_b), (&mut c, &sock_c)]).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        if std::env::var("MESH_DEBUG").is_ok() {
            eprintln!(
                "a={} b={} c={} live_a={:?}",
                a.connected_count(),
                b.connected_count(),
                c.connected_count(),
                a.manager().live_calls()
            );
        }
    }
    assert_eq!(a.connected_count(), 2, "A 的两条会话都要连通");
    assert_eq!(b.connected_count(), 1, "B 只有一条");
    assert_eq!(c.connected_count(), 1, "C 只有一条");

    // A 的音频必须同时到 B 和 C
    let mut heard_b = 0usize;
    let mut heard_c = 0usize;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && (heard_b == 0 || heard_c == 0) {
        for id in &call_ids {
            a.write_audio(id, &[0xEE; 80]).unwrap();
        }
        let mut buf = vec![0u8; 2048];
        // 泵 A
        for e in a.tick(Instant::now()) {
            if let Effect::Transmit { to, bytes } = e {
                let _ = sock_a.send_to(&bytes, to).await;
            }
        }
        for (sock, host, local, counter) in [
            (&sock_b, &mut b, b_addr, &mut heard_b),
            (&sock_c, &mut c, c_addr, &mut heard_c),
        ] {
            while let Ok(Ok((n, from))) =
                tokio::time::timeout(Duration::from_millis(2), sock.recv_from(&mut buf)).await
            {
                for e in host.handle_datagram(from, local, &buf[..n]) {
                    if let Effect::Audio { payload, .. } = e {
                        if payload == vec![0xEE; 80] {
                            *counter += 1;
                        }
                    }
                }
            }
            let _ = host.tick(Instant::now());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(heard_b > 0, "B 必须听到 A（mesh 下音频要喂给每一条会话）");
    assert!(heard_c > 0, "C 必须听到 A");
}

/// 两条会话的 invite 必须是不同的 call_id——共用会让状态机串台。
#[test]
fn each_peer_gets_its_own_call_id() {
    let mut host = CallHost::new("alice", "g-1", "127.0.0.1:41000".parse().unwrap());
    let (ids, effects) = host.start_many(&["bob".into(), "carol".into()], false);
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    let invited: Vec<String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Signal(s) => match &s.content {
                SignalContent::Invite { .. } => Some(s.to.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(invited, vec!["bob".to_string(), "carol".to_string()]);
}
