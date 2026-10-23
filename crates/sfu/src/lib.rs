//! 服务端 SFU（Selective Forwarding Unit）：把 N 个人的媒体在服务端汇聚再分发。
//!
//! 为什么必须要有它：mesh 是每人向其他人各发一路，上行 = 码率 × (人数−1)。
//! 8 人音频还行，几十人就是灾难；上百人更不可能。SFU 把"每人 N−1 路上行"
//! 变成"每人 1 路上行 + 1 路下行/订阅"，带宽与 CPU 都与人数线性而非平方相关。
//!
//! 架构参照 `refs/atm0s-media-server`（Rust + str0m，与我们同一套栈）的
//! `transport_webrtc` + `media_core` 分层，但按其思路重写而非翻译：
//! 这里只做单节点的"接入 + 转发"，跨节点级联（他们的 `media_connector`）
//! 留给下一层——单节点的容量先算清楚，级联才有意义。
//!
//! ## 关键设计
//!
//! * **每个参与者一个 UDP socket**。省掉 ICE ufrag 解复用：包从哪个 socket
//!   进来就是谁。代价是每人占一个端口（OS 上限远大于房间人数）。
//! * **SFU 永远是 answerer**（`MediaSession::new(false)`）：客户端发 offer，
//!   服务端回 answer，候选直接写在 answer 里，不需要 trickle。
//! * **转发的是编码后的载荷**，不解码不混流：收到谁的 `MediaData` 就原样写给
//!   其他每个人的 `write_audio`，由各自的会话映射成自己协商到的 payload type。
//!   这是 SFU 与 MCU 的本质区别，也是它能扛几百人的原因。
//! * **入会是服务端裁决的**（#58）：`join` 只收 Bearer token，不接受客户端
//!   自报的 peer id；账号由平台会话解析，房间成员资格走平台既有的
//!   `conv_member_allowed`。非成员与不存在的房间返回同一个 404，避免探测。
//!
//! ## 容量
//!
//! 音频 20ms 一帧、每人 50 包/秒。转发量 = 发布者数 × 订阅者数 × 50。
//! 100 人音频全互联转发约 50 万包/秒——单节点可以，但已经很重；
//! 几百人需要把房间拆到多节点并用级联（每人只在本节点内转发，跨节点各一条
//! 上行）。所以本模块刻意把"一个房间 = 一个节点"作为边界。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use anyhow::Context as _;
use serde::Serialize;
use wxlike_platform::{PlatformError, PlatformState};
use wxlike_rtc::media::{Datagram, MediaSession};

/// 默认房间容量（与通话控制面的产品上限一致）。
pub const DEFAULT_CAPACITY: usize = 50;

/// `join` 的拒绝原因。HTTP 状态随错误一起给出，接线时直接映射。
#[derive(Debug, thiserror::Error)]
pub enum SfuJoinError {
    #[error("missing or invalid session token")]
    Unauthorized,
    /// 非成员、房间不存在、conv 格式非法一律 404：不泄露房间是否存在。
    #[error("not found")]
    NotFound,
    #[error("room is full")]
    Full,
    /// 同一个账号已经在房里：不顶替旧会话（旧行为等于"用他人 id 踢人"）。
    #[error("peer already joined")]
    PeerConflict,
    #[error("internal failure: {0}")]
    Internal(String),
    #[error("media negotiation failed: {0}")]
    Media(#[from] anyhow::Error),
}

impl SfuJoinError {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Unauthorized => 401,
            Self::NotFound => 404,
            Self::Full | Self::PeerConflict => 409,
            Self::Media(_) => 400,
            Self::Internal(_) => 500,
        }
    }
}

/// 加入成功后返回给客户端的东西。
#[derive(Debug, Clone, Serialize)]
pub struct JoinReply {
    /// 服务端的 SDP answer（含候选）。
    pub sdp: serde_json::Value,
    /// 服务端媒体地址，客户端把数据报发到这里。
    pub media_addr: SocketAddr,
}

struct Participant {
    session: MediaSession,
    socket: UdpSocket,
    /// 写进 SDP 的地址（绑 0.0.0.0 时换成对外地址）。
    advertised: SocketAddr,
    /// 是否请求了视频 m-line（当前只转发音频，视频留接口）。
    #[allow(dead_code)]
    video: bool,
}

/// 一个房间（= 一个 SFU 节点上的一个转发域）。
pub struct SfuRoom {
    /// 会话与成员资格的唯一来源（只调用，不修改平台）。
    platform: PlatformState,
    /// 本房间对应的会话 id（直聊 `d-a~b` / 群 `g-…` / 频道）。
    conv: String,
    /// 房间人数上限（含正在协商中的参与者）。
    capacity: usize,
    participants: HashMap<String, Participant>,
    /// 对外宣告的 IP（`WXLIKE_SFU_ADVERTISE` 或自动探测）。
    advertise: IpAddr,
    /// 收到的包数（诊断：0 说明根本没通到 SFU）。
    pub recv_packets: u64,
    /// 发出的包数。
    pub sent_packets: u64,
    /// 会话泵出错次数（诊断）。
    pub step_errors: u64,
    /// 端口分配：0 表示让 OS 选（测试用），否则从 base 递增。
    base_port: u16,
    next_port: u16,
}

impl SfuRoom {
    /// `advertise = None` 时自动探测本机网卡地址。
    pub fn new(
        platform: PlatformState,
        conv: &str,
        advertise: Option<IpAddr>,
        base_port: u16,
    ) -> Self {
        Self::with_capacity(platform, conv, DEFAULT_CAPACITY, advertise, base_port)
    }

    pub fn with_capacity(
        platform: PlatformState,
        conv: &str,
        capacity: usize,
        advertise: Option<IpAddr>,
        base_port: u16,
    ) -> Self {
        let advertise = advertise.unwrap_or_else(|| {
            primary_ipv4()
                .map(IpAddr::V4)
                .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
        });
        Self {
            platform,
            conv: conv.to_string(),
            capacity,
            participants: HashMap::new(),
            recv_packets: 0,
            sent_packets: 0,
            step_errors: 0,
            advertise,
            base_port,
            next_port: base_port,
        }
    }

    /// 从环境变量构造：`WXLIKE_SFU_ADVERTISE` / `WXLIKE_SFU_PORT`。
    pub fn from_env(platform: PlatformState, conv: &str) -> Self {
        let advertise = std::env::var("WXLIKE_SFU_ADVERTISE")
            .ok()
            .and_then(|s| s.parse::<IpAddr>().ok());
        let base = std::env::var("WXLIKE_SFU_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);
        Self::new(platform, conv, advertise, base)
    }

    pub fn conv(&self) -> &str {
        &self.conv
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn participants(&self) -> Vec<String> {
        self.participants.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.participants.len()
    }

    pub fn is_empty(&self) -> bool {
        self.participants.is_empty()
    }

    pub fn connected(&self) -> usize {
        self.participants
            .values()
            .filter(|p| p.session.is_connected())
            .count()
    }

    /// 加入：先鉴权与校验，再收 offer、回 answer。
    ///
    /// 参与者 id 一律取平台会话解析出的 user_id；客户端自报的 id 不进这个函数。
    pub fn join(
        &mut self,
        token: &str,
        offer: serde_json::Value,
        video: bool,
    ) -> Result<JoinReply, SfuJoinError> {
        let user = self
            .platform
            .lookup_token(token)
            .map_err(|e| SfuJoinError::Internal(format!("session lookup failed: {e}")))?
            .ok_or(SfuJoinError::Unauthorized)?;

        // 非成员与不存在的房间走同一个错误，外部无法区分"房间存在但我不在"。
        match self.platform.conv_member_allowed(&self.conv, &user) {
            Ok(true) => {}
            Ok(false) | Err(PlatformError::NotFound) | Err(PlatformError::BadInput(_)) => {
                return Err(SfuJoinError::NotFound)
            }
            Err(e) => {
                return Err(SfuJoinError::Internal(format!(
                    "membership lookup failed: {e}"
                )))
            }
        }

        // 重名 = 拒绝，不再 leave 旧会话：否则知道对方 id 就能把人踢下线。
        if self.participants.contains_key(&user) {
            return Err(SfuJoinError::PeerConflict);
        }
        if self.participants.len() >= self.capacity {
            return Err(SfuJoinError::Full);
        }

        let socket = self.bind_socket()?;
        socket
            .set_nonblocking(true)
            .context("SFU socket 设为非阻塞失败")?;
        let local = socket.local_addr().context("SFU socket 取本地地址失败")?;
        let advertised = SocketAddr::new(self.advertise, local.port());

        // answerer 一侧：候选必须先注入，answer 里才有 candidate 行
        let mut session = MediaSession::new(false);
        session.add_host_candidate(advertised);
        let sdp = session
            .accept_offer(offer)
            .context("SFU 无法接受该 offer（SDP 解析或编解码协商失败）")?;

        self.participants.insert(
            user,
            Participant {
                session,
                socket,
                advertised,
                video,
            },
        );
        Ok(JoinReply {
            sdp,
            media_addr: advertised,
        })
    }

    /// 移除参与者。`user` 是服务端绑定的账号 id（与 `join` 的解析结果同一来源）。
    pub fn leave(&mut self, user: &str) {
        self.participants.remove(user);
    }

    /// 一轮：收包 → 喂会话 → 泵 → 把媒体转发给其他所有人 → 发出去。
    ///
    /// 调用方（服务端循环）应当 10ms 调一次。整轮是**非阻塞**的。
    pub fn poll(&mut self) {
        // 1) 收：从每个参与者的 socket 收干净
        let mut buf = [0u8; 2048];
        for p in self.participants.values_mut() {
            let local = p.advertised;
            while let Ok((n, from)) = p.socket.recv_from(&mut buf) {
                self.recv_packets += 1;
                let d = Datagram {
                    data: buf[..n].to_vec(),
                    source: from,
                    destination: local,
                };
                let _ = p.session.handle_receive(&d);
            }
        }

        // 2) 泵 + 收集待转发的音频（先收齐再转发，避免同时可变借用两个参与者）
        let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
        for (id, p) in self.participants.iter_mut() {
            let audio_mid = p.session.audio_mid();
            let Ok((events, datagrams)) = p.session.step() else {
                self.step_errors += 1;
                continue;
            };
            for d in datagrams {
                if p.socket.send_to(&d.data, d.destination).is_ok() {
                    self.sent_packets += 1;
                }
            }
            if std::env::var("SFU_DEBUG").is_ok() {
                for ev in &events {
                    eprintln!("[sfu {}] {:?}", id, ev);
                }
            }
            for ev in events {
                if let str0m::Event::MediaData(md) = ev {
                    if Some(md.mid) == audio_mid {
                        pending.push((id.clone(), md.data));
                    }
                }
            }
        }

        // 3) 转发：同一份编码载荷写给其他每个人，各自映射到自己的 PT
        for (from, payload) in pending {
            for (id, p) in self.participants.iter_mut() {
                if *id == from {
                    continue;
                }
                // 还没协商完/已断开的会话会报错，丢掉即可
                let _ = p.session.write_audio(&payload);
            }
        }
    }

    fn bind_socket(&mut self) -> anyhow::Result<UdpSocket> {
        if self.base_port == 0 {
            // 测试/单机：让 OS 选端口
            return Ok(UdpSocket::bind("0.0.0.0:0")?);
        }
        // 固定端口段：便于防火墙放行
        let mut last_err = None;
        for _ in 0..64 {
            let port = self.next_port;
            self.next_port = if self.next_port >= 65500 {
                self.base_port
            } else {
                self.next_port + 1
            };
            match UdpSocket::bind(("0.0.0.0", port)) {
                Ok(s) => return Ok(s),
                Err(e) => last_err = Some(e),
            }
        }
        Err(anyhow::anyhow!(
            "端口段 {} 起连续 64 个都绑不上: {:?}",
            self.base_port,
            last_err
        ))
    }
}

/// 本机主网卡 IPv4（connect 一个 TEST-NET 地址拿路由选中的源地址）。
fn primary_ipv4() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(_) => None,
    }
}
