//! 通话引擎：把 `wxlike_rtc::host::CallHost` 接到真实网络与 UI 上。
//!
//! 分工：
//! * `CallHost`（rtc crate）——纯逻辑，返回"该发什么、发到哪"；
//! * 本模块——唯一碰 socket / HTTP / 事件的地方，跑在一个专用 OS 线程上。
//!
//! 为什么用阻塞线程而不是 tokio 任务：`ureq` 是同步客户端，`MediaSession`
//! 需要 10ms 量级的固定节拍。混进 async 运行时要不停 `spawn_blocking`，
//! 反而更容易把节拍拖歪。一个线程 + `UdpSocket::set_nonblocking` + 10ms
//! 睡眠，节奏稳定且代码只有一屏。
//!
//! 打开 `audio` feature 时还会接上真实声卡：麦克风 → Opus → `write_audio`，
//! `Effect::Audio` → Opus 解码 → 扬声器。没有声卡（CI / 无头）时这一步静默
//! 跳过，引擎照常跑，只是听不见声音。
//!
//! 线程里的循环每轮做四件事：
//! 1. 收 UI 命令（接听 / 挂断）；
//! 2. `host.tick` 推进状态机与媒体，把 `Transmit` 真发出去；
//! 3. 非阻塞收 UDP 包喂回 `host`；
//! 4. 每 300ms 拉一次信令中继，同时把要发的信令 POST 上去。

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use wxlike_rtc::host::{CallHost, Effect};
use wxlike_rtc::ice::TurnConfig;
use wxlike_rtc::signal::Signal;

/// UI → 引擎的命令。
pub enum Cmd {
    Accept,
    Hangup,
    Stop,
}

/// 引擎对外的状态快照（UI 直接渲染）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct EngineStatus {
    pub active: bool,
    pub conv: String,
    pub call_id: String,
    /// 是否还有一通"响铃中"的来电（决定 UI 要不要显示接听按钮）。
    pub ringing: bool,
    pub connected: bool,
    pub phase: String,
    /// 收到的音频帧计数（没有声卡时也能证明媒体面在流动）。
    pub audio_frames: u64,
    /// 本端麦克风编码后送出的帧数（`audio` feature 关闭时恒为 0）。
    pub mic_frames: u64,
    /// 本通通话的对端总数（1 = 一对一直聊，>1 = 多方 mesh）。
    pub peers: usize,
    /// 其中已连通的对端数。
    pub connected_peers: usize,
}

/// 引擎句柄，挂在 Tauri 的 managed state 里。
#[derive(Default)]
pub struct CallEngine {
    stop: Arc<AtomicBool>,
    tx: Mutex<Option<Sender<Cmd>>>,
    status: Arc<Mutex<EngineStatus>>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// 声卡一侧：麦克风采集 + Opus 编解码 + 播放。
///
/// 只在 `audio` feature 下存在。任何一步失败（没设备、格式不支持）都退化成
/// `None` 并打印一行原因——通话本身不能因为没麦克风就起不来。
#[cfg(feature = "audio")]
struct AudioLeg {
    /// 只为持有流（drop 即停采集），不直接读。
    _mic: Option<wxlike_rtc::audio_io::MicStream>,
    speaker: Option<wxlike_rtc::audio_io::SpeakerSink>,
    codec: Option<wxlike_rtc::capture::OpusCodec>,
    rx: std::sync::mpsc::Receiver<Vec<i16>>,
    /// 编码后的帧数，用来判断"麦克风真的在工作"。
    sent: u64,
}

#[cfg(feature = "audio")]
impl AudioLeg {
    fn start() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let mic = match wxlike_rtc::audio_io::MicStream::start(tx) {
            Ok(m) => {
                eprintln!(
                    "[call] 麦克风已开: {} Hz / {} ch",
                    m.device_rate(),
                    m.device_channels()
                );
                Some(m)
            }
            Err(err) => {
                eprintln!("[call] 麦克风不可用，只收不发: {err}");
                None
            }
        };
        let speaker = match wxlike_rtc::audio_io::SpeakerSink::new() {
            Ok(sp) => {
                eprintln!(
                    "[call] 扬声器已开: {} Hz / {} ch",
                    sp.device_rate(),
                    sp.device_channels()
                );
                Some(sp)
            }
            Err(err) => {
                eprintln!("[call] 扬声器不可用，只发不收: {err}");
                None
            }
        };
        let codec = match wxlike_rtc::capture::OpusCodec::new() {
            Ok(c) => Some(c),
            Err(err) => {
                eprintln!("[call] Opus 初始化失败: {err}");
                None
            }
        };
        Self {
            _mic: mic,
            speaker,
            codec,
            rx,
            sent: 0,
        }
    }

    /// 把麦克风攒下的 PCM 帧编码后写进媒体会话。
    fn pump_mic(&mut self, host: &mut CallHost, call_id: &str) {
        let Some(codec) = self.codec.as_mut() else {
            return;
        };
        while let Ok(pcm) = self.rx.try_recv() {
            match codec.encode(&pcm) {
                Ok(bytes) => {
                    // 还没连上时 write_audio 会报错：正常，丢帧即可
                    if host.write_audio(call_id, &bytes).is_ok() {
                        self.sent += 1;
                    }
                }
                Err(err) => eprintln!("[call] Opus 编码失败: {err}"),
            }
        }
    }

    /// 对端音频帧 → PCM → 扬声器。
    fn play(&mut self, payload: &[u8]) {
        let (Some(codec), Some(speaker)) = (self.codec.as_mut(), self.speaker.as_mut()) else {
            return;
        };
        match codec.decode(payload) {
            Ok(pcm) => speaker.push(&pcm),
            Err(err) => eprintln!("[call] Opus 解码失败: {err}"),
        }
    }
}

/// 连服务端中继所需的最小上下文。
#[derive(Clone)]
pub struct RelayCfg {
    pub base: String,
    pub token: String,
}

/// TURN 短期凭据（coturn REST）。只活在内存里：不写盘、不进日志。
#[derive(Clone)]
struct TurnCredentials {
    username: String,
    password: String,
    uris: Vec<String>,
    expires_at: Instant,
}

/// 到期前这个窗口内重新取一次凭据（合同要求 5 分钟）。
const TURN_REFRESH_WINDOW: Duration = Duration::from_secs(300);
const TURN_CRED_TIMEOUT: Duration = Duration::from_secs(5);
const TURN_DEFAULT_PORT: u16 = 3478;

/// 进程内凭据缓存：同一登录态复用，换账号/换服务端自然失配。
struct TurnCache {
    base: String,
    token: String,
    credentials: TurnCredentials,
}

static TURN_CACHE: Mutex<Option<TurnCache>> = Mutex::new(None);

fn turn_credentials_url(base: &str) -> String {
    format!("{}/api/turn/credentials", base.trim_end_matches('/'))
}

/// 服务端 `GET /api/turn/credentials` 的响应语义：
/// 200 → 凭据；404 → 没配 `WXLIKE_TURN_SECRET`，调用方走直连降级。
fn parse_turn_credentials(
    status: u16,
    body: &serde_json::Value,
) -> Result<Option<TurnCredentials>, String> {
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(format!("TURN 凭据接口返回 HTTP {status}"));
    }
    let username = body["username"].as_str().unwrap_or_default();
    let password = body["password"].as_str().unwrap_or_default();
    if username.is_empty() || password.is_empty() {
        return Err("TURN 凭据响应缺少 username/password".into());
    }
    let ttl_seconds = body["ttl_seconds"]
        .as_u64()
        .filter(|ttl| *ttl > 0)
        .unwrap_or(3_600);
    let uris = body["uris"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(TurnCredentials {
        username: username.to_string(),
        password: password.to_string(),
        uris,
        expires_at: Instant::now() + Duration::from_secs(ttl_seconds),
    }))
}

fn fetch_turn_credentials(cfg: &RelayCfg) -> Result<Option<TurnCredentials>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TURN_CRED_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into();
    let mut resp = agent
        .get(&turn_credentials_url(&cfg.base))
        .header("Authorization", format!("Bearer {}", cfg.token))
        .call()
        .map_err(|e| format!("TURN 凭据请求失败: {e}"))?;
    let status = resp.status().as_u16();
    let body = if (200..300).contains(&status) {
        resp.body_mut()
            .read_json()
            .unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };
    parse_turn_credentials(status, &body)
}

fn needs_refresh(expires_at: Instant, now: Instant) -> bool {
    expires_at <= now + TURN_REFRESH_WINDOW
}

/// 内存缓存 + 到期前 5 分钟重取。404（未配置）不缓存，下次通话再问一次。
fn turn_credentials(cfg: &RelayCfg) -> Result<Option<TurnCredentials>, String> {
    let now = Instant::now();
    {
        let cache = TURN_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(hit) = cache.as_ref() {
            if hit.base == cfg.base
                && hit.token == cfg.token
                && !needs_refresh(hit.credentials.expires_at, now)
            {
                return Ok(Some(hit.credentials.clone()));
            }
        }
    }
    let fetched = fetch_turn_credentials(cfg)?;
    let mut cache = TURN_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    *cache = fetched.as_ref().map(|credentials| TurnCache {
        base: cfg.base.clone(),
        token: cfg.token.clone(),
        credentials: credentials.clone(),
    });
    Ok(fetched)
}

/// 服务端可能给多个 URI；这里只认 UDP 的 `turn:`（TurnClient 不支持 TLS/TCP）。
fn turn_config_from(creds: &TurnCredentials) -> Option<TurnConfig> {
    for uri in &creds.uris {
        let Some(rest) = uri.strip_prefix("turn:") else {
            continue;
        };
        let (authority, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q)),
            None => (rest, None),
        };
        if query.is_some_and(|q| q.split('&').any(|kv| kv == "transport=tcp")) {
            continue;
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => match port.parse::<u16>() {
                Ok(port) => (host.to_string(), port),
                Err(_) => continue,
            },
            None => (authority.to_string(), TURN_DEFAULT_PORT),
        };
        if host.is_empty() {
            continue;
        }
        return Some(TurnConfig {
            host,
            port,
            user: creds.username.clone(),
            password: creds.password.clone(),
        });
    }
    None
}

impl CallEngine {
    pub fn status(&self) -> EngineStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// 发起通话：每个对端一条媒体会话，返回各自的本地 call_id。
    pub fn start(
        &self,
        app: AppHandle,
        cfg: RelayCfg,
        me: String,
        conv: String,
        peers: Vec<String>,
        video: bool,
    ) -> Result<Vec<String>, String> {
        self.shutdown();
        if peers.is_empty() {
            return Err("没有可呼叫的对端".into());
        }

        let transport = Transport::open(&cfg)?;
        let mut host = CallHost::new(me, conv.clone(), transport.local());
        let (call_ids, effects) = host.start_many(&peers, video);
        let call_id = call_ids.first().cloned().unwrap_or_default();

        let (tx, rx) = channel();
        *self.tx.lock().unwrap() = Some(tx);
        self.stop.store(false, Ordering::SeqCst);
        *self.status.lock().unwrap() = EngineStatus {
            active: true,
            conv: conv.clone(),
            call_id: call_id.clone(),
            ringing: false,
            connected: false,
            phase: "calling".into(),
            audio_frames: 0,
            mic_frames: 0,
            peers: peers.len(),
            connected_peers: 0,
        };

        let stop = self.stop.clone();
        let status = self.status.clone();
        let handle = std::thread::Builder::new()
            .name("wxlike-call".into())
            .spawn(move || {
                run(app, cfg, host, transport, rx, stop, status, effects);
            })
            .map_err(|e| format!("启动通话线程失败: {e}"))?;
        *self.join.lock().unwrap() = Some(handle);
        Ok(call_ids)
    }

    pub fn send(&self, cmd: Cmd) {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(cmd);
        }
    }

    pub fn accept(&self) {
        self.send(Cmd::Accept);
    }

    pub fn hangup(&self) {
        self.send(Cmd::Hangup);
    }

    /// 停线程（幂等）。挂断后调用，避免线程泄漏。
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.send(Cmd::Stop);
        if let Some(handle) = self.join.lock().unwrap().take() {
            let _ = handle.join();
        }
        if let Ok(mut s) = self.status.lock() {
            *s = EngineStatus::default();
        }
    }
}

impl Drop for CallEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// 媒体传输：要么直连（自带 UDP socket），要么全程走 TURN 中继。
///
/// 服务端下发了 TURN URI 就**只**用中继，并且 SDP 里只放中继地址。理由：
/// host 候选在跨 NAT 时根本到不了对端，同时放两个会让 ICE 先试直连、失败后
/// 再回退，白白多几秒"正在连接"；而"看起来有候选但打不通"是最难解释的故障。
struct Transport {
    /// 写进 SDP 的本端地址。
    local: SocketAddr,
    direct: Option<UdpSocket>,
    turn: Option<wxlike_rtc::turn::TurnClient>,
    /// 取凭据与到期刷新用；只留在内存里。
    relay: RelayCfg,
    turn_expires_at: Option<Instant>,
    last_cred_check: Instant,
}

impl Transport {
    fn open(cfg: &RelayCfg) -> Result<Self, String> {
        let creds = turn_credentials(cfg)?;
        if let Some(creds) = creds {
            let Some(turn_cfg) = turn_config_from(&creds) else {
                // 服务端配了 secret 但没下发可用 URI：与"没配 TURN"一样走直连。
                return Self::direct(cfg);
            };
            let turn = wxlike_rtc::turn::TurnClient::allocate(&turn_cfg)
                .map_err(|e| format!("TURN 分配失败（检查服务端中继配置与可达性）: {e}"))?;
            turn.set_read_timeout(Duration::from_millis(1));
            let local = turn.relayed_addr();
            eprintln!("[call] TURN 中继就绪：本端 {local} via {}", turn.server());
            return Ok(Self {
                local,
                direct: None,
                turn: Some(turn),
                relay: cfg.clone(),
                turn_expires_at: Some(creds.expires_at),
                last_cred_check: Instant::now(),
            });
        }
        eprintln!("[call] 服务端未配置 TURN，媒体走直连");
        Self::direct(cfg)
    }

    fn direct(cfg: &RelayCfg) -> Result<Self, String> {
        let socket = UdpSocket::bind(bind_addr()).map_err(|e| format!("绑定媒体端口失败: {e}"))?;
        socket
            .set_nonblocking(true)
            .map_err(|e| format!("设置非阻塞失败: {e}"))?;
        let raw = socket.local_addr().map_err(|e| e.to_string())?;
        // 绑 0.0.0.0 时不能把这个地址写进 SDP（对端不可达），换成本机网卡地址
        let local = if raw.ip().is_unspecified() {
            SocketAddr::new(
                IpAddr::V4(primary_ipv4().unwrap_or(Ipv4Addr::LOCALHOST)),
                raw.port(),
            )
        } else {
            raw
        };
        Ok(Self {
            local,
            direct: Some(socket),
            turn: None,
            relay: cfg.clone(),
            turn_expires_at: None,
            last_cred_check: Instant::now(),
        })
    }

    /// 到期前 5 分钟把**下一次分配要用的**凭据换新。
    ///
    /// 已建立的 TURN 分配绑定在旧凭据上（rtc 的公开 API 不提供换凭据），
    /// 所以这里只预热缓存：本次通话继续用原分配，续期由 `TurnClient` 自己做。
    fn refresh_credentials_if_due(&mut self) {
        /// 凭据到期检查的节流间隔：poll 每帧都跑，不能每帧都发一次请求。
        const CRED_CHECK_INTERVAL: Duration = Duration::from_secs(30);
        let Some(expires_at) = self.turn_expires_at else {
            return;
        };
        if self.last_cred_check.elapsed() < CRED_CHECK_INTERVAL {
            return;
        }
        self.last_cred_check = Instant::now();
        if !needs_refresh(expires_at, Instant::now()) {
            return;
        }
        match turn_credentials(&self.relay) {
            Ok(Some(creds)) => self.turn_expires_at = Some(creds.expires_at),
            Ok(None) => self.turn_expires_at = None,
            Err(err) => eprintln!("[call] TURN 凭据刷新失败: {err}"),
        }
    }

    fn local(&self) -> SocketAddr {
        self.local
    }

    /// 发一包。中继模式下首次给某对端发之前要绑通道（阻塞一个 RTT，只发生一次）。
    fn send(&mut self, to: SocketAddr, bytes: &[u8]) {
        if let Some(turn) = self.turn.as_mut() {
            if !turn.has_channel(to) {
                turn.set_read_timeout(Duration::from_secs(3));
                let bound = turn.bind_channel(to);
                turn.set_read_timeout(Duration::from_millis(1));
                if let Err(err) = bound {
                    eprintln!("[call] TURN 绑通道失败（{to}）: {err}");
                    return;
                }
            }
            let _ = turn.send(to, bytes);
            return;
        }
        if let Some(sock) = &self.direct {
            let _ = sock.send_to(bytes, to);
        }
    }

    /// 收包并喂回宿主。中继模式下顺手续期分配。
    fn poll(&mut self, host: &mut CallHost) -> Vec<Effect> {
        self.refresh_credentials_if_due();
        let mut out = Vec::new();
        let local = self.local;
        if let Some(turn) = self.turn.as_mut() {
            turn.maybe_refresh();
            let mut buf = vec![0u8; 2048];
            while let Some((peer, data)) = turn.recv(&mut buf) {
                out.extend(host.handle_datagram(peer, local, &data));
            }
            return out;
        }
        if let Some(sock) = &self.direct {
            let mut buf = [0u8; 2048];
            while let Ok((n, from)) = sock.recv_from(&mut buf) {
                out.extend(host.handle_datagram(from, local, &buf[..n]));
            }
        }
        out
    }
}

fn bind_addr() -> String {
    std::env::var("WXLIKE_MEDIA_BIND").unwrap_or_else(|_| "0.0.0.0:0".into())
}

/// 本机主网卡 IPv4（connect 一个 TEST-NET 地址拿到路由选中的源地址，
/// 不会真的发包）。
fn primary_ipv4() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(_) => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    app: AppHandle,
    cfg: RelayCfg,
    mut host: CallHost,
    mut transport: Transport,
    rx: Receiver<Cmd>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<EngineStatus>>,
    first: Vec<Effect>,
) {
    let mut last_poll = Instant::now() - Duration::from_secs(1);
    let conn = Connection::new(&cfg);
    let mut audio_frames = 0u64;
    #[cfg(feature = "audio")]
    let mut audio = AudioLeg::start();

    let publish = |st: &Arc<Mutex<EngineStatus>>, app: &AppHandle| {
        let snapshot = st.lock().map(|s| s.clone()).unwrap_or_default();
        let _ = app.emit("call://rtc", &snapshot);
    };

    let mut pending: Vec<Effect> = first;
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        // 1. UI 命令
        while let Ok(cmd) = rx.try_recv() {
            let effects = match cmd {
                Cmd::Accept => {
                    let id = status.lock().map(|s| s.call_id.clone()).unwrap_or_default();
                    host.accept(&id, None)
                }
                Cmd::Hangup | Cmd::Stop => {
                    let ids = host.manager().live_calls();
                    let mut all = Vec::new();
                    for id in ids {
                        all.extend(host.hangup(&id));
                    }
                    all
                }
            };
            pending.extend(effects);
        }

        // 2. 麦克风 → 编码 → 媒体（先喂再泵，编码帧能赶在同一轮发出去）
        #[cfg(feature = "audio")]
        {
            // mesh 下每人一条会话，麦克风必须喂给每一条——只喂第一条的话，
            // 多人通话里只有一个人能听到你。
            let ids = host.manager().live_calls();
            for id in ids {
                audio.pump_mic(&mut host, &id);
            }
        }

        // 3. 推进时钟 + 媒体
        pending.extend(host.tick(Instant::now()));

        // 4. 收包（直连 socket 或 TURN 中继）
        pending.extend(transport.poll(&mut host));

        // 5. 信令：每 300ms 拉一次
        if last_poll.elapsed() >= Duration::from_millis(300) {
            last_poll = Instant::now();
            match conn.take(host.conv()) {
                Ok(signals) => {
                    for sig in signals {
                        pending.extend(host.handle_signal(sig));
                    }
                }
                Err(err) => eprintln!("[call] 拉取信令失败: {err}"),
            }
        }

        // 6. 落地所有副作用
        let mut ended = false;
        for effect in pending.drain(..) {
            match effect {
                Effect::Signal(sig) => {
                    if let Err(err) = conn.post(host.conv(), &sig) {
                        eprintln!("[call] 发送信令失败: {err}");
                    }
                }
                Effect::Transmit { to, bytes } => transport.send(to, &bytes),
                Effect::Audio { payload, .. } => {
                    audio_frames += 1;
                    #[cfg(feature = "audio")]
                    audio.play(&payload);
                    #[cfg(not(feature = "audio"))]
                    let _ = &payload;
                }
                Effect::Connected { call_id } => {
                    if let Ok(mut s) = status.lock() {
                        s.connected = true;
                        s.phase = "connected".into();
                        s.call_id = call_id;
                    }
                    publish(&status, &app);
                }
                Effect::Ended { call_id, .. } => {
                    ended = true;
                    if let Ok(mut s) = status.lock() {
                        s.connected = false;
                        s.phase = "ended".into();
                        s.call_id = call_id;
                    }
                    publish(&status, &app);
                }
            }
        }

        // 有新来电时把 call_id / ringing 同步给 UI
        #[cfg(feature = "audio")]
        let mic_frames = audio.sent;
        #[cfg(not(feature = "audio"))]
        let mic_frames = 0u64;
        sync_status(&host, &status, &app, audio_frames, mic_frames);

        if ended {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    if let Ok(mut s) = status.lock() {
        s.active = false;
        s.connected = false;
        s.phase = "ended".into();
    }
    let snapshot = status.lock().map(|s| s.clone()).unwrap_or_default();
    let _ = app.emit("call://rtc", &snapshot);
}

/// 把 host 里"有没有响铃中的来电"同步进状态，变化时才发事件。
fn sync_status(
    host: &CallHost,
    status: &Arc<Mutex<EngineStatus>>,
    app: &AppHandle,
    audio_frames: u64,
    mic_frames: u64,
) {
    let live = host.manager().live_calls();
    let mut changed = false;
    if let Ok(mut s) = status.lock() {
        let ringing = live.iter().any(|id| host.manager().phase_of(id).is_some());
        let call_id = live.first().cloned().unwrap_or_default();
        if s.ringing != ringing || (s.call_id.is_empty() && !call_id.is_empty()) {
            s.ringing = ringing;
            if !call_id.is_empty() {
                s.call_id = call_id;
            }
            changed = true;
        }
        let connected_peers = host.connected_count();
        if s.audio_frames != audio_frames
            || s.mic_frames != mic_frames
            || s.connected_peers != connected_peers
        {
            s.audio_frames = audio_frames;
            s.mic_frames = mic_frames;
            s.connected_peers = connected_peers;
            changed = true;
        }
    }
    if changed {
        let snapshot = status.lock().map(|s| s.clone()).unwrap_or_default();
        let _ = app.emit("call://rtc", &snapshot);
    }
}

/// 中继的最小客户端：POST 一条信令、GET 一批信令。
struct Connection {
    base: String,
    token: String,
}

impl Connection {
    fn new(cfg: &RelayCfg) -> Self {
        Self {
            base: cfg.base.trim_end_matches('/').to_string(),
            token: cfg.token.clone(),
        }
    }

    fn post(&self, conv: &str, sig: &Signal) -> Result<(), String> {
        let url = format!("{}/api/call/{conv}/signal", self.base);
        let body = serde_json::to_string(sig).map_err(|e| e.to_string())?;
        // ureq 3：`set` → `header`，`send_string` → `send`。
        // 4xx/5xx 不再是错误，所以 409（对方已离开房间，信令没人收）要走
        // Ok 分支自己判。
        let resp = ureq::post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .send(body.as_str());
        match resp {
            Ok(r) if r.status() == http::StatusCode::CONFLICT => Ok(()),
            Ok(r) if r.status().is_success() => Ok(()),
            Ok(r) => Err(format!("signal post: HTTP {}", r.status().as_u16())),
            Err(err) => Err(err.to_string()),
        }
    }

    fn take(&self, conv: &str) -> Result<Vec<Signal>, String> {
        let url = format!("{}/api/call/{conv}/signal", self.base);
        // 204 = 没有待收信令，是正常答案不是错误
        let mut resp = ureq::get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .call()
            .map_err(|e| e.to_string())?;
        if resp.status() == http::StatusCode::NO_CONTENT {
            return Ok(Vec::new());
        }
        resp.body_mut()
            .read_json::<Vec<Signal>>()
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(uris: &[&str]) -> TurnCredentials {
        TurnCredentials {
            username: "1700003600:u1".into(),
            password: "c2VjcmV0".into(),
            uris: uris.iter().map(|u| (*u).to_string()).collect(),
            expires_at: Instant::now() + Duration::from_secs(3_600),
        }
    }

    #[test]
    fn refresh_window_is_five_minutes() {
        let now = Instant::now();
        assert!(needs_refresh(now + Duration::from_secs(299), now));
        assert!(!needs_refresh(now + Duration::from_secs(301), now));
    }

    #[test]
    fn missing_turn_secret_degrades_to_direct() {
        assert!(parse_turn_credentials(404, &serde_json::Value::Null)
            .unwrap()
            .is_none());
        let configured_without_uri = parse_turn_credentials(
            200,
            &serde_json::json!({
                "username": "1:u", "password": "p", "ttl_seconds": 3600, "uris": []
            }),
        )
        .unwrap()
        .unwrap();
        assert!(turn_config_from(&configured_without_uri).is_none());
    }

    #[test]
    fn parses_coturn_rest_credentials() {
        let body = serde_json::json!({
            "username": "1700003600:u1",
            "password": "c2VjcmV0",
            "ttl_seconds": 3600,
            "uris": ["turn:relay.example.com:3478"]
        });
        let parsed = parse_turn_credentials(200, &body).unwrap().unwrap();
        assert_eq!(parsed.username, "1700003600:u1");
        assert_eq!(parsed.password, "c2VjcmV0");
        let cfg = turn_config_from(&parsed).unwrap();
        assert_eq!(cfg.host, "relay.example.com");
        assert_eq!(cfg.port, 3478);
        assert_eq!(cfg.user, "1700003600:u1");
    }

    #[test]
    fn only_udp_turn_uris_are_used() {
        let cfg = turn_config_from(&creds(&[
            "turns:relay.example.com:5349",
            "turn:relay.example.com:3478?transport=tcp",
            "turn:relay.example.com:3478?transport=udp",
        ]))
        .unwrap();
        assert_eq!(cfg.host, "relay.example.com");
        assert_eq!(cfg.port, 3478);
    }

    #[test]
    fn rejects_malformed_or_failed_responses() {
        assert!(parse_turn_credentials(500, &serde_json::Value::Null).is_err());
        assert!(
            parse_turn_credentials(200, &serde_json::json!({"username": "", "password": ""}))
                .is_err()
        );
    }
}
