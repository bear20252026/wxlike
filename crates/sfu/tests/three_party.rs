//! 三个客户端经 SFU 通话：A 说话，B 和 C 都要听到，且 A 不该收到自己的回音。
//!
//! 这是 SFU 的核心契约。mesh 版本（`rtc/tests/mesh_udp.rs`）证明的是"两两直连"，
//! 这里证明的是"每人只连服务端一条链路，服务端负责复制"——只有这样
//! 上行才不随人数增长，才谈得上几十上百人。
//!
//! 全程真 UDP（127.0.0.1）、真 ICE/DTLS/SRTP，不依赖任何外部服务。

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use wxlike_platform::{Platform, PlatformState};
use wxlike_rtc::media::{Datagram, MediaSession};
use wxlike_sfu::{SfuJoinError, SfuRoom};

#[derive(Default)]
struct Stats {
    recv: u64,
    recv_err: u64,
}

struct Client {
    name: &'static str,
    session: MediaSession,
    socket: UdpSocket,
    local: SocketAddr,
    stats: Stats,
}

/// 平台侧夹具：三个注册用户 + 一个三人群 + 每人一条会话 token。
struct Fixture {
    platform: PlatformState,
    conv: String,
    ids: HashMap<&'static str, String>,
    tokens: HashMap<&'static str, String>,
}

fn fixture() -> Fixture {
    let p = Platform::in_memory().unwrap();
    let alice = p.register("alice", "pw").unwrap();
    let bob = p.register("bob", "pw").unwrap();
    let carol = p.register("carol", "pw").unwrap();
    let conv = p
        .create_group(&alice, "sfu-three", &[bob.clone(), carol.clone()])
        .unwrap();
    let ids = HashMap::from([("alice", alice), ("bob", bob), ("carol", carol)]);
    let tokens = ids
        .iter()
        .map(|(name, id)| (*name, p.create_session(id, 3_600_000).unwrap()))
        .collect();
    Fixture {
        platform: p.shared(),
        conv,
        ids,
        tokens,
    }
}

/// 建一个客户端并完成 offer/answer（answer 里带候选，无需 trickle）。
fn try_join(room: &mut SfuRoom, name: &'static str, token: &str) -> Result<Client, SfuJoinError> {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let local = socket.local_addr().unwrap();

    let mut session = MediaSession::new(true);
    session.add_host_candidate(local);
    let offer = session.offer(false).unwrap();

    let reply = room.join(token, offer, false)?;
    session.accept_answer(reply.sdp).unwrap();

    Ok(Client {
        name,
        session,
        socket,
        local,
        stats: Stats::default(),
    })
}

fn join(room: &mut SfuRoom, f: &Fixture, name: &'static str) -> Client {
    try_join(room, name, &f.tokens[name]).expect("SFU 应接受 offer")
}

/// 推进所有客户端与服务端一轮。
///
/// 注意：`MediaSession::step()` 每次**只吐一个** output（一个数据报或一个事件），
/// 所以这里每轮调两次并把两次的数据报都发出去——只发第一次的话，DTLS 握手包
/// 会被丢掉一半，表现就是 ICE Completed 但永远不 Connected。
fn pump(room: &mut SfuRoom, clients: &mut [Client], inbox: &mut Vec<(String, Vec<u8>)>) {
    let mut buf = [0u8; 2048];
    for c in clients.iter_mut() {
        let (mut events, mut out) = c.session.step().expect("client step");
        // 收
        while let Ok((n, from)) = c.socket.recv_from(&mut buf) {
            c.stats.recv += 1;
            let d = Datagram {
                data: buf[..n].to_vec(),
                source: from,
                destination: c.local,
            };
            if c.session.handle_receive(&d).is_err() {
                c.stats.recv_err += 1;
            }
        }
        let (ev2, out2) = c.session.step().expect("client step 2");
        events.extend(ev2);
        out.extend(out2);
        // 发（两批都发）
        for d in out {
            let _ = c.socket.send_to(&d.data, d.destination);
        }
        if std::env::var("SFU_DEBUG").is_ok() {
            for ev in &events {
                eprintln!("[{}] {:?}", c.name, ev);
            }
        }
        for ev in events {
            if let str0m::Event::MediaData(md) = ev {
                inbox.push((c.name.to_string(), md.data));
            }
        }
    }
    room.poll();
}

#[test]
fn three_clients_through_sfu_hear_each_other() {
    let f = fixture();
    let mut room = SfuRoom::new(
        f.platform.clone(),
        &f.conv,
        Some("127.0.0.1".parse().unwrap()),
        0,
    );
    let mut clients = vec![
        join(&mut room, &f, "alice"),
        join(&mut room, &f, "bob"),
        join(&mut room, &f, "carol"),
    ];
    assert_eq!(room.len(), 3);

    let mut inbox = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    // 服务端先到 3 不代表客户端也处理完了自己的 Connected 事件，两边都要等
    while Instant::now() < deadline
        && (room.connected() < 3 || clients.iter().any(|c| !c.session.is_connected()))
    {
        pump(&mut room, &mut clients, &mut inbox);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        room.connected(),
        3,
        "三条链路都要连到 SFU（SFU 收 {} / 发 {} / 泵错 {}；客户端收 {:?}）",
        room.recv_packets,
        room.sent_packets,
        room.step_errors,
        clients
            .iter()
            .map(|c| (c.name, c.stats.recv, c.stats.recv_err))
            .collect::<Vec<_>>()
    );
    for c in &clients {
        assert!(c.session.is_connected(), "{} 未连通", c.name);
    }

    // alice 说一句话，bob 与 carol 都要收到，alice 自己不该收到
    let payload = vec![0xAB; 80];
    let alice = clients.iter_mut().find(|c| c.name == "alice").unwrap();
    let alice_mid = alice.session.audio_mid().unwrap();
    let _ = alice_mid;
    alice.session.write_audio(&payload).unwrap();

    let mut heard_by: Vec<String> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && heard_by.len() < 2 {
        inbox.clear();
        pump(&mut room, &mut clients, &mut inbox);
        for (who, data) in &inbox {
            if *data == payload && who != "alice" && !heard_by.contains(who) {
                heard_by.push(who.clone());
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    heard_by.sort();
    assert_eq!(
        heard_by,
        vec!["bob".to_string(), "carol".to_string()],
        "SFU 必须把 alice 的音频复制给其余两人，且不回给她自己"
    );
}

#[test]
fn leaving_frees_the_participant() {
    let f = fixture();
    let mut room = SfuRoom::new(
        f.platform.clone(),
        &f.conv,
        Some("127.0.0.1".parse().unwrap()),
        0,
    );
    let _a = join(&mut room, &f, "alice");
    let _b = join(&mut room, &f, "bob");
    assert_eq!(room.len(), 2);
    room.leave(&f.ids["bob"]);
    assert_eq!(room.len(), 1);
    assert_eq!(room.participants(), vec![f.ids["alice"].clone()]);
    // 重复 leave 不该 panic
    room.leave(&f.ids["bob"]);
}

/// 同一账号重复加入必须被拒绝：旧实现先 `leave` 再加，等于知道 id 就能踢人。
#[test]
fn rejoin_with_the_same_identity_is_rejected() {
    let f = fixture();
    let mut room = SfuRoom::new(
        f.platform.clone(),
        &f.conv,
        Some("127.0.0.1".parse().unwrap()),
        0,
    );
    let _a = join(&mut room, &f, "alice");

    let err = match try_join(&mut room, "alice", &f.tokens["alice"]) {
        Ok(_) => panic!("重复 join 应被拒绝"),
        Err(err) => err,
    };
    assert!(matches!(err, SfuJoinError::PeerConflict), "实际: {err:?}");
    assert_eq!(err.http_status(), 409);
    assert_eq!(room.len(), 1, "冲突的 join 不能顶替或新增参与者");
    assert_eq!(room.participants(), vec![f.ids["alice"].clone()]);
}
