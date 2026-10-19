//! TURN 客户端对着一台**最小 TURN 服务器**跑通完整中继路径。
//!
//! 服务器是测试里自己写的（同进程 UDP），只实现客户端会用到的那几步：
//! Allocate 的 401 挑战 → 带 MESSAGE-INTEGRITY 的 Allocate → CreatePermission
//! → ChannelBind → ChannelData 双向转发。这样验证的是客户端协议本身，不需要
//! 部署 coturn，也不会因为外网而 flaky。
//!
//! 为什么值得单独测：TURN 鉴权有两个极易写错的地方——(1) HMAC 覆盖的头部里
//! length 必须已经算上 MESSAGE-INTEGRITY 那 24 字节；(2) 属性必须 4 字节对齐。
//! 任一处写错，真实服务器只会回 401/400，而本机"看起来一切正常"。

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use wxlike_rtc::ice::TurnConfig;
use wxlike_rtc::turn::{
    build, decode_xor_addr, encode_xor_addr, long_term_key, msg_type, pad4, put_attr,
    verify_integrity, Message, TurnClient, A_CHANNEL_NUMBER, A_ERROR_CODE, A_LIFETIME, A_NONCE,
    A_REALM, A_XOR_PEER_ADDRESS, A_XOR_RELAYED_ADDRESS, C_ERROR, C_REQUEST, C_SUCCESS, M_ALLOCATE,
    M_CHANNEL_BIND, M_CREATE_PERMISSION, M_REFRESH,
};

const REALM: &str = "wxlike.test";
const NONCE: &str = "nonce-1";
const USER: &str = "alice";
const PASS: &str = "s3cret";

fn error_code(code: u16, reason: &str) -> Vec<u8> {
    let mut v = vec![0u8, 0, (code / 100) as u8, (code % 100) as u8];
    v.extend_from_slice(reason.as_bytes());
    v
}

/// 起一台最小 TURN 服务器；返回 `(地址, 停止开关)`。
fn spawn_turn_server() -> (SocketAddr, Arc<AtomicBool>) {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let addr = sock.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();

    std::thread::spawn(move || {
        let key = long_term_key(USER, REALM, PASS);
        let mut channels: Vec<(u16, SocketAddr)> = Vec::new();
        let mut client: Option<SocketAddr> = None;
        let mut buf = [0u8; 2048];
        while !flag.load(Ordering::SeqCst) {
            let Ok((n, from)) = sock.recv_from(&mut buf) else {
                continue; // 超时，回头检查停止位
            };
            let pkt = &buf[..n];

            // ① 对端发来的裸数据 → 包成 ChannelData 回给客户端。
            //    必须先按**来源**区分，不能按首字节猜：裸数据的第一个字节
            //    完全可能落在 0x40–0x7F（正好是通道号高位），把它当 ChannelData
            //    解析就会静默丢包。真实 TURN 服务器也是按来源 socket 区分的。
            if let Some(c) = client {
                if from != c {
                    if let Some((ch, _)) = channels.iter().find(|(_, p)| *p == from) {
                        let mut out = Vec::with_capacity(4 + n + 3);
                        out.extend_from_slice(&ch.to_be_bytes());
                        out.extend_from_slice(&(n as u16).to_be_bytes());
                        out.extend_from_slice(pkt);
                        out.extend(std::iter::repeat_n(0u8, pad4(n)));
                        let _ = sock.send_to(&out, c);
                        continue;
                    }
                }
            }

            // ② 客户端发来的 ChannelData → 转发给对端
            if pkt.len() >= 4 && pkt[0] & 0xC0 == 0x40 {
                let ch = u16::from_be_bytes([pkt[0], pkt[1]]);
                let len = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
                if let Some((_, peer)) = channels.iter().find(|(c, _)| *c == ch) {
                    let _ = sock.send_to(&pkt[4..4 + len], peer);
                }
                continue;
            }

            let Some(msg) = Message::parse(pkt) else {
                continue;
            };
            client = Some(from);

            let reply = match msg.method {
                M_ALLOCATE if msg.attr(0x0008).is_none() => build(
                    M_ALLOCATE,
                    C_ERROR,
                    msg.txid,
                    &[
                        (A_ERROR_CODE, error_code(401, "Unauthorized")),
                        (A_REALM, REALM.as_bytes().to_vec()),
                        (A_NONCE, NONCE.as_bytes().to_vec()),
                    ],
                    None,
                ),
                M_ALLOCATE => {
                    if verify_integrity(&msg, &key) {
                        build(
                            M_ALLOCATE,
                            C_SUCCESS,
                            msg.txid,
                            &[
                                // 中继地址就用服务器自己：测试里够用
                                (A_XOR_RELAYED_ADDRESS, encode_xor_addr(addr)),
                                (A_LIFETIME, 600u32.to_be_bytes().to_vec()),
                            ],
                            Some(&key),
                        )
                    } else {
                        build(
                            M_ALLOCATE,
                            C_ERROR,
                            msg.txid,
                            &[
                                (A_ERROR_CODE, error_code(401, "Bad Integrity")),
                                (A_REALM, REALM.as_bytes().to_vec()),
                                (A_NONCE, NONCE.as_bytes().to_vec()),
                            ],
                            None,
                        )
                    }
                }
                M_CHANNEL_BIND => {
                    let ok = verify_integrity(&msg, &key);
                    if ok {
                        let ch = msg
                            .attr(A_CHANNEL_NUMBER)
                            .map(|v| u16::from_be_bytes([v[0], v[1]]))
                            .unwrap_or(0);
                        if let Some(peer) = msg.attr(A_XOR_PEER_ADDRESS).and_then(decode_xor_addr) {
                            channels.push((ch, peer));
                        }
                    }
                    build(
                        M_CHANNEL_BIND,
                        if ok { C_SUCCESS } else { C_ERROR },
                        msg.txid,
                        &if ok {
                            Vec::new()
                        } else {
                            vec![(A_ERROR_CODE, error_code(401, "Bad Integrity"))]
                        },
                        ok.then_some(&key[..]),
                    )
                }
                M_CREATE_PERMISSION | M_REFRESH => {
                    let ok = verify_integrity(&msg, &key);
                    build(
                        msg.method,
                        if ok { C_SUCCESS } else { C_ERROR },
                        msg.txid,
                        &if ok {
                            vec![(A_LIFETIME, 600u32.to_be_bytes().to_vec())]
                        } else {
                            vec![(A_ERROR_CODE, error_code(401, "Bad Integrity"))]
                        },
                        ok.then_some(&key[..]),
                    )
                }
                _ => continue,
            };
            let _ = sock.send_to(&reply, from);
        }
    });

    (addr, stop)
}

#[test]
fn allocate_bind_and_relay_both_ways() {
    let (server, stop) = spawn_turn_server();
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let peer_addr = peer.local_addr().unwrap();

    let cfg = TurnConfig {
        host: server.ip().to_string(),
        port: server.port(),
        user: USER.into(),
        password: PASS.into(),
    };

    let mut client = TurnClient::allocate(&cfg).expect("分配应成功（含 401 挑战重发）");
    assert_eq!(
        client.relayed_addr(),
        server,
        "中继地址必须来自 XOR-RELAYED-ADDRESS"
    );

    let ch = client.bind_channel(peer_addr).expect("ChannelBind 应成功");
    assert!(
        (0x4000..=0x7FFF).contains(&ch),
        "通道号超出 RFC 范围: {ch:x}"
    );

    // 客户端 → 对端
    client.send(peer_addr, b"hello").expect("send");
    let mut buf = [0u8; 1500];
    let (n, from) = peer.recv_from(&mut buf).expect("对端应收到数据");
    assert_eq!(&buf[..n], b"hello", "中继必须原样透传");
    assert_eq!(from, server, "对端看到的来源应是 TURN 服务器");

    // 对端 → 客户端（服务器按通道表包成 ChannelData）
    peer.send_to(b"world", server).expect("peer send");
    let (from, data) =
        wait_for(&mut client, Duration::from_secs(3)).expect("客户端没收到经中继回来的数据");
    assert_eq!(from, peer_addr, "来源必须是通道绑定的对端");
    assert_eq!(data, b"world");

    stop.store(true, Ordering::SeqCst);
}

fn wait_for(client: &mut TurnClient, timeout: Duration) -> Option<(SocketAddr, Vec<u8>)> {
    let deadline = Instant::now() + timeout;
    let mut buf = vec![0u8; 2048];
    while Instant::now() < deadline {
        if let Some(v) = client.recv(&mut buf) {
            return Some(v);
        }
    }
    None
}

/// 4 字节对齐与类型编码的守卫。
#[test]
fn padding_and_type_encoding() {
    assert_eq!(pad4(0), 0);
    assert_eq!(pad4(1), 3);
    assert_eq!(pad4(3), 1);
    assert_eq!(pad4(4), 0);
    let mut v = Vec::new();
    put_attr(&mut v, 0x0006, b"abc");
    assert_eq!(v.len(), 8, "3 字节值必须补到 8 字节");
    // STUN 首两位必须是 00，否则会被当成 ChannelData
    assert_eq!(msg_type(M_ALLOCATE, C_REQUEST) & 0xC000, 0);
}
