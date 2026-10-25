//! G09-05 直播房间角色门禁：主播/连麦者可发，观众只收；停播清空连接。

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use wxlike_platform::{Platform, PlatformState};
use wxlike_rtc::media::{Datagram, MediaSession};
use wxlike_sfu::live::{LiveRoom, LiveRoomError};
use wxlike_sfu::SfuRoom;

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
        .create_group(&alice, "live-room", &[bob.clone(), carol.clone()])
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

fn offer(video: bool) -> (MediaSession, UdpSocket, SocketAddr, serde_json::Value) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_nonblocking(true).unwrap();
    let local = socket.local_addr().unwrap();
    let mut session = MediaSession::new(true);
    session.add_host_candidate(local);
    let offer = session.offer(video).unwrap();
    (session, socket, local, offer)
}

fn finish(
    name: &'static str,
    mut session: MediaSession,
    socket: UdpSocket,
    local: SocketAddr,
    reply: wxlike_sfu::JoinReply,
) -> Client {
    session.accept_answer(reply.sdp).unwrap();
    Client {
        name,
        session,
        socket,
        local,
    }
}

fn pump(room: &mut LiveRoom, clients: &mut [Client], inbox: &mut Vec<(&'static str, Vec<u8>)>) {
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
            if let str0m::Event::MediaData(md) = ev {
                inbox.push((c.name, md.data));
            }
        }
    }
    room.media_mut().poll();
}

fn connect(room: &mut LiveRoom, clients: &mut [Client]) {
    let mut inbox = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline
        && (room.media().connected() < clients.len()
            || clients.iter().any(|c| !c.session.is_connected()))
    {
        pump(room, clients, &mut inbox);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(room.media().connected(), clients.len(), "全部链路连通");
}

fn heard(
    room: &mut LiveRoom,
    clients: &mut [Client],
    byte: u8,
    rounds: usize,
) -> Vec<&'static str> {
    let mut heard = Vec::new();
    let mut inbox = Vec::new();
    for _ in 0..rounds {
        inbox.clear();
        pump(room, clients, &mut inbox);
        for (who, data) in &inbox {
            if data.first() == Some(&byte) && !heard.contains(who) {
                heard.push(*who);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    heard
}

#[test]
fn only_host_and_cohost_publish() {
    let f = fixture();
    let media = SfuRoom::new(
        f.platform.clone(),
        &f.conv,
        Some("127.0.0.1".parse().unwrap()),
        0,
    );
    let mut live = LiveRoom::new(media, f.ids["alice"].as_str());

    let (s, sock, local, off) = offer(false);
    let host_reply = live
        .join_host(&f.ids["alice"], &f.tokens["alice"], off, false)
        .unwrap();
    let (s2, sock2, local2, off2) = offer(false);
    let viewer_reply = live.join_viewer(&f.tokens["bob"], off2, false).unwrap();
    let (s3, sock3, local3, off3) = offer(false);
    let cohost_reply = live
        .join_cohost(&f.ids["carol"], &f.tokens["carol"], off3, false)
        .unwrap();

    let mut clients = vec![
        finish("alice", s, sock, local, host_reply),
        finish("bob", s2, sock2, local2, viewer_reply),
        finish("carol", s3, sock3, local3, cohost_reply),
    ];
    connect(&mut live, &mut clients);
    assert!(live.media().is_publisher(&f.ids["alice"]));
    assert!(!live.media().is_publisher(&f.ids["bob"]));
    assert!(live.media().is_publisher(&f.ids["carol"]));
    assert!(live.is_cohost(&f.ids["carol"]));

    // 观众偷偷上行：谁都收不到
    clients
        .iter_mut()
        .find(|c| c.name == "bob")
        .unwrap()
        .session
        .write_audio(&[0x11; 80])
        .unwrap();
    let got = heard(&mut live, &mut clients, 0x11, 40);
    assert!(
        !got.contains(&"alice") && !got.contains(&"carol"),
        "观众上行不得转发: {got:?}"
    );

    // 连麦者上行：主播与观众都收到
    clients
        .iter_mut()
        .find(|c| c.name == "carol")
        .unwrap()
        .session
        .write_audio(&[0x22; 80])
        .unwrap();
    let got = heard(&mut live, &mut clients, 0x22, 80);
    assert!(
        got.contains(&"alice") && got.contains(&"bob"),
        "连麦音频应转发: {got:?}"
    );

    // 取消连麦：上行立即被截断
    assert!(live.revoke_cohost(&f.ids["carol"]));
    clients
        .iter_mut()
        .find(|c| c.name == "carol")
        .unwrap()
        .session
        .write_audio(&[0x33; 80])
        .unwrap();
    let got = heard(&mut live, &mut clients, 0x33, 40);
    assert!(
        !got.contains(&"alice") && !got.contains(&"bob"),
        "取消连麦后不得转发: {got:?}"
    );
}

#[test]
fn token_identity_must_match_the_role() {
    let f = fixture();
    let media = SfuRoom::new(
        f.platform.clone(),
        &f.conv,
        Some("127.0.0.1".parse().unwrap()),
        0,
    );
    let mut live = LiveRoom::new(media, f.ids["alice"].as_str());
    let (_s, _sock, _local, off) = offer(false);
    let err = live
        .join_host(&f.ids["alice"], &f.tokens["bob"], off, false)
        .unwrap_err();
    assert!(matches!(err, LiveRoomError::IdentityMismatch));
    assert_eq!(live.viewer_connections(), 0, "身份不符不得留下半连接");

    let (_s, _sock, _local, off) = offer(false);
    let err = live
        .join_cohost(&f.ids["carol"], &f.tokens["bob"], off, false)
        .unwrap_err();
    assert!(matches!(err, LiveRoomError::IdentityMismatch));
}

#[test]
fn stop_reclaims_every_connection_and_refuses_late_joins() {
    let f = fixture();
    let media = SfuRoom::new(
        f.platform.clone(),
        &f.conv,
        Some("127.0.0.1".parse().unwrap()),
        0,
    );
    let mut live = LiveRoom::new(media, f.ids["alice"].as_str());
    let (_s1, _sock1, _l1, off1) = offer(false);
    live.join_host(&f.ids["alice"], &f.tokens["alice"], off1, false)
        .unwrap();
    let (_s2, _sock2, _l2, off2) = offer(false);
    live.join_viewer(&f.tokens["bob"], off2, false).unwrap();
    assert_eq!(live.viewer_connections(), 2);

    assert_eq!(live.stop(), 2);
    assert!(live.is_stopped());
    assert_eq!(live.viewer_connections(), 0, "停播必须释放全部媒体连接");
    assert_eq!(live.stop(), 2, "重复停播幂等");

    let (_s3, _sock3, _l3, off3) = offer(false);
    let err = live
        .join_viewer(&f.tokens["carol"], off3, false)
        .unwrap_err();
    assert!(matches!(err, LiveRoomError::Stopped));
}
