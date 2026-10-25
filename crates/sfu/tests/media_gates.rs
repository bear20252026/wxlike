//! G09-03/G09-05 的 SFU 媒体门禁：真 UDP、真 ICE/DTLS/SRTP，验证
//! "谁能发、谁能收、服务端静音是否真的截断"。
//!
//! 三条契约：
//! * 视频只转发给协商了视频 m-line 的订阅者（纯音频端不会被塞视频）；
//! * 服务端静音后，客户端继续发也转发不出去；
//! * 观众（viewer）的上行永不被转发，但仍能收到发布者的媒体。

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use wxlike_platform::{Platform, PlatformState};
use wxlike_rtc::media::{Datagram, MediaSession};
use wxlike_sfu::{MediaTrack, SfuJoinError, SfuRoom};

struct Client {
    name: &'static str,
    session: MediaSession,
    socket: UdpSocket,
    local: SocketAddr,
}

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
        .create_group(&alice, "sfu-gates", &[bob.clone(), carol.clone()])
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

fn join_as(
    room: &mut SfuRoom,
    name: &'static str,
    token: &str,
    video: bool,
    viewer: bool,
) -> Client {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let local = socket.local_addr().unwrap();

    let mut session = MediaSession::new(true);
    session.add_host_candidate(local);
    let offer = session.offer(video).unwrap();

    let reply = if viewer {
        room.join_viewer(token, offer, video)
    } else {
        room.join(token, offer, video)
    };
    let reply: wxlike_sfu::JoinReply = reply.expect("SFU 应接受 offer");
    session.accept_answer(reply.sdp).unwrap();

    Client {
        name,
        session,
        socket,
        local,
    }
}

fn pump(room: &mut SfuRoom, clients: &mut [Client], inbox: &mut Vec<(&'static str, Vec<u8>)>) {
    let mut buf = [0u8; 2048];
    for c in clients.iter_mut() {
        let (mut events, mut out) = c.session.step().expect("client step");
        while let Ok((n, from)) = c.socket.recv_from(&mut buf) {
            let d = Datagram {
                data: buf[..n].to_vec(),
                source: from,
                destination: c.local,
            };
            let _ = c.session.handle_receive(&d);
        }
        let (ev2, out2) = c.session.step().expect("client step 2");
        events.extend(ev2);
        out.extend(out2);
        for d in out {
            let _ = c.socket.send_to(&d.data, d.destination);
        }
        for ev in events {
            if std::env::var("SFU_DEBUG").is_ok() {
                eprintln!("[client {}] {:?}", c.name, ev);
            }
            if let str0m::Event::MediaData(md) = ev {
                inbox.push((c.name, md.data));
            }
        }
    }
    room.poll();
}

fn connect(room: &mut SfuRoom, clients: &mut [Client]) {
    let mut inbox = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline
        && (room.connected() < clients.len() || clients.iter().any(|c| !c.session.is_connected()))
    {
        pump(room, clients, &mut inbox);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        room.connected(),
        clients.len(),
        "全部链路连通（SFU 收 {} / 发 {} / 泵错 {}）",
        room.recv_packets,
        room.sent_packets,
        room.step_errors
    );
}

/// 推进若干轮，返回期间谁收到了哪个载荷。
fn drain_for(
    room: &mut SfuRoom,
    clients: &mut [Client],
    want_prefix: u8,
    rounds: usize,
) -> Vec<&'static str> {
    let mut heard = Vec::new();
    let mut inbox = Vec::new();
    for _ in 0..rounds {
        inbox.clear();
        pump(room, clients, &mut inbox);
        for (who, data) in &inbox {
            if data.first() == Some(&want_prefix) && !heard.contains(who) {
                heard.push(*who);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    heard
}

#[test]
fn video_is_forwarded_only_to_video_subscribers() {
    let f = fixture();
    let mut room = SfuRoom::new(
        f.platform.clone(),
        &f.conv,
        Some("127.0.0.1".parse().unwrap()),
        0,
    );
    let mut clients = vec![
        join_as(&mut room, "alice", &f.tokens["alice"], true, false),
        join_as(&mut room, "bob", &f.tokens["bob"], true, false),
        join_as(&mut room, "carol", &f.tokens["carol"], false, false),
    ];
    connect(&mut room, &mut clients);

    let alice = clients.iter_mut().find(|c| c.name == "alice").unwrap();
    alice.session.write_audio(&[0xA1; 80]).unwrap();
    alice.session.write_video(&[0x71; 60]).unwrap();

    let audio = drain_for(&mut room, &mut clients, 0xA1, 80);
    assert!(
        audio.contains(&"bob") && audio.contains(&"carol"),
        "纯音频端也要收到音频: {audio:?}"
    );
    assert!(!audio.contains(&"alice"), "不回发给发布者");

    // 视频帧不像音频那样每 20ms 重发：丢掉一轮就没了，所以再送一帧
    // 并在同一段窗口里观察。
    clients
        .iter_mut()
        .find(|c| c.name == "alice")
        .unwrap()
        .session
        .write_video(&[0x72; 60])
        .unwrap();
    let video = drain_for(&mut room, &mut clients, 0x72, 40);
    assert!(
        video.contains(&"bob"),
        "协商了视频的订阅者应收到视频: {video:?}"
    );
    assert!(
        !video.contains(&"carol"),
        "只协商音频的端不该被塞视频: {video:?}"
    );
}

#[test]
fn server_side_mute_stops_forwarding_even_if_the_client_keeps_sending() {
    let f = fixture();
    let mut room = SfuRoom::new(
        f.platform.clone(),
        &f.conv,
        Some("127.0.0.1".parse().unwrap()),
        0,
    );
    let mut clients = vec![
        join_as(&mut room, "alice", &f.tokens["alice"], false, false),
        join_as(&mut room, "bob", &f.tokens["bob"], false, false),
    ];
    connect(&mut room, &mut clients);

    let alice_id = f.ids["alice"].clone();
    assert!(room.set_published_muted(&alice_id, MediaTrack::Audio, true));

    clients
        .iter_mut()
        .find(|c| c.name == "alice")
        .unwrap()
        .session
        .write_audio(&[0xB2; 80])
        .unwrap();
    let heard = drain_for(&mut room, &mut clients, 0xB2, 60);
    assert!(!heard.contains(&"bob"), "服务端静音后不得转发: {heard:?}");

    assert!(room.set_published_muted(&alice_id, MediaTrack::Audio, false));
    clients
        .iter_mut()
        .find(|c| c.name == "alice")
        .unwrap()
        .session
        .write_audio(&[0xB3; 80])
        .unwrap();
    let heard = drain_for(&mut room, &mut clients, 0xB3, 80);
    assert!(heard.contains(&"bob"), "解除静音后恢复转发: {heard:?}");
}

#[test]
fn viewer_uplink_is_never_forwarded_but_downlink_works() {
    let f = fixture();
    let mut room = SfuRoom::new(
        f.platform.clone(),
        &f.conv,
        Some("127.0.0.1".parse().unwrap()),
        0,
    );
    let mut clients = vec![
        join_as(&mut room, "alice", &f.tokens["alice"], false, false),
        join_as(&mut room, "bob", &f.tokens["bob"], false, true),
    ];
    connect(&mut room, &mut clients);
    assert!(room.is_publisher(&f.ids["alice"]));
    assert!(!room.is_publisher(&f.ids["bob"]));

    // 观众自称在发：SFU 收到也不转发。
    clients
        .iter_mut()
        .find(|c| c.name == "bob")
        .unwrap()
        .session
        .write_audio(&[0xC4; 80])
        .unwrap();
    let heard = drain_for(&mut room, &mut clients, 0xC4, 60);
    assert!(!heard.contains(&"alice"), "观众上行不得进入转发: {heard:?}");

    // 主播下行仍然通。
    clients
        .iter_mut()
        .find(|c| c.name == "alice")
        .unwrap()
        .session
        .write_audio(&[0xC5; 80])
        .unwrap();
    let heard = drain_for(&mut room, &mut clients, 0xC5, 80);
    assert!(heard.contains(&"bob"), "观众必须收到主播媒体: {heard:?}");
}

#[test]
fn viewer_can_be_promoted_and_demoted() {
    let f = fixture();
    let mut room = SfuRoom::new(
        f.platform.clone(),
        &f.conv,
        Some("127.0.0.1".parse().unwrap()),
        0,
    );
    let mut clients = vec![
        join_as(&mut room, "alice", &f.tokens["alice"], false, false),
        join_as(&mut room, "bob", &f.tokens["bob"], false, true),
    ];
    connect(&mut room, &mut clients);

    let bob_id = f.ids["bob"].clone();
    assert!(room.set_publisher(&bob_id, true), "观众可被批准连麦");
    clients
        .iter_mut()
        .find(|c| c.name == "bob")
        .unwrap()
        .session
        .write_audio(&[0xD6; 80])
        .unwrap();
    let heard = drain_for(&mut room, &mut clients, 0xD6, 80);
    assert!(heard.contains(&"alice"), "批准后上行应被转发: {heard:?}");

    assert!(room.set_publisher(&bob_id, false));
    clients
        .iter_mut()
        .find(|c| c.name == "bob")
        .unwrap()
        .session
        .write_audio(&[0xD7; 80])
        .unwrap();
    let heard = drain_for(&mut room, &mut clients, 0xD7, 60);
    assert!(
        !heard.contains(&"alice"),
        "取消连麦后上行应被截断: {heard:?}"
    );
}

#[test]
fn join_viewer_still_requires_membership() {
    let f = fixture();
    let mut room = SfuRoom::new(
        f.platform.clone(),
        &f.conv,
        Some("127.0.0.1".parse().unwrap()),
        0,
    );
    let mut session = MediaSession::new(true);
    session.add_host_candidate("127.0.0.1:0".parse().unwrap());
    let offer = session.offer(false).unwrap();
    let err = match room.join_viewer("not-a-session", offer, false) {
        Ok(_) => panic!("未知 token 不得以观众身份进入"),
        Err(err) => err,
    };
    assert!(matches!(err, SfuJoinError::Unauthorized), "实际: {err:?}");
    assert_eq!(room.len(), 0);
}
