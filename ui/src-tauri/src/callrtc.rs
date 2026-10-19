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
//! 线程里的循环每轮做四件事：
//! 1. 收 UI 命令（接听 / 挂断）；
//! 2. `host.tick` 推进状态机与媒体，把 `Transmit` 真发出去；
//! 3. 非阻塞收 UDP 包喂回 `host`；
//! 4. 每 300ms 拉一次信令中继，同时把要发的信令 POST 上去。

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use wxlike_rtc::host::{CallHost, Effect};
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
}

/// 引擎句柄，挂在 Tauri 的 managed state 里。
#[derive(Default)]
pub struct CallEngine {
    stop: Arc<AtomicBool>,
    tx: Mutex<Option<Sender<Cmd>>>,
    status: Arc<Mutex<EngineStatus>>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// 连服务端中继所需的最小上下文。
pub struct RelayCfg {
    pub base: String,
    pub token: String,
}

impl CallEngine {
    pub fn status(&self) -> EngineStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// 发起通话：开线程，返回本地 call_id。
    pub fn start(
        &self,
        app: AppHandle,
        cfg: RelayCfg,
        me: String,
        conv: String,
        peer: String,
        video: bool,
    ) -> Result<String, String> {
        self.shutdown();

        let socket = UdpSocket::bind(bind_addr()).map_err(|e| format!("绑定媒体端口失败: {e}"))?;
        socket
            .set_nonblocking(true)
            .map_err(|e| format!("设置非阻塞失败: {e}"))?;
        let local = socket.local_addr().map_err(|e| e.to_string())?;
        // 绑 0.0.0.0 时不能把这个地址写进 SDP（对端不可达），换成本机网卡地址
        let advertise = if local.ip().is_unspecified() {
            SocketAddr::new(
                IpAddr::V4(primary_ipv4().unwrap_or(Ipv4Addr::LOCALHOST)),
                local.port(),
            )
        } else {
            local
        };

        let mut host = CallHost::new(me, conv.clone(), advertise);
        let (call_id, effects) = host.start(&peer, video, true);

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
        };

        let stop = self.stop.clone();
        let status = self.status.clone();
        let handle = std::thread::Builder::new()
            .name("wxlike-call".into())
            .spawn(move || {
                run(app, cfg, host, socket, advertise, rx, stop, status, effects);
            })
            .map_err(|e| format!("启动通话线程失败: {e}"))?;
        *self.join.lock().unwrap() = Some(handle);
        Ok(call_id)
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
    socket: UdpSocket,
    advertise: SocketAddr,
    rx: Receiver<Cmd>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<EngineStatus>>,
    first: Vec<Effect>,
) {
    let mut buf = vec![0u8; 2048];
    let mut last_poll = Instant::now() - Duration::from_secs(1);
    let conn = Connection::new(&cfg);
    let mut audio_frames = 0u64;

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

        // 2. 推进时钟 + 媒体
        pending.extend(host.tick(Instant::now()));

        // 3. 收 UDP
        loop {
            match socket.recv_from(&mut buf) {
                Ok((n, from)) => {
                    pending.extend(host.handle_datagram(from, advertise, &buf[..n]));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        // 4. 信令：每 300ms 拉一次
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

        // 5. 落地所有副作用
        let mut ended = false;
        for effect in pending.drain(..) {
            match effect {
                Effect::Signal(sig) => {
                    if let Err(err) = conn.post(host.conv(), &sig) {
                        eprintln!("[call] 发送信令失败: {err}");
                    }
                }
                Effect::Transmit { to, bytes } => {
                    let _ = socket.send_to(&bytes, to);
                }
                Effect::Audio { .. } => {
                    audio_frames += 1;
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
        sync_status(&host, &status, &app, audio_frames);

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
        if s.audio_frames != audio_frames {
            s.audio_frames = audio_frames;
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
        match ureq::post(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Content-Type", "application/json")
            .send_string(&body)
        {
            Ok(_) => Ok(()),
            // 409 = 对方已离开房间，信令没人收，不算错误
            Err(ureq::Error::Status(409, _)) => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }

    fn take(&self, conv: &str) -> Result<Vec<Signal>, String> {
        let url = format!("{}/api/call/{conv}/signal", self.base);
        let resp = ureq::get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .call();
        match resp {
            Ok(r) => r.into_json::<Vec<Signal>>().map_err(|e| e.to_string()),
            Err(ureq::Error::Status(204, _)) => Ok(Vec::new()),
            Err(err) => Err(err.to_string()),
        }
    }
}
