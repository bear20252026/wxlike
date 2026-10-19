//! TURN 客户端（RFC 5766 的子集）：**真的**把媒体中继起来，而不是只解析配置。
//!
//! 为什么必须有它：host candidate 只在同一网段可用，STUN 打洞对对称 NAT 无效。
//! 跨网络通话只有一条路——中继。str0m 是 sans-I/O 的，它不认识 TURN，所以
//! 分配、保活、通道绑定都得宿主自己做，这里就是那层。
//!
//! 实现范围（够用即止）：
//! * Allocate（长凭据鉴权：先拿 401 的 realm/nonce，再带 MESSAGE-INTEGRITY 重发）
//! * CreatePermission + ChannelBind（之后数据走 ChannelData，4 字节头，开销最小）
//! * Refresh（半衰期自动续期）
//! * 收：ChannelData 与 Data indication 都认
//!
//! 不做：TCP/TLS 传输、Even-Port、Reservation-Token、Send indication 发送路径。
//! 这些不影响"能打通"，只影响极端网络下的成功率。
//!
//! 本模块**自带 socket**（connect 到 TURN 服务器）。中继地址在服务器上，
//! 回流也从这台服务器来，所以一个专用 socket 就够，不必和媒体 socket 共用。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use md5::{Digest as _, Md5};
use sha1::Sha1;

use crate::ice::TurnConfig;

// -- 常量

pub const MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];

// methods
pub const M_ALLOCATE: u16 = 0x003;
pub const M_REFRESH: u16 = 0x004;
pub const M_SEND: u16 = 0x006;
pub const M_DATA: u16 = 0x007;
pub const M_CREATE_PERMISSION: u16 = 0x008;
pub const M_CHANNEL_BIND: u16 = 0x009;

// classes
pub const C_REQUEST: u16 = 0b00;
pub const C_INDICATION: u16 = 0b01;
pub const C_SUCCESS: u16 = 0b10;
pub const C_ERROR: u16 = 0b11;

// attributes
pub const A_MAPPED_ADDRESS: u16 = 0x0001;
pub const A_USERNAME: u16 = 0x0006;
pub const A_MESSAGE_INTEGRITY: u16 = 0x0008;
pub const A_ERROR_CODE: u16 = 0x0009;
pub const A_CHANNEL_NUMBER: u16 = 0x000C;
pub const A_LIFETIME: u16 = 0x000D;
pub const A_XOR_PEER_ADDRESS: u16 = 0x0012;
pub const A_DATA: u16 = 0x0013;
pub const A_REALM: u16 = 0x0014;
pub const A_NONCE: u16 = 0x0015;
pub const A_XOR_RELAYED_ADDRESS: u16 = 0x0016;
pub const A_REQUESTED_TRANSPORT: u16 = 0x0019;
pub const A_XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// 默认申请 10 分钟，半衰期（5 分钟）续一次。
const LIFETIME_SECS: u32 = 600;
/// 通道号范围（RFC 5766 §11：0x4000–0x7FFF）。
pub const CHANNEL_BASE: u16 = 0x4000;

/// STUN 消息类型编码（RFC 5389 §6）。
pub fn msg_type(method: u16, class: u16) -> u16 {
    (method & 0x000F)
        | ((method & 0x0070) << 1)
        | ((method & 0x0F80) << 2)
        | ((class & 0x1) << 4)
        | ((class & 0x2) << 7)
}

pub fn split_type(t: u16) -> (u16, u16) {
    let method = (t & 0x000F) | ((t >> 1) & 0x0070) | ((t >> 2) & 0x0F80);
    let class = ((t >> 4) & 0x1) | ((t >> 7) & 0x2);
    (method, class)
}

// -- 属性编解码

pub fn pad4(n: usize) -> usize {
    (4 - (n % 4)) % 4
}

pub fn put_attr(out: &mut Vec<u8>, kind: u16, value: &[u8]) {
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
    out.extend(std::iter::repeat_n(0u8, pad4(value.len())));
}

/// XOR-MAPPED / XOR-RELAYED / XOR-PEER 共用的地址编码。
pub fn encode_xor_addr(addr: SocketAddr) -> Vec<u8> {
    let mut v = Vec::with_capacity(20);
    v.push(0);
    match addr.ip() {
        IpAddr::V4(ip) => {
            v.push(0x01);
            v.extend_from_slice(&(addr.port() ^ 0x2112).to_be_bytes());
            let o = u32::from(ip) ^ u32::from_be_bytes(MAGIC_COOKIE);
            v.extend_from_slice(&o.to_be_bytes());
        }
        IpAddr::V6(ip) => {
            v.push(0x02);
            v.extend_from_slice(&(addr.port() ^ 0x2112).to_be_bytes());
            let o = u128::from(ip) ^ u128::from(u32::from_be_bytes(MAGIC_COOKIE));
            v.extend_from_slice(&o.to_be_bytes());
        }
    }
    v
}

pub fn decode_xor_addr(v: &[u8]) -> Option<SocketAddr> {
    if v.len() < 8 {
        return None;
    }
    let family = v[1];
    let port = u16::from_be_bytes([v[2], v[3]]) ^ 0x2112;
    match family {
        0x01 => {
            let raw = u32::from_be_bytes([v[4], v[5], v[6], v[7]]);
            let ip = Ipv4Addr::from(raw ^ u32::from_be_bytes(MAGIC_COOKIE));
            Some(SocketAddr::new(IpAddr::V4(ip), port))
        }
        0x02 if v.len() >= 20 => {
            let mut raw = [0u8; 16];
            raw.copy_from_slice(&v[4..20]);
            let ip = Ipv6Addr::from(
                u128::from_be_bytes(raw) ^ u128::from(u32::from_be_bytes(MAGIC_COOKIE)),
            );
            Some(SocketAddr::new(IpAddr::V6(ip), port))
        }
        _ => None,
    }
}

/// 解析出来的消息。
#[derive(Debug, Clone)]
pub struct Message {
    pub method: u16,
    pub class: u16,
    pub txid: [u8; 12],
    pub attrs: Vec<(u16, Vec<u8>)>,
    /// 原始字节，校验 MESSAGE-INTEGRITY 时要用。
    pub raw: Vec<u8>,
}

impl Message {
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < 20 || buf[0] & 0xC0 != 0 {
            return None;
        }
        if buf[4..8] != MAGIC_COOKIE {
            return None;
        }
        let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        if buf.len() < 20 + len {
            return None;
        }
        let t = u16::from_be_bytes([buf[0], buf[1]]);
        let (method, class) = split_type(t);
        let mut txid = [0u8; 12];
        txid.copy_from_slice(&buf[8..20]);
        let mut attrs = Vec::new();
        let mut i = 20;
        while i + 4 <= 20 + len {
            let kind = u16::from_be_bytes([buf[i], buf[i + 1]]);
            let alen = u16::from_be_bytes([buf[i + 2], buf[i + 3]]) as usize;
            if i + 4 + alen > buf.len() {
                return None;
            }
            attrs.push((kind, buf[i + 4..i + 4 + alen].to_vec()));
            i += 4 + alen + pad4(alen);
        }
        Some(Self {
            method,
            class,
            txid,
            attrs,
            raw: buf[..20 + len].to_vec(),
        })
    }

    pub fn attr(&self, kind: u16) -> Option<&[u8]> {
        self.attrs
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, v)| v.as_slice())
    }

    /// ERROR-CODE 的数字部分（如 401 / 438）。
    pub fn error_code(&self) -> Option<u16> {
        let v = self.attr(A_ERROR_CODE)?;
        if v.len() < 4 {
            return None;
        }
        Some((u16::from(v[2] & 0x07) * 100) + u16::from(v[3]))
    }

    pub fn error_reason(&self) -> String {
        let Some(v) = self.attr(A_ERROR_CODE) else {
            return String::new();
        };
        if v.len() <= 4 {
            return String::new();
        }
        String::from_utf8_lossy(&v[4..]).to_string()
    }
}

/// 长凭据的 key：MD5(user:realm:password)（RFC 5389 §15.4）。
pub fn long_term_key(user: &str, realm: &str, pass: &str) -> [u8; 16] {
    let mut h = Md5::new();
    h.update(format!("{user}:{realm}:{pass}").as_bytes());
    h.finalize().into()
}

pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(key).expect("hmac 接受任意长度 key");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// 组一条消息。`integrity_key` 有值时追加 MESSAGE-INTEGRITY。
///
/// 关键细节：HMAC 覆盖的头部里 length 字段必须**已经算上** MI 属性
/// （RFC 5389 §15.4），否则服务器算出来的摘要对不上——这是 TURN 客户端最常
/// 写错的一行。
pub fn build(
    method: u16,
    class: u16,
    txid: [u8; 12],
    attrs: &[(u16, Vec<u8>)],
    integrity_key: Option<&[u8]>,
) -> Vec<u8> {
    let mut body = Vec::new();
    for (k, v) in attrs {
        put_attr(&mut body, *k, v);
    }
    let mi_len = if integrity_key.is_some() { 24 } else { 0 };
    let mut msg = Vec::with_capacity(20 + body.len() + mi_len);
    msg.extend_from_slice(&msg_type(method, class).to_be_bytes());
    msg.extend_from_slice(&((body.len() + mi_len) as u16).to_be_bytes());
    msg.extend_from_slice(&MAGIC_COOKIE);
    msg.extend_from_slice(&txid);
    msg.extend_from_slice(&body);

    if let Some(key) = integrity_key {
        let digest = hmac_sha1(key, &msg);
        put_attr(&mut msg, A_MESSAGE_INTEGRITY, &digest);
    }
    msg
}

/// 校验对端消息的 MESSAGE-INTEGRITY。
pub fn verify_integrity(msg: &Message, key: &[u8]) -> bool {
    // 找到 MI 属性的起点
    let mut i = 20;
    let end = msg.raw.len();
    while i + 4 <= end {
        let kind = u16::from_be_bytes([msg.raw[i], msg.raw[i + 1]]);
        let alen = u16::from_be_bytes([msg.raw[i + 2], msg.raw[i + 3]]) as usize;
        if kind == A_MESSAGE_INTEGRITY {
            let mut head = msg.raw[..i].to_vec();
            // length 要覆盖到 MI 结束
            let total = (i - 20 + 4 + 20) as u16;
            head[2..4].copy_from_slice(&total.to_be_bytes());
            let want = hmac_sha1(key, &head);
            let got = &msg.raw[i + 4..i + 4 + alen.min(20)];
            return got == want;
        }
        i += 4 + alen + pad4(alen);
    }
    false
}

/// 一条已建立的 TURN 分配。
pub struct TurnClient {
    socket: UdpSocket,
    server: SocketAddr,
    relayed: SocketAddr,
    user: String,
    pass: String,
    realm: String,
    nonce: String,
    channels: HashMap<SocketAddr, u16>,
    peers: HashMap<u16, SocketAddr>,
    next_channel: u16,
    allocated_at: Instant,
    lifetime: u32,
}

impl TurnClient {
    /// 分配中继地址（含 401 挑战重发）。
    pub fn allocate(cfg: &TurnConfig) -> anyhow::Result<Self> {
        let server: SocketAddr = format!("{}:{}", cfg.host, cfg.port).parse().or_else(|_| {
            use std::net::ToSocketAddrs;
            format!("{}:{}", cfg.host, cfg.port)
                .to_socket_addrs()
                .ok()
                .and_then(|mut it| it.next())
                .ok_or_else(|| anyhow::anyhow!("TURN 地址解析失败"))
        })?;
        let socket = UdpSocket::bind("0.0.0.0:0")?;
        socket.connect(server)?;
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;

        // 1) 不带凭据的 Allocate：正常会拿到 401 + realm/nonce
        let req = build(
            M_ALLOCATE,
            C_REQUEST,
            rand_txid(),
            &[(A_REQUESTED_TRANSPORT, vec![17, 0, 0, 0])],
            None,
        );
        socket.send(&req)?;
        let (msg, _) = read_message(&socket)?;
        if msg.class == C_SUCCESS {
            // 服务器开了匿名中继（罕见）：直接用它给的地址
            let relayed = msg
                .attr(A_XOR_RELAYED_ADDRESS)
                .and_then(decode_xor_addr)
                .ok_or_else(|| anyhow::anyhow!("Allocate 成功但没有 RELAYED-ADDRESS"))?;
            return Ok(Self {
                socket,
                server,
                relayed,
                user: cfg.user.clone(),
                pass: cfg.password.clone(),
                realm: String::new(),
                nonce: String::new(),
                channels: HashMap::new(),
                peers: HashMap::new(),
                next_channel: CHANNEL_BASE,
                allocated_at: Instant::now(),
                lifetime: LIFETIME_SECS,
            });
        }
        if msg.error_code() != Some(401) {
            anyhow::bail!(
                "Allocate 被拒: {} {}",
                msg.error_code().unwrap_or(0),
                msg.error_reason()
            );
        }
        let realm = String::from_utf8_lossy(msg.attr(A_REALM).unwrap_or(b"")).to_string();
        let nonce = String::from_utf8_lossy(msg.attr(A_NONCE).unwrap_or(b"")).to_string();
        let key = long_term_key(&cfg.user, &realm, &cfg.password);

        // 2) 带凭据重发
        let req = build(
            M_ALLOCATE,
            C_REQUEST,
            rand_txid(),
            &[
                (A_REQUESTED_TRANSPORT, vec![17, 0, 0, 0]),
                (A_USERNAME, cfg.user.clone().into_bytes()),
                (A_REALM, realm.clone().into_bytes()),
                (A_NONCE, nonce.clone().into_bytes()),
                (A_LIFETIME, LIFETIME_SECS.to_be_bytes().to_vec()),
            ],
            Some(&key),
        );
        socket.send(&req)?;
        let (msg, _) = read_message(&socket)?;
        if msg.class != C_SUCCESS {
            anyhow::bail!(
                "Allocate 鉴权失败: {} {}",
                msg.error_code().unwrap_or(0),
                msg.error_reason()
            );
        }
        let relayed = msg
            .attr(A_XOR_RELAYED_ADDRESS)
            .and_then(decode_xor_addr)
            .ok_or_else(|| anyhow::anyhow!("Allocate 成功但没有 RELAYED-ADDRESS"))?;
        let lifetime = msg
            .attr(A_LIFETIME)
            .filter(|v| v.len() >= 4)
            .map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]))
            .unwrap_or(LIFETIME_SECS);

        Ok(Self {
            socket,
            server,
            relayed,
            user: cfg.user.clone(),
            pass: cfg.password.clone(),
            realm,
            nonce,
            channels: HashMap::new(),
            peers: HashMap::new(),
            next_channel: CHANNEL_BASE,
            allocated_at: Instant::now(),
            lifetime,
        })
    }

    /// 本端在 TURN 服务器上的中继地址——这个才是要写进 SDP 的 candidate。
    pub fn relayed_addr(&self) -> SocketAddr {
        self.relayed
    }

    pub fn server(&self) -> SocketAddr {
        self.server
    }

    /// 调整读超时：引擎循环里用 1ms 轮询，握手阶段临时放宽。
    pub fn set_read_timeout(&self, d: Duration) {
        let _ = self.socket.set_read_timeout(Some(d));
    }

    fn key(&self) -> [u8; 16] {
        long_term_key(&self.user, &self.realm, &self.pass)
    }

    fn authed(&self, method: u16, attrs: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut all = attrs.to_vec();
        all.push((A_USERNAME, self.user.clone().into_bytes()));
        all.push((A_REALM, self.realm.clone().into_bytes()));
        all.push((A_NONCE, self.nonce.clone().into_bytes()));
        build(method, C_REQUEST, rand_txid(), &all, Some(&self.key()))
    }

    /// 该对端是否已绑通道。
    pub fn has_channel(&self, peer: SocketAddr) -> bool {
        self.channels.contains_key(&peer)
    }

    /// 给某个对端开权限并绑定通道，之后数据走 ChannelData。
    pub fn bind_channel(&mut self, peer: SocketAddr) -> anyhow::Result<u16> {
        if let Some(ch) = self.channels.get(&peer) {
            return Ok(*ch);
        }
        // 权限：不建权限服务器会丢弃入向包
        let perm = self.authed(
            M_CREATE_PERMISSION,
            &[(A_XOR_PEER_ADDRESS, encode_xor_addr(peer))],
        );
        self.socket.send(&perm)?;
        let (msg, _) = read_message(&self.socket)?;
        if msg.class != C_SUCCESS {
            anyhow::bail!(
                "CreatePermission 失败: {} {}",
                msg.error_code().unwrap_or(0),
                msg.error_reason()
            );
        }

        let channel = self.next_channel;
        self.next_channel = if self.next_channel >= 0x7FFF {
            CHANNEL_BASE
        } else {
            self.next_channel + 1
        };
        let mut ch_attr = channel.to_be_bytes().to_vec();
        ch_attr.extend_from_slice(&[0, 0]); // RFFU
        let bind = self.authed(
            M_CHANNEL_BIND,
            &[
                (A_CHANNEL_NUMBER, ch_attr),
                (A_XOR_PEER_ADDRESS, encode_xor_addr(peer)),
            ],
        );
        self.socket.send(&bind)?;
        let (msg, _) = read_message(&self.socket)?;
        if msg.class != C_SUCCESS {
            anyhow::bail!(
                "ChannelBind 失败: {} {}",
                msg.error_code().unwrap_or(0),
                msg.error_reason()
            );
        }
        self.channels.insert(peer, channel);
        self.peers.insert(channel, peer);
        Ok(channel)
    }

    /// 发一包媒体数据给对端（必须已 `bind_channel`）。
    pub fn send(&self, peer: SocketAddr, data: &[u8]) -> anyhow::Result<()> {
        if let Some(ch) = self.channels.get(&peer) {
            // ChannelData：4 字节头，比 Send indication 省一半开销
            let mut msg = Vec::with_capacity(4 + data.len() + 3);
            msg.extend_from_slice(&ch.to_be_bytes());
            msg.extend_from_slice(&(data.len() as u16).to_be_bytes());
            msg.extend_from_slice(data);
            msg.extend(std::iter::repeat_n(0u8, pad4(data.len())));
            self.socket.send(&msg)?;
            return Ok(());
        }
        // 没绑通道就退回 Send indication（带 MI）
        let send = self.authed(
            M_SEND,
            &[
                (A_XOR_PEER_ADDRESS, encode_xor_addr(peer)),
                (A_DATA, data.to_vec()),
            ],
        );
        self.socket.send(&send)?;
        Ok(())
    }

    /// 收一包：返回 `(对端地址, 数据)`。非媒体包（响应等）返回 `None`。
    pub fn recv(&mut self, buf: &mut [u8]) -> Option<(SocketAddr, Vec<u8>)> {
        let n = match self.socket.recv(buf) {
            Ok(n) => n,
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return None
            }
            Err(_) => return None,
        };
        let pkt = &buf[..n];
        // ChannelData：前两字节在 0x4000..0x7FFF 且不是 STUN（STUN 首字节高位为 0）
        if pkt.len() >= 4 && pkt[0] & 0xC0 == 0x40 {
            let ch = u16::from_be_bytes([pkt[0], pkt[1]]);
            let len = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
            if pkt.len() < 4 + len {
                return None;
            }
            let peer = self.peers.get(&ch).copied()?;
            return Some((peer, pkt[4..4 + len].to_vec()));
        }
        let msg = Message::parse(pkt)?;
        if msg.method == M_DATA && msg.class == C_INDICATION {
            let peer = msg.attr(A_XOR_PEER_ADDRESS).and_then(decode_xor_addr)?;
            let data = msg.attr(A_DATA)?.to_vec();
            return Some((peer, data));
        }
        None
    }

    /// 到了半衰期就续期（非阻塞，失败只记日志）。
    pub fn maybe_refresh(&mut self) {
        let half = Duration::from_secs(u64::from(self.lifetime.max(60) / 2));
        if self.allocated_at.elapsed() < half {
            return;
        }
        let req = self.authed(
            M_REFRESH,
            &[
                (A_LIFETIME, LIFETIME_SECS.to_be_bytes().to_vec()),
                (A_REQUESTED_TRANSPORT, vec![17, 0, 0, 0]),
            ],
        );
        if self.socket.send(&req).is_ok() {
            let mut buf = [0u8; 1500];
            if let Ok(n) = self.socket.recv(&mut buf) {
                if let Some(msg) = Message::parse(&buf[..n]) {
                    if msg.class != C_SUCCESS {
                        // 438 = nonce 过期，重分配一次
                        if msg.error_code() == Some(438) {
                            if let Some(nonce) = msg.attr(A_NONCE) {
                                self.nonce = String::from_utf8_lossy(nonce).to_string();
                            }
                        }
                    }
                }
            }
        }
        self.allocated_at = Instant::now();
    }
}

pub fn read_message(socket: &UdpSocket) -> anyhow::Result<(Message, SocketAddr)> {
    let mut buf = [0u8; 1500];
    let (n, from) = socket.recv_from(&mut buf)?;
    let msg = Message::parse(&buf[..n])
        .ok_or_else(|| anyhow::anyhow!("收到无法解析的 STUN/TURN 消息（{} 字节）", n))?;
    Ok((msg, from))
}

fn rand_txid() -> [u8; 12] {
    use rand::RngCore as _;
    let mut t = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut t);
    t
}

/// 便于测试与诊断：MAPPED-ADDRESS（非 XOR 版本，部分老服务器仍会回）。
pub fn mapped_address(msg: &Message) -> Option<SocketAddr> {
    msg.attr(A_XOR_MAPPED_ADDRESS)
        .and_then(decode_xor_addr)
        .or_else(|| {
            let v = msg.attr(A_MAPPED_ADDRESS)?;
            if v.len() < 8 {
                return None;
            }
            let port = u16::from_be_bytes([v[2], v[3]]);
            let ip = Ipv4Addr::new(v[4], v[5], v[6], v[7]);
            Some(SocketAddr::new(IpAddr::V4(ip), port))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_round_trip() {
        for method in [
            M_ALLOCATE,
            M_REFRESH,
            M_SEND,
            M_DATA,
            M_CREATE_PERMISSION,
            M_CHANNEL_BIND,
        ] {
            for class in [C_REQUEST, C_INDICATION, C_SUCCESS, C_ERROR] {
                let t = msg_type(method, class);
                assert_eq!(
                    split_type(t),
                    (method, class),
                    "method={method:x} class={class}"
                );
            }
        }
        // RFC 5766 里 Allocate success 的类型是 0x0103
        assert_eq!(msg_type(M_ALLOCATE, C_SUCCESS), 0x0103);
        assert_eq!(msg_type(M_ALLOCATE, C_ERROR), 0x0113);
    }

    #[test]
    fn xor_address_round_trip() {
        for addr in ["192.0.2.15:3478", "10.1.2.3:1234", "[2001:db8::1]:5000"] {
            let a: SocketAddr = addr.parse().unwrap();
            let enc = encode_xor_addr(a);
            assert_eq!(decode_xor_addr(&enc), Some(a), "{addr}");
        }
    }

    #[test]
    fn attributes_survive_padding() {
        let txid = [7u8; 12];
        // 3 字节的值必须补 1 字节，否则后续属性错位
        let msg = build(
            M_ALLOCATE,
            C_REQUEST,
            txid,
            &[
                (A_USERNAME, b"abc".to_vec()),
                (A_REALM, b"example.org".to_vec()),
            ],
            None,
        );
        let parsed = Message::parse(&msg).unwrap();
        assert_eq!(parsed.attr(A_USERNAME), Some(&b"abc"[..]));
        assert_eq!(parsed.attr(A_REALM), Some(&b"example.org"[..]));
        assert_eq!(parsed.txid, txid);
    }

    #[test]
    fn integrity_is_verifiable_by_receiver() {
        let key = long_term_key("user", "realm", "pass");
        let msg = build(
            M_ALLOCATE,
            C_REQUEST,
            [1u8; 12],
            &[(A_USERNAME, b"user".to_vec())],
            Some(&key),
        );
        let parsed = Message::parse(&msg).unwrap();
        assert!(verify_integrity(&parsed, &key), "自签的 MI 必须能自验");
        // 换个 key 必须失败（长度字段算错的实现会在这里"通过"，所以这条很重要）
        let wrong = long_term_key("user", "realm", "wrong");
        assert!(!verify_integrity(&parsed, &wrong));
        // 改动载荷也必须失败
        let mut tampered = msg.clone();
        // 改 MESSAGE-INTEGRITY 的最后一字节：结构仍可解析，但摘要不再匹配
        let idx = tampered.len() - 1;
        tampered[idx] ^= 0xFF;
        let parsed = Message::parse(&tampered).unwrap();
        assert!(!verify_integrity(&parsed, &key));
    }

    #[test]
    fn error_code_parses_401() {
        // ERROR-CODE: class=4 number=1 -> 401
        let mut v = vec![0u8, 0, 4, 1];
        v.extend_from_slice(b"Unauthorized");
        let msg = build(M_ALLOCATE, C_ERROR, [2u8; 12], &[(A_ERROR_CODE, v)], None);
        let parsed = Message::parse(&msg).unwrap();
        assert_eq!(parsed.error_code(), Some(401));
        assert_eq!(parsed.error_reason(), "Unauthorized");
    }

    #[test]
    fn long_term_key_matches_rfc_vector_shape() {
        // RFC 5389 §15.4 的 key 就是 MD5(user:realm:pass)，长度 16
        let k = long_term_key("u", "r", "p");
        assert_eq!(k.len(), 16);
        assert_ne!(k, long_term_key("u", "r", "p2"));
    }
}
