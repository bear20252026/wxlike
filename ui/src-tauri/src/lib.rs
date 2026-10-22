//! wxlike UI backend (WP-11): Tauri commands bridging the webview to the
//! wxlike server.
//!
//! Architecture rule (task book §0.1.1): all business logic lives in Rust.
//! The frontend only renders and calls `invoke(...)` — it never opens an
//! HTTP connection or a QUIC socket itself. Every server interaction is a
//! command in this crate:
//!
//! * platform data plane (accounts, friends, groups, wallet, media) over
//!   HTTP ([`http`]);
//! * message transport over the QUIC hub ([`wxlike_net::Client`]), with
//!   WP-12 TOFU certificate pinning — first connect pins the hub cert
//!   fingerprint under the app data dir, a changed cert is refused.
//!
//! Command handlers are thin wrappers; the real logic lives in inherent
//! methods on [`AppState`], which the integration tests drive against a
//! **real** in-process server (platform + hub) — mocks are not allowed at
//! this boundary.

// the MSVC linker prints localized progress lines on non-English Windows;
// rustc surfaces that as a lint which is pure noise here
#![allow(linker_messages)]

use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use tauri::Manager as _;
use wxlike_core::custom_kind;
use wxlike_core::e2ee::{Crypto, DEVICE_KEY_BYTES};
use wxlike_core::{Envelope, MsgBody, MsgId};
use wxlike_net::Client;

pub mod callrtc;

pub use wxlike_client::{direct_conv_id, Http};

/// 把 `host:port` / `host` 拆开。IPv6 的 `[::1]:80` 也认。
fn split_host_port(s: &str, default_port: u16) -> (String, u16) {
    if let Some(rest) = s.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail
                .strip_prefix(':')
                .and_then(|p| p.parse().ok())
                .unwrap_or(default_port);
            return (host.to_string(), port);
        }
    }
    match s.rsplit_once(':') {
        Some((host, port)) => match port.parse::<u16>() {
            Ok(p) => (host.to_string(), p),
            // 冒号后面不是端口（IPv6 没括号），整体当 host
            Err(_) => (s.to_string(), default_port),
        },
        None => (s.to_string(), default_port),
    }
}

/// 词法归一化路径：消掉 `.`、折叠 `..`，不做符号链接解析。
///
/// 这里防的是"webview 传 `../../x`"这类**字符串级**越界；符号链接穿越不在
/// 威胁模型内（能改应用数据目录的符号链接的人已经拿到了同等权限）。
fn normalize_path(p: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Runs `fut` to completion from sync command code. Commands execute on
/// tauri's blocking pool (no ambient runtime) so plain `block_on` is fine
/// there; tests call the same methods from inside a tokio runtime, where
/// `block_in_place` keeps the worker alive.
fn wait<F: std::future::Future>(fut: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| tauri::async_runtime::block_on(fut))
    } else {
        tauri::async_runtime::block_on(fut)
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// 32-char hex → 16-byte msg id (no panics on odd input).
fn decode_msg_id(s: &str) -> Option<MsgId> {
    let bytes = hex::decode(s).ok()?;
    <[u8; 16]>::try_from(bytes).ok()
}

/// One window's whole backend state. Managed by Tauri; tests drive it via `Arc`.
pub struct AppState {
    http: Mutex<Http>,
    /// `Arc` 是为了让后台泵能跨线程持有它（重连会替换里面的 Client，
    /// 泵每轮重新读一次，所以必须共享同一个槽而不是拷贝一份）。
    client: Arc<Mutex<Option<Client>>>,
    hub_addr: Mutex<Option<String>>,
    seq: AtomicU64,
    /// P1 secret chat: the local Olm account, held for the app session
    /// (key persistence lands with P4's full secret chats). Shared with the
    /// receive pump so inbound sealed payloads decrypt before emit.
    secret: Arc<Mutex<Option<SecretStore>>>,
    /// 数据目录（本地库所在目录）：秘密聊天的密钥与快照也放这里。
    data_dir: Mutex<Option<std::path::PathBuf>>,
    /// 秘密会话开关的本地缓存。**服务端是唯一真相**（`secret_settings`），
    /// 这里只是发送路径的快速判断：每发一条都去 GET 一次不现实，而且网络
    /// 抖动不该让"这条能不能发"变得不可知。
    secret_convs: Mutex<std::collections::HashMap<String, bool>>,
    /// P2 local message library (client-side SQLite): the chat opens from
    /// disk instantly, the server history only *adds* older rows.
    local: Arc<Mutex<Option<LocalStore>>>,
    /// 发送队列：`send()` 失败的消息落盘，连上后自动重发。
    /// 进程被杀也不丢——见 `wxlike_core::outbox`。
    outbox: Mutex<Option<Arc<wxlike_core::outbox::Outbox>>>,
    /// 重发泵只起一次：每次 `connect_hub` 都 spawn 一个的话，
    /// 重连三次就有三个线程同时抢同一条任务（靠数据库状态不会重发两次，
    /// 但白烧 CPU）。
    outbox_pump: std::sync::atomic::AtomicBool,
    /// 本地防雪崩：给**自动重发**用的频率/流量漏斗。
    /// 用户手点发送不走它（`set_active` 的语义见 `crates/net/src/avalanche.rs`）。
    avalanche: Arc<wxlike_net::avalanche::Avalanche>,
}

impl AppState {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: Mutex::new(Http::new(base_url)),
            client: Arc::new(Mutex::new(None)),
            hub_addr: Mutex::new(None),
            seq: AtomicU64::new(1),
            secret: Arc::new(Mutex::new(None)),
            data_dir: Mutex::new(None),
            secret_convs: Mutex::new(std::collections::HashMap::new()),
            local: Arc::new(Mutex::new(None)),
            outbox: Mutex::new(None),
            outbox_pump: std::sync::atomic::AtomicBool::new(false),
            avalanche: Arc::new(wxlike_net::avalanche::Avalanche::new(
                wxlike_net::avalanche::AvalancheConfig::messages(),
            )),
        }
    }

    fn http(&self) -> MutexGuard<'_, Http> {
        self.http.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    // ---- server / auth ---------------------------------------------------

    pub fn set_server(&self, base_url: &str) {
        let token = self.http().token.clone();
        *self.http() = Http::new(base_url);
        self.http().token = token;
    }

    pub fn register(&self, name: &str, password: &str) -> Result<serde_json::Value, String> {
        let reply = self.http().expect_json(
            "POST",
            "/api/register",
            Some(&serde_json::json!({"name": name, "password": password})),
        )?;
        self.http().token = Some(reply["token"].as_str().unwrap_or_default().to_string());
        Ok(reply)
    }

    pub fn login(&self, name: &str, password: &str) -> Result<serde_json::Value, String> {
        let reply = self.http().expect_json(
            "POST",
            "/api/login",
            Some(&serde_json::json!({"name": name, "password": password})),
        )?;
        self.http().token = Some(reply["token"].as_str().unwrap_or_default().to_string());
        Ok(reply)
    }

    pub fn logout(&self) -> Result<(), String> {
        self.http().expect_json("POST", "/api/logout", None)?;
        self.http().token = None;
        Ok(())
    }

    pub fn me(&self) -> Result<serde_json::Value, String> {
        self.http().expect_json("GET", "/api/me", None)
    }

    pub fn update_profile(
        &self,
        name: Option<&str>,
        avatar: Option<&str>,
        signature: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        self.http().expect_json(
            "PUT",
            "/api/profile",
            Some(&serde_json::json!({
                "name": name, "avatar": avatar, "signature": signature
            })),
        )
    }

    /// 当前登录凭据。`None` = 还没登录。
    ///
    /// 连 hub 与网络诊断都要用它：hub 只从凭据解析身份，不接受客户端自报
    /// `user_id`（issue #3）。测试与前端也不该直接拿整个 `Http`。
    pub fn auth_token(&self) -> Option<String> {
        self.http().token.clone()
    }

    /// 恢复一次登录（重启后重新载入保存的凭据；命令层不做持久化）。
    pub fn set_auth_token(&self, token: &str) {
        self.http().token = Some(token.to_string());
    }

    pub fn user_id(&self) -> Result<String, String> {
        Ok(self.me()?["user_id"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    // ---- friends ----------------------------------------------------------

    pub fn friends(&self) -> Result<serde_json::Value, String> {
        self.http().expect_json("GET", "/api/friends", None)
    }

    pub fn friend_request(&self, to_user_id: &str, message: &str) -> Result<(), String> {
        self.http().expect_json(
            "POST",
            "/api/friends/requests",
            Some(&serde_json::json!({"to_user_id": to_user_id, "message": message})),
        )?;
        Ok(())
    }

    pub fn friend_requests_incoming(&self) -> Result<serde_json::Value, String> {
        self.http()
            .expect_json("GET", "/api/friends/requests/incoming", None)
    }

    pub fn friend_requests_outgoing(&self) -> Result<serde_json::Value, String> {
        self.http()
            .expect_json("GET", "/api/friends/requests/outgoing", None)
    }

    pub fn friend_accept(&self, req_id: &str) -> Result<(), String> {
        self.http().expect_json(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
        )?;
        Ok(())
    }

    pub fn friend_decline(&self, req_id: &str) -> Result<(), String> {
        self.http().expect_json(
            "POST",
            &format!("/api/friends/requests/{req_id}/decline"),
            None,
        )?;
        Ok(())
    }

    pub fn friend_delete(&self, user_id: &str) -> Result<(), String> {
        self.http()
            .expect_json("DELETE", &format!("/api/friends/{user_id}"), None)?;
        Ok(())
    }

    // ---- groups -----------------------------------------------------------

    pub fn groups_create(&self, name: &str, member_ids: &[String]) -> Result<String, String> {
        let reply = self.http().expect_json(
            "POST",
            "/api/groups",
            Some(&serde_json::json!({"name": name, "member_ids": member_ids})),
        )?;
        Ok(reply["group_id"].as_str().unwrap_or_default().to_string())
    }

    pub fn groups_mine(&self) -> Result<serde_json::Value, String> {
        self.http().expect_json("GET", "/api/groups/mine", None)
    }

    pub fn group_info(&self, group_id: &str) -> Result<serde_json::Value, String> {
        self.http()
            .expect_json("GET", &format!("/api/groups/{group_id}"), None)
    }

    // ---- wallet ------------------------------------------------------------

    pub fn wallet_balance(&self) -> Result<i64, String> {
        let v = self
            .http()
            .expect_json("GET", "/api/wallet/balance", None)?;
        Ok(v["balance"].as_i64().unwrap_or(0))
    }

    pub fn wallet_transfer(&self, to_uid: &str, amount: i64, memo: &str) -> Result<String, String> {
        let reply = self.http().expect_json(
            "POST",
            "/api/wallet/transfer",
            Some(&serde_json::json!({"to_uid": to_uid, "amount": amount, "memo": memo})),
        )?;
        Ok(reply["transfer_id"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    pub fn wallet_redpacket(&self, amount: i64, count: i64, memo: &str) -> Result<String, String> {
        let reply = self.http().expect_json(
            "POST",
            "/api/wallet/redpacket",
            Some(&serde_json::json!({"amount": amount, "count": count, "memo": memo})),
        )?;
        Ok(reply["redpacket_id"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    pub fn wallet_claim(&self, redpacket_id: &str) -> Result<i64, String> {
        let v = self.http().expect_json(
            "POST",
            &format!("/api/wallet/redpacket/{redpacket_id}/claim"),
            None,
        )?;
        Ok(v["amount"].as_i64().unwrap_or(0))
    }

    // ---- media --------------------------------------------------------------

    pub fn upload_media(
        &self,
        bytes: Vec<u8>,
        content_type: &str,
        sha256: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        self.http().upload(&bytes, content_type, sha256)
    }

    pub fn download_media(&self, media_id: &str, thumb: bool) -> Result<MediaBlob, String> {
        let (bytes, content_type) = self.http().download(media_id, thumb)?;
        Ok(MediaBlob {
            base64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes),
            content_type,
        })
    }

    // ---- hub / messages -------------------------------------------------------

    /// Connects to the QUIC hub with TOFU pinning. `pins_path` is where the
    /// hub certificate fingerprint is remembered between runs.
    /// `token` 是**凭据**，不是 user_id：hub 从它解析身份并回 `HelloAck`
    /// （issue #3）。`Client::user_id` 是服务端给的，不是我们传的。
    pub fn connect_hub(
        &self,
        addr: &str,
        token: &str,
        pins_path: &std::path::Path,
    ) -> Result<String, String> {
        let sock: std::net::SocketAddr = addr.parse().map_err(|e| format!("bad addr: {e}"))?;
        let client = wait(Client::connect_verified(
            sock,
            "localhost",
            token,
            pins_path,
        ))
        .map_err(|e| format!("connect: {e:#}"))?;
        *self.client.lock().unwrap_or_else(PoisonError::into_inner) = Some(client);
        *self.hub_addr.lock().unwrap_or_else(PoisonError::into_inner) = Some(addr.to_string());
        Ok(format!("quic {sock} (tofu pinned)"))
    }

    pub fn is_connected(&self) -> bool {
        self.client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    fn client(&self) -> Result<Client, String> {
        self.client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| "未连接到服务器".to_string())
    }

    fn send_body(&self, conv_id: &str, to: Vec<String>, body: MsgBody) -> Result<Envelope, String> {
        self.send_body_with_echo(conv_id, to, body, None)
    }

    /// `echo` 是**发送方自己已经知道的明文**（只有秘密聊天需要）：
    /// 自己的密文不能用接收方的路径去解——对端是自己，没有会话，必然失败。
    fn send_body_with_echo(
        &self,
        conv_id: &str,
        to: Vec<String>,
        body: MsgBody,
        echo: Option<&str>,
    ) -> Result<Envelope, String> {
        // 秘密会话只收密文：明文（文本/图片/转发卡片）进秘密会话，等于把
        // 内容原样交给服务器，UI 上却挂着锁——这是"承诺没兑现"（issue #15）。
        // 控制帧（typing）放行：它本来就不落库、不带内容。
        let allowed_in_secret = matches!(&body, MsgBody::Custom { kind, .. }
            if kind == custom_kind::E2EE_DM || kind == custom_kind::TYPING);
        if !allowed_in_secret && self.conv_is_secret(conv_id) {
            return Err("这是秘密聊天会话：只能发送加密消息，明文已取消发送".into());
        }
        let client = self.client()?;
        let me = self.user_id()?;
        let env = Envelope::with_body(conv_id, me, to, self.next_seq(), body);
        match wait(client.send(&env)) {
            Ok(()) => {
                self.append_own_echo(&env, echo);
                Ok(env)
            }
            // 连接问题不该变成"用户手动重发"：落盘排队，后台泵连上就发
            Err(err) => self.queue_for_retry(&env, &err, echo),
        }
    }

    /// 自己的消息落本地库（秘密聊天时用发送方已知的明文，不去解密自己的密文）。
    fn append_own_echo(&self, env: &Envelope, echo: Option<&str>) {
        if env.is_recall() || env.is_typing() {
            return;
        }
        self.local_append(&render_event_with(env, &self.secret, echo));
    }

    /// 发送失败 → 写进 outbox，等后台泵重发。
    ///
    /// `task_id` 用 `msg_id`：重试时把**同一条 envelope** 原样重发，服务端按
    /// `msg_id` 去重，所以重试不会变成两条消息。这也是 `enqueue_with_id`
    /// 存在的原因——同一个 id 重复入队是空操作。
    fn queue_for_retry(
        &self,
        env: &Envelope,
        err: &anyhow::Error,
        echo: Option<&str>,
    ) -> Result<Envelope, String> {
        let Some(outbox) = self.outbox() else {
            // 没打开本地库 = 无处落盘，只能如实报错（总比假装成功好）
            return Err(format!("send: {err:#}"));
        };
        // 秘密消息把明文一起存：重发成功后本地回显才能显示正文，而不是
        // "无法解密"。普通消息保持裸 envelope，老队列项照样能读。
        let payload = match echo {
            Some(text) => serde_json::json!({ "env": env, "echo": text }),
            None => serde_json::to_value(env).map_err(|e| format!("outbox encode: {e}"))?,
        };
        outbox
            .enqueue_with_id(
                &hex(&env.msg_id),
                &env.conv_id,
                "envelope",
                &payload,
                wxlike_core::types::now_ms() as i64,
            )
            .map_err(|e| format!("outbox: {e:#}"))?;
        Ok(env.clone())
    }

    /// 后台重发泵：每秒看一次队列。
    ///
    /// 用**独立 OS 线程**而不是 async 任务：`wait()` 在 async 上下文里要走
    /// `block_in_place`，而这里本来就是阻塞式的发送调用，放线程里更直白，
    /// 也不会和收包泵抢同一个 runtime 的工作线程。
    pub fn start_outbox_pump(&self, app: tauri::AppHandle) {
        let Some(outbox) = self.outbox() else {
            return;
        };
        if self
            .outbox_pump
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return; // 已经在跑了
        }
        let client_slot = self.client.clone();
        let local = self.local.clone();
        let secret = self.secret.clone();
        let avalanche = self.avalanche.clone();
        std::thread::spawn(move || {
            use tauri::Emitter as _;
            let mut next_sample = std::time::Instant::now();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let client = client_slot
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                let Some(client) = client else {
                    continue;
                };
                // 自适应心跳：按策略给的时间点采一次 RTT/存活状态。
                // 收敛结果在**下一次连接**时生效——QUIC 的 TransportConfig 是
                // 连接级不可变的，改不了已建连接的保活间隔。
                if std::time::Instant::now() >= next_sample {
                    let interval = client.sample_heartbeat();
                    next_sample = std::time::Instant::now() + interval;
                }
                let now = wxlike_core::types::now_ms() as i64;
                let report = outbox.flush_with(now, 20, |task| {
                    let (env, echo) = decode_outbox_task(&task.payload)?;
                    // 本地防雪崩：弱网下重发风暴会把流量打光、把连接堵死。
                    // 键用 conv_id——同一条命令不同会话互不牵连。
                    let bytes = task.payload.to_string().len();
                    if let wxlike_net::avalanche::Verdict::Deny {
                        reason,
                        retry_after_ms,
                    } = avalanche.check(&task.conv_id, bytes, now)
                    {
                        return Err(wxlike_core::outbox::SendFailure::throttled(format!(
                            "本地限流（{}），{}ms 后再试",
                            reason.as_str(),
                            retry_after_ms
                        )));
                    }
                    match wait(client.send(&env)) {
                        Ok(()) => {
                            if !env.is_recall() && !env.is_typing() {
                                let payload = render_event_with(&env, &secret, echo.as_deref());
                                let guard = local.lock().unwrap_or_else(PoisonError::into_inner);
                                if let Some(store) = guard.as_ref() {
                                    let _ = store.append(&payload);
                                }
                            }
                            Ok(())
                        }
                        Err(e) => Err(wxlike_core::outbox::SendFailure::retryable(format!(
                            "{e:#}"
                        ))),
                    }
                });
                if let Ok(report) = report {
                    if report.sent > 0 || report.failed > 0 || report.throttled > 0 {
                        let pending = outbox.pending_count().unwrap_or(0);
                        let _ = app.emit(
                            "im-outbox",
                            serde_json::json!({
                                "sent": report.sent,
                                "retrying": report.retrying,
                                "failed": report.failed,
                                "throttled": report.throttled,
                                "pending": pending,
                            }),
                        );
                    }
                }
            }
        });
    }

    pub fn send_text(&self, conv_id: &str, to: Vec<String>, text: &str) -> Result<(), String> {
        self.send_body(conv_id, to, MsgBody::Text(text.to_string()))?;
        Ok(())
    }

    /// Image message: `MsgBody::Bytes(media_id)` — the reference, never the
    /// bytes (WP-05); the peer downloads by id.
    pub fn send_image(&self, conv_id: &str, to: Vec<String>, media_id: &str) -> Result<(), String> {
        self.send_body(conv_id, to, MsgBody::Bytes(media_id.as_bytes().to_vec()))?;
        Ok(())
    }

    pub fn send_redpacket(
        &self,
        conv_id: &str,
        to: Vec<String>,
        redpacket_id: &str,
        amount: i64,
        count: i64,
        memo: &str,
    ) -> Result<(), String> {
        self.send_body(
            conv_id,
            to,
            MsgBody::Custom {
                kind: "wallet.redpacket".into(),
                payload: serde_json::json!({
                    "redpacket_id": redpacket_id,
                    "amount": amount,
                    "count": count,
                    "memo": memo,
                }),
            },
        )?;
        Ok(())
    }

    // ---- P2: rich message types (voice / file / sticker / video) ------------

    /// Voice note: bars arrive precomputed by the webview (decoded from the
    /// recording), so the Rust side only validates and sends.
    pub fn send_voice(
        &self,
        conv_id: &str,
        to: Vec<String>,
        media_id: &str,
        duration_ms: u32,
        waveform: Vec<u8>,
    ) -> Result<(), String> {
        let payload = wxlike_core::msgmeta::VoicePayload {
            media_id: media_id.to_string(),
            duration_ms,
            waveform,
            asr: None, // 转文字占位: provider lands at P4
        };
        payload.validate()?;
        self.send_body(
            conv_id,
            to,
            MsgBody::Custom {
                kind: "msg.voice".into(),
                payload: serde_json::to_value(&payload).map_err(|e| e.to_string())?,
            },
        )?;
        Ok(())
    }

    pub fn send_file(
        &self,
        conv_id: &str,
        to: Vec<String>,
        media_id: &str,
        name: &str,
        size: u64,
        sha256: &str,
    ) -> Result<(), String> {
        let payload = wxlike_core::msgmeta::FilePayload {
            media_id: media_id.to_string(),
            name: name.to_string(),
            size,
            sha256: sha256.to_string(),
        };
        payload.validate()?;
        self.send_body(
            conv_id,
            to,
            MsgBody::Custom {
                kind: "msg.file".into(),
                payload: serde_json::to_value(&payload).map_err(|e| e.to_string())?,
            },
        )?;
        Ok(())
    }

    pub fn send_sticker(
        &self,
        conv_id: &str,
        to: Vec<String>,
        media_id: &str,
        pack: &str,
        id: &str,
    ) -> Result<(), String> {
        let payload = wxlike_core::msgmeta::StickerPayload {
            media_id: media_id.to_string(),
            pack: pack.to_string(),
            id: id.to_string(),
        };
        payload.validate()?;
        self.send_body(
            conv_id,
            to,
            MsgBody::Custom {
                kind: "msg.sticker".into(),
                payload: serde_json::to_value(&payload).map_err(|e| e.to_string())?,
            },
        )?;
        Ok(())
    }

    /// Uploads bytes to video-service (P2 minimal: single-res HLS + thumb +
    /// progressive MP4) and returns its ingest reply verbatim.
    pub fn upload_video(&self, bytes: Vec<u8>) -> Result<serde_json::Value, String> {
        let boundary = "wxlike-ui-video-9a2f1";
        let mut body = Vec::with_capacity(bytes.len() + 256);
        let _ = write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"bytes\"; filename=\"video\"\r\nContent-Type: video/mp4\r\n\r\n"
        );
        body.extend_from_slice(&bytes);
        let _ = write!(body, "\r\n--{boundary}--\r\n");

        let resp = self
            .http()
            .req("POST", "/api/video")
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .set("Content-Length", &body.len().to_string())
            .send_bytes(&body)
            .map_err(|e| match e {
                ureq::Error::Status(status, r) => {
                    let msg = r.into_string().unwrap_or_else(|_| format!("HTTP {status}"));
                    msg
                }
                other => format!("upload video: {other}"),
            })?;
        resp.into_json::<serde_json::Value>()
            .map_err(|e| format!("upload video decode: {e}"))
    }

    /// Downloads a video-service artifact (`/api/video/{id}/play.mp4` or
    /// `thumb.png`) for the webview. The path allowlist mirrors the server's.
    pub fn download_video(&self, path: &str) -> Result<MediaBlob, String> {
        let ok = path.starts_with("/api/video/")
            && (path.ends_with("/play.mp4") || path.ends_with("/thumb.png"))
            && !path.contains("..");
        if !ok {
            return Err("video path outside /api/video".into());
        }
        let resp = self.http().req("GET", path).call().map_err(|e| match e {
            ureq::Error::Status(404, _) => "video not found".to_string(),
            other => format!("download video: {other}"),
        })?;
        let content_type = resp.content_type().to_string();
        let mut bytes = Vec::new();
        resp.into_reader()
            .read_to_end(&mut bytes)
            .map_err(|e| format!("video read: {e}"))?;
        Ok(MediaBlob {
            base64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes),
            content_type,
        })
    }

    pub fn send_video(
        &self,
        conv_id: &str,
        to: Vec<String>,
        payload: &serde_json::Value,
    ) -> Result<(), String> {
        let parsed: wxlike_core::msgmeta::VideoPayload = serde_json::from_value(payload.clone())
            .map_err(|e| format!("bad video payload: {e}"))?;
        parsed.validate()?;
        self.send_body(
            conv_id,
            to,
            MsgBody::Custom {
                kind: "msg.video".into(),
                payload: payload.clone(),
            },
        )?;
        Ok(())
    }

    /// Avatar (P2: 头像接媒体): upload the bytes as media, then point the
    /// profile at the media id. The id is what other clients fetch and render.
    pub fn upload_avatar(&self, bytes: Vec<u8>, content_type: &str) -> Result<String, String> {
        let up = self.upload_media(bytes, content_type, None)?;
        let media_id = up["media_id"]
            .as_str()
            .ok_or("upload returned no media_id")?
            .to_string();
        self.update_profile(None, Some(&media_id), None)?;
        Ok(media_id)
    }

    // ---- P2: local message library ------------------------------------------

    /// Opens (or creates) the client-side SQLite message store. Called once
    /// at app start with the app-data path; tests pass a temp path.
    pub fn local_open(&self, path: &std::path::Path) -> Result<(), String> {
        let store = LocalStore::open(path).map_err(|e| format!("local db: {e:#}"))?;
        // 发送队列与本地消息库**同一个文件**、各自一条连接（WAL + busy_timeout）。
        // 放同一文件是为了"一条消息要么在库里、要么在队列里"，备份/迁移不用管两处。
        let outbox = wxlike_core::outbox::Outbox::open(Some(&path.to_string_lossy()))
            .map_err(|e| format!("outbox: {e:#}"))?;
        // 自适应心跳：按网络指纹存收敛结果，放在库文件同目录。
        // 桌面端拿不到"WiFi 还是 4G"，所以用 `WXLIKE_NET_KIND` 显式指定，
        // 不指定就是 unknown（按最保守的 15 秒起步，收敛后自动变长）。
        let hb_dir = path.parent().map(std::path::Path::to_path_buf);
        let _ = wxlike_net::heartbeat::Heartbeat::init(
            wxlike_net::heartbeat::NetProfile::from_env(),
            wxlike_net::heartbeat::store_in(hb_dir.as_deref()),
        );
        *self.outbox.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(outbox));
        *self.local.lock().unwrap_or_else(PoisonError::into_inner) = Some(store);
        if let Some(dir) = path.parent() {
            *self.data_dir.lock().unwrap_or_else(PoisonError::into_inner) = Some(dir.to_path_buf());
        }
        Ok(())
    }

    /// 数据目录。秘密聊天**必须**有它才能持久化密钥，所以没打开本地库时
    /// 明确报错，而不是退回进程内状态（那等于"重启换身份"）。
    fn data_dir(&self) -> Result<std::path::PathBuf, String> {
        self.data_dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| "本地库未打开：秘密聊天需要数据目录保存密钥".to_string())
    }

    /// 控制面地址（`http://host:port`），诊断用。
    fn http_base(&self) -> String {
        self.http
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .base
            .clone()
    }

    fn hub_addr(&self) -> Option<String> {
        self.hub_addr
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn outbox(&self) -> Option<Arc<wxlike_core::outbox::Outbox>> {
        self.outbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn local_ready(&self) -> bool {
        self.local
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// Newest-first local page for a conversation (`before_seq = 0` = latest),
    /// shaped exactly like the hub event payload so the frontend merges both
    /// with one code path.
    pub fn local_history(
        &self,
        conv_id: &str,
        before_seq: i64,
        limit: usize,
    ) -> Result<serde_json::Value, String> {
        let guard = self.local.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(store) = guard.as_ref() else {
            return Ok(serde_json::json!({"messages": [], "next_before_seq": null}));
        };
        let rows = store
            .page(conv_id, before_seq, limit)
            .map_err(|e| format!("local page: {e:#}"))?;
        // 消失消息：到期的那条不再出现在本地历史里（issue #15）。删除动作
        // 由本地库自己的清理负责，这里先保证"看不见"——界面上的承诺是
        // "到点消失"，不是"到点仍在但灰掉"。
        let ttl = self.conv_ttl_ms(conv_id);
        let now = wxlike_core::types::now_ms();
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|r| !expired(r.to_json().get("sent_at_ms"), ttl, now))
            .collect();
        let messages: Vec<serde_json::Value> = rows.iter().map(|r| r.to_json()).collect();
        let next = rows.last().map(|r| r.seq).filter(|_| rows.len() == limit);
        Ok(serde_json::json!({ "messages": messages, "next_before_seq": next }))
    }

    /// Persists one event payload (called by the pump for every inbound
    /// envelope and by the senders for their own echoes).
    pub fn local_append(&self, payload: &serde_json::Value) {
        let guard = self.local.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(store) = guard.as_ref() {
            if let Err(err) = store.append(payload) {
                eprintln!("[local-db] append failed: {err:#}");
            }
        }
    }

    // ---- P1: typing / forward / merge forward --------------------------------

    /// Transient "I'm typing" ping. The hub routes it online-only; it is
    /// never stored, so nothing is sent back on reconnect.
    pub fn send_typing(&self, conv_id: &str, to: Vec<String>) -> Result<(), String> {
        self.send_body(
            conv_id,
            to,
            MsgBody::Custom {
                kind: custom_kind::TYPING.into(),
                payload: serde_json::json!({ "conv_id": conv_id }),
            },
        )?;
        Ok(())
    }

    /// Forwards one message into another conversation as a structured
    /// snapshot (the original stays where it is; a recall does not follow).
    ///
    /// **秘密会话的内容禁止转发**：转发会造出第三份副本，而密钥只在两端，
    /// 消失消息也不追到副本上。来源会话从条目自带的 `conv_id` 判断。
    pub fn forward_message(
        &self,
        to_conv: &str,
        to: Vec<String>,
        item: &serde_json::Value,
    ) -> Result<(), String> {
        if let Some(from) = item.get("conv_id").and_then(|v| v.as_str()) {
            if self.conv_is_secret(from) {
                return Err("秘密聊天的消息不能转发".into());
            }
        }
        self.send_body(
            to_conv,
            to,
            MsgBody::Custom {
                kind: custom_kind::FORWARD.into(),
                payload: item.clone(),
            },
        )?;
        Ok(())
    }

    /// Recalls one of my own messages (hub verifies authorship; a forged
    /// recall is dropped server-side, WP-03 A3).
    pub fn recall_msg(&self, conv_id: &str, target_hex: &str) -> Result<(), String> {
        let target: MsgId = decode_msg_id(target_hex).ok_or("bad msg id")?;
        let me = self.user_id()?;
        let env = Envelope::recall(conv_id, me, target);
        let client = self.client()?;
        wait(client.send(&env)).map_err(|e| format!("send: {e:#}"))?;
        Ok(())
    }

    /// Read receipt for everything up to `up_to_hex` (P1): the hub routes it
    /// back to the original author, whose UI flips ✓ to ✓✓.
    pub fn send_read_receipt(&self, conv_id: &str, up_to_hex: &str) -> Result<(), String> {
        let up_to: MsgId = decode_msg_id(up_to_hex).ok_or("bad msg id")?;
        let me = self.user_id()?;
        let mut env =
            Envelope::with_body(conv_id, me, Vec::new(), 0, MsgBody::Notice(String::new()));
        env.kind = wxlike_core::MsgKind::System;
        env.quote_of = Some(up_to);
        env.receipt = Some(wxlike_core::Receipt::Read {
            up_to_msg_id: up_to,
        });
        let client = self.client()?;
        wait(client.send(&env)).map_err(|e| format!("send: {e:#}"))?;
        Ok(())
    }

    /// Merged forward: several messages bundled into one card.
    pub fn merge_forward(
        &self,
        to_conv: &str,
        to: Vec<String>,
        items: &[serde_json::Value],
    ) -> Result<(), String> {
        if items.is_empty() {
            return Err("没有选中要合并转发的消息".into());
        }
        // 与单条转发同一条规则：任何一条来自秘密会话，整批都不发。
        if items.iter().any(|item| {
            item.get("conv_id")
                .and_then(|v| v.as_str())
                .is_some_and(|from| self.conv_is_secret(from))
        }) {
            return Err("选中项里有秘密聊天的消息，不能转发".into());
        }
        self.send_body(
            to_conv,
            to,
            MsgBody::Custom {
                kind: custom_kind::MERGE_FORWARD.into(),
                payload: serde_json::json!({
                    "title": format!("{} 条聊天记录", items.len()),
                    "items": items,
                }),
            },
        )?;
        Ok(())
    }

    // ---- P1: history + search (cloud sync through the data plane) ------------

    /// One page of server-side history for a conversation; `before_seq = 0`
    /// means "newest page". Returns the raw reply (`messages` +
    /// `next_before_seq`), already mapped to the event payload shape.
    pub fn load_history(
        &self,
        conv_id: &str,
        before_seq: i64,
        limit: usize,
    ) -> Result<serde_json::Value, String> {
        self.http().expect_json(
            "GET",
            &format!("/api/history/{conv_id}?before_seq={before_seq}&limit={limit}"),
            None,
        )
    }

    pub fn search_messages(&self, q: &str, limit: usize) -> Result<serde_json::Value, String> {
        self.http().expect_json(
            "GET",
            &format!("/api/search/messages?q={}&limit={limit}", urlencode(q)),
            None,
        )
    }

    // ---- P1: favorites -------------------------------------------------------

    pub fn favorite_add(&self, item: &serde_json::Value) -> Result<serde_json::Value, String> {
        self.http()
            .expect_json("POST", "/api/favorites", Some(item))
    }

    pub fn favorites_list(&self, limit: usize) -> Result<serde_json::Value, String> {
        self.http()
            .expect_json("GET", &format!("/api/favorites?limit={limit}"), None)
    }

    pub fn favorite_remove(&self, id: &str) -> Result<(), String> {
        self.http()
            .expect_empty("DELETE", &format!("/api/favorites/{id}"), None)
    }

    // ---- P4: device management (sessions, remote sign-out) -------------------

    /// 设备列表：登录会话 + 推送令牌（页面合成一页展示）。
    pub fn sessions_list(&self) -> Result<serde_json::Value, String> {
        self.http().expect_json("GET", "/api/sessions", None)
    }

    /// 远程下线一台设备（按 sid；服务端立即删除会话行）。
    pub fn session_revoke(&self, sid: &str) -> Result<(), String> {
        self.http()
            .expect_empty("POST", &format!("/api/sessions/{sid}/revoke"), None)
    }

    /// 一键退出其他设备（保留当前 token）。
    pub fn sessions_revoke_others(&self) -> Result<serde_json::Value, String> {
        self.http()
            .expect_json("POST", "/api/sessions/revoke-others", None)
    }

    // ---- P1: E2EE secret chat (prototype + documented fallback) --------------

    /// Enables the local Olm account and publishes its pre-key bundle to the
    /// platform directory. Idempotent: enabling twice keeps the same keys.
    pub fn secret_enable(&self) -> Result<String, String> {
        {
            let guard = self.secret.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(store) = guard.as_ref() {
                return Ok(store.crypto.device().identity_fingerprint());
            }
        }
        let dir = self.data_dir()?;
        let me = self.user_id()?;
        // 先看磁盘：重启后必须还是**同一个身份**，否则对方的安全码会变、
        // 老会话全部解不开（issue #15）。
        let mut store = match SecretStore::open(&dir)? {
            Some(store) => store,
            None => SecretStore::create(&dir)?,
        };
        // 无论新旧都重发一次 bundle：一次性密钥可能已经用掉，服务端也可能
        // 换过库。重发是幂等的（`publish_keys` 是 upsert）。
        let bundle = store.crypto.bundle_refilled(&me);
        self.http().expect_empty(
            "POST",
            "/api/keys",
            Some(&serde_json::to_value(&bundle).map_err(|e| e.to_string())?),
        )?;
        store.persist()?;
        let fingerprint = store.crypto.device().identity_fingerprint();
        *self.secret.lock().unwrap_or_else(PoisonError::into_inner) = Some(store);
        Ok(fingerprint)
    }

    pub fn secret_enabled(&self) -> bool {
        self.secret
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// 打开/关闭某个会话的秘密聊天，并设置消失消息 TTL。
    ///
    /// 这是"开关接入发送链"的那一步：以前前端的 `secretConvs` 只是一个
    /// 进程内 `Set`，服务端根本不知道这个会话是秘密的，于是发送、转发、
    /// 历史过滤全都看不到它（issue #15）。
    pub fn secret_configure(
        &self,
        conv_id: &str,
        enabled: bool,
        ttl_ms: u64,
    ) -> Result<serde_json::Value, String> {
        if enabled {
            // 开会话之前先把本机账户准备好（身份 + bundle 发布）
            self.secret_enable()?;
        }
        let state = self.http().expect_json(
            "PUT",
            &format!("/api/secret/{conv_id}"),
            Some(&serde_json::json!({ "enabled": enabled, "ttl_ms": ttl_ms })),
        )?;
        self.cache_secret_conv(conv_id, enabled);
        Ok(state)
    }

    /// 服务端记着的秘密会话状态（开关 + TTL）。
    pub fn secret_state(&self, conv_id: &str) -> Result<serde_json::Value, String> {
        let state = self
            .http()
            .expect_json("GET", &format!("/api/secret/{conv_id}"), None)?;
        let enabled = state["enabled"].as_bool().unwrap_or(false);
        self.cache_secret_conv(conv_id, enabled);
        Ok(state)
    }

    fn cache_secret_conv(&self, conv_id: &str, enabled: bool) {
        self.secret_convs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(conv_id.to_string(), enabled);
    }

    /// 这个会话是不是秘密聊天（发送/转发闸门用）。
    ///
    /// 命中缓存就返回；否则问一次服务端。**查询失败时返回 false**：这是
    /// 客户端的显示/存储承诺，不是访问控制——服务端抖一下就"所有消息都
    /// 发不出去"是更坏的失败模式。真正的强制在 `secret_settings` 与
    /// 客户端把消息加密这两件事上。
    fn conv_is_secret(&self, conv_id: &str) -> bool {
        if let Some(v) = self
            .secret_convs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(conv_id)
        {
            return *v;
        }
        match self.secret_state(conv_id) {
            Ok(state) => state["enabled"].as_bool().unwrap_or(false),
            Err(_) => false,
        }
    }

    /// 会话的消失消息 TTL（毫秒，0 = 关）。查不到就按 0 处理。
    fn conv_ttl_ms(&self, conv_id: &str) -> u64 {
        self.secret_state(conv_id)
            .ok()
            .and_then(|v| v["ttl_ms"].as_u64())
            .unwrap_or(0)
    }

    /// Sends one secret-chat message. **fail closed**: any crypto-side failure
    /// (no peer bundle, drained pre-keys, no session) cancels the send — a
    /// secret chat must never degrade to cleartext (issue #4).
    pub fn send_secret_text(
        &self,
        conv_id: &str,
        to: Vec<String>,
        text: &str,
    ) -> Result<(), String> {
        let peer = to
            .first()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| "秘密聊天需要明确的接收方".to_string())?
            .clone();
        // 两条路分开：有会话就 `encrypt_dm_existing`（不需要 bundle），
        // 没会话才取 bundle 建会话。取不到就报错，不猜。
        let sealed = {
            let mut guard = self.secret.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(store) = guard.as_mut() else {
                return Err("secret chat not enabled".into());
            };
            let sealed = if store.crypto.has_session_with(&peer) {
                store.crypto.encrypt_dm_existing(&peer, text.as_bytes())
            } else {
                let bundle = self
                    .http()
                    .expect_json("GET", &format!("/api/keys/{peer}"), None)
                    .ok()
                    .and_then(|v| serde_json::from_value::<wxlike_core::e2ee::PreKeyBundle>(v).ok())
                    .filter(|p| !p.identity.is_empty() && !p.one_time_keys.is_empty())
                    .ok_or_else(|| {
                        format!("对方（{peer}）没有可用的预密钥：秘密聊天不能发送明文，已取消发送")
                    })?;
                store.crypto.encrypt_dm(&peer, &bundle, text.as_bytes())
            };
            let sealed = sealed
                .map_err(|err| format!("秘密聊天加密失败，已取消发送（不会退化成明文）：{err}"))?;
            // 棘轮已经前进：**发出去之前**先把新状态落盘，否则进程此刻被杀，
            // 下一条消息会用同一个索引重发，对方直接解不开。
            store.persist()?;
            sealed
        };

        let payload = serde_json::json!({
            "peer": peer,
            "pre_key": sealed.pre_key,
            "body": sealed.body,
        });
        // 本地回显用**自己已知的明文**：拿自己的密文走接收路径必然失败
        // （对端是自己，没有会话），以前的结果是卡片上写"无法解密"。
        self.send_body_with_echo(
            conv_id,
            to,
            MsgBody::Custom {
                kind: custom_kind::E2EE_DM.into(),
                payload,
            },
            Some(text),
        )?;
        Ok(())
    }

    /// The next envelope from the hub, as the structured event payload the
    /// frontend renders (`None` when not connected; the live pump in the
    /// running app loops on this, tests call it directly).
    pub async fn next_event(&self) -> Result<Option<serde_json::Value>, String> {
        let Some(client) = self
            .client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return Ok(None);
        };
        let env = client
            .next_envelope()
            .await
            .map_err(|e| format!("recv: {e:#}"))?;
        let payload = self.render_event(&env);
        self.local_append(&payload);
        Ok(Some(payload))
    }

    /// The receiver pump: forwards hub envelopes to the webview as `im-in`.
    pub fn start_recv_pump(&self, app: tauri::AppHandle) -> Result<(), String> {
        let Some(client) = self
            .client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return Err("未连接到服务器".into());
        };
        let secret = self.secret.clone();
        let local = self.local.clone();
        tauri::async_runtime::spawn(async move {
            use tauri::Emitter as _;
            loop {
                match client.next_envelope().await {
                    Ok(env) => {
                        let payload = render_event_with(&env, &secret, None);
                        {
                            let guard = local.lock().unwrap_or_else(PoisonError::into_inner);
                            if let Some(store) = guard.as_ref() {
                                let _ = store.append(&payload);
                            }
                        }
                        let _ = app.emit("im-in", payload);
                    }
                    Err(_) => {
                        let _ = app.emit("im-down", serde_json::json!({"reason": "hub 连接断开"}));
                        break;
                    }
                }
            }
        });
        Ok(())
    }

    /// Event payload with the local secret account applied (sealed messages
    /// come out as cleartext for the UI, or as a marked failure).
    fn render_event(&self, env: &Envelope) -> serde_json::Value {
        render_event_with(env, &self.secret, None)
    }
}

/// P2 local message library: the client's own SQLite mirror of the
/// conversations it has seen. "打开即渲染" — the chat list paints from here
/// before the network answers; the server history only contributes *older*
/// rows (and authoritative recall state).
///
/// Rows are the same JSON event payloads the webview renders, so the
/// frontend merges hub-live, local, and server pages with one function.
pub struct LocalStore {
    conn: Mutex<rusqlite::Connection>,
}

pub struct LocalRow {
    pub seq: i64,
    pub conv_id: String,
    pub sender: String,
    pub kind: String,
    pub text: Option<String>,
    pub media_id: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub msg_id: Option<String>,
    pub quote_of: Option<String>,
    pub sent_at_ms: i64,
}

impl LocalRow {
    fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "conv_id": self.conv_id,
            "seq": self.seq,
            "sender": self.sender,
            "kind": self.kind,
            "sent_at_ms": self.sent_at_ms,
        });
        if let Some(t) = &self.text {
            v["text"] = t.as_str().into();
        }
        if let Some(m) = &self.media_id {
            v["media_id"] = m.as_str().into();
        }
        if let Some(p) = &self.payload {
            v["payload"] = p.clone();
        }
        if let Some(id) = &self.msg_id {
            v["msg_id"] = id.as_str().into();
        }
        if let Some(q) = &self.quote_of {
            v["quote_of"] = q.as_str().into();
        }
        v
    }
}

impl LocalStore {
    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = rusqlite::Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS local_messages (
                conv_id    TEXT NOT NULL,
                seq        INTEGER NOT NULL,
                msg_id     TEXT NOT NULL,
                sender     TEXT NOT NULL DEFAULT '',
                kind       TEXT NOT NULL DEFAULT 'text',
                text       TEXT,
                media_id   TEXT,
                payload    TEXT,
                quote_of   TEXT,
                sent_at_ms INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (conv_id, seq, msg_id)
            );
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// One event payload → one row. Typing signals and receipts are refused
    /// by design: they are transient, and the local library must not replay
    /// "正在输入" on the next launch.
    pub fn append(&self, payload: &serde_json::Value) -> anyhow::Result<()> {
        let kind = payload["kind"].as_str().unwrap_or_default();
        if kind == "typing" || kind == "receipt" {
            return Ok(());
        }
        let conv_id = payload["conv_id"].as_str().unwrap_or_default().to_string();
        if conv_id.is_empty() {
            return Ok(()); // not a conversation event
        }
        let seq = payload["seq"].as_i64().unwrap_or(0);
        let msg_id = payload["msg_id"].as_str().unwrap_or_default().to_string();
        let sender = payload["sender"].as_str().unwrap_or_default().to_string();
        let text = payload["text"].as_str().map(str::to_string);
        let media_id = payload["media_id"].as_str().map(str::to_string);
        let quote_of = payload["quote_of"].as_str().map(str::to_string);
        let payload_json = payload.get("payload").map(|p| p.to_string());
        let sent_at_ms = payload["sent_at_ms"].as_i64().unwrap_or(0);
        self.lock().execute(
            "INSERT OR IGNORE INTO local_messages
             (conv_id, seq, msg_id, sender, kind, text, media_id, payload, quote_of, sent_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                conv_id,
                seq,
                msg_id,
                sender,
                kind,
                text,
                media_id,
                payload_json,
                quote_of,
                sent_at_ms
            ],
        )?;
        Ok(())
    }

    /// Newest-first page (`before_seq = 0` = latest), returned ascending so
    /// the caller prepends directly; `limit`-sized pages, shorter page = end.
    pub fn page(
        &self,
        conv_id: &str,
        before_seq: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<LocalRow>> {
        let limit = limit.clamp(1, 500) as i64;
        let mut rows = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT seq, conv_id, sender, kind, text, media_id, payload, msg_id, quote_of, sent_at_ms
                 FROM local_messages
                 WHERE conv_id = ?1 AND (?2 = 0 OR seq < ?2)
                 ORDER BY seq DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(rusqlite::params![conv_id, before_seq, limit], |r| {
                let payload_text: Option<String> = r.get(6)?;
                Ok(LocalRow {
                    seq: r.get(0)?,
                    conv_id: r.get(1)?,
                    sender: r.get(2)?,
                    kind: r.get(3)?,
                    text: r.get(4)?,
                    media_id: r.get(5)?,
                    payload: payload_text.and_then(|t| serde_json::from_str(&t).ok()),
                    msg_id: r.get(7)?,
                    quote_of: r.get(8)?,
                    sent_at_ms: r.get(9)?,
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        rows.reverse();
        Ok(rows)
    }
}

/// 桌面端的秘密聊天状态：Olm 账户 + 落盘路径 + 设备密钥。
///
/// 单独包一层是因为 `Crypto` 的棘轮**每发/每收一条都会变**，而磁盘上的副本
/// 必须跟着变：只在退出时保存，等于"崩溃后所有会话都解不开"。所以每次
/// 变动都调 [`SecretStore::persist`]。
struct SecretStore {
    crypto: Crypto,
    key: [u8; DEVICE_KEY_BYTES],
    path: std::path::PathBuf,
}

impl SecretStore {
    const SNAPSHOT_FILE: &'static str = "secret-account.bin";
    const KEY_FILE: &'static str = "secret-device.key";

    /// 打开已有状态。`Ok(None)` = 这台设备还没有秘密聊天账户。
    ///
    /// **有快照但没有密钥文件时报错**：那是"数据在、解不开"，不是"新设备"。
    /// 悄悄新建一个身份会让对方的安全码变化而毫无提示。
    fn open(dir: &std::path::Path) -> Result<Option<Self>, String> {
        let path = dir.join(Self::SNAPSHOT_FILE);
        let key_path = dir.join(Self::KEY_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let key = match std::fs::read(&key_path) {
            Ok(bytes) if bytes.len() == DEVICE_KEY_BYTES => {
                let mut key = [0u8; DEVICE_KEY_BYTES];
                key.copy_from_slice(&bytes);
                key
            }
            _ => {
                return Err(format!(
                    "秘密聊天密钥文件缺失或损坏（{}）：无法恢复身份，请删除 {} 后重新启用（会让对方的安全码变化）",
                    key_path.display(),
                    path.display()
                ))
            }
        };
        let blob = std::fs::read(&path).map_err(|e| format!("读秘密聊天快照失败：{e}"))?;
        let plain = wxlike_core::e2ee::open_at_rest(&key, &blob).map_err(|e| e.to_string())?;
        let crypto = Crypto::restore(&plain).map_err(|e| e.to_string())?;
        Ok(Some(Self { crypto, key, path }))
    }

    /// 新建账户（或复用已有的设备密钥）。
    fn create(dir: &std::path::Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建数据目录失败：{e}"))?;
        let key_path = dir.join(Self::KEY_FILE);
        let key = match std::fs::read(&key_path) {
            Ok(bytes) if bytes.len() == DEVICE_KEY_BYTES => {
                let mut key = [0u8; DEVICE_KEY_BYTES];
                key.copy_from_slice(&bytes);
                key
            }
            _ => {
                let mut key = [0u8; DEVICE_KEY_BYTES];
                rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut key);
                write_private(&key_path, &key)?;
                key
            }
        };
        Ok(Self {
            crypto: Crypto::new(wxlike_core::e2ee::Device::new()),
            key,
            path: dir.join(Self::SNAPSHOT_FILE),
        })
    }

    /// 原子写：先写 `.tmp` 再 rename，避免"写一半被杀"留下半截快照。
    fn persist(&self) -> Result<(), String> {
        let plain = self.crypto.snapshot().map_err(|e| e.to_string())?;
        let blob = wxlike_core::e2ee::seal_at_rest(&self.key, &plain).map_err(|e| e.to_string())?;
        write_private(&self.path, &blob)
    }
}

/// 写文件并收紧权限（Unix 0600）。Windows 上靠用户目录 ACL，这里只管 Unix。
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("替换 {} 失败：{e}", path.display()))?;
    Ok(())
}

/// A downloaded object for the webview: base64 (data-URL ready) + type.
#[derive(Serialize, Debug)]
pub struct MediaBlob {
    pub base64: String,
    pub content_type: String,
}

/// Maps an envelope to the event payload the UI renders. Sealed secret-chat
/// payloads are decrypted here (the fallback path marks failures instead of
/// dropping the card).
///
/// `own_text` 是**自己发出的秘密消息的明文**：发送方不能走接收路径去解自己
/// 的密文（对端是自己、没有会话），旧实现因此把每条自己发的秘密消息都渲染成
/// "无法解密"。
fn render_event_with(
    env: &Envelope,
    secret: &Mutex<Option<SecretStore>>,
    own_text: Option<&str>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "sender": env.sender,
        "conv_id": env.conv_id,
        "seq": env.seq,
        "sent_at_ms": env.sent_at_ms,
        "msg_id": hex(&env.msg_id),
        "quote_of": env.quote_of.map(|m| hex(&m)),
    });
    if let Some(receipt) = env.receipt {
        payload["kind"] = "receipt".into();
        payload["receipt"] = serde_json::json!({
            "state": match receipt {
                wxlike_core::Receipt::Delivered => "delivered",
                wxlike_core::Receipt::Read { .. } => "read",
            },
            "target": env
                .refers_to()
                .map(|m| hex(&m))
                .unwrap_or_default(),
        });
        return payload;
    }
    match &env.body {
        MsgBody::Text(t) => {
            payload["kind"] = "text".into();
            payload["text"] = t.as_str().into();
        }
        MsgBody::Bytes(b) => {
            payload["kind"] = "image".into();
            payload["media_id"] = String::from_utf8_lossy(b).into_owned().into();
        }
        MsgBody::Notice(n) => {
            payload["kind"] = "system".into();
            payload["text"] = n.as_str().into();
        }
        MsgBody::Recall { target } => {
            payload["kind"] = "recall".into();
            payload["target"] = hex(target).into();
        }
        MsgBody::Custom { kind, payload: p } => {
            payload["kind"] = kind.as_str().into();
            payload["payload"] = p.clone();
            if kind == custom_kind::E2EE_DM {
                payload["kind"] = "e2ee".into();
                if let Some(text) = own_text {
                    // 自己发的：明文就在手里，不需要（也不能）解密
                    payload["text"] = text.into();
                    payload["encrypted"] = true.into();
                    return payload;
                }
                match try_decrypt_secret(secret, p, &env.sender) {
                    Some(Ok(text)) => {
                        payload["text"] = text.into();
                        payload["encrypted"] = true.into();
                    }
                    Some(Err(err)) => {
                        eprintln!("[e2ee] inbound decrypt failed: {err}");
                        payload["text"] = "[加密消息·无法解密]".into();
                        payload["decrypt_failed"] = true.into();
                    }
                    None => {
                        payload["text"] = "[加密消息·本端未启用或格式不符]".into();
                        payload["decrypt_failed"] = true.into();
                    }
                }
            }
        }
    }
    payload
}

/// Standalone decryption used by [`render_event_with`] (kept separate so the
/// borrow of the secret lock stays short).
fn try_decrypt_secret(
    secret: &Mutex<Option<SecretStore>>,
    payload: &serde_json::Value,
    sender: &str,
) -> Option<Result<String, String>> {
    // **对端是发送者**，不是 payload 里的 `peer`。
    //
    // `peer` 是"发送者眼中的收件人"——对我们来说就是**自己**。拿它当对端去
    // 解密，必然找不到会话（或者找到一条通向自己的空会话），结果是
    // `decryption failed (wrong session, ...)`。
    let peer = sender.to_string();
    let sealed = wxlike_core::e2ee::Sealed {
        pre_key: payload.get("pre_key")?.as_bool()?,
        body: payload.get("body")?.as_str()?.to_string(),
    };
    let mut guard = secret.lock().unwrap_or_else(PoisonError::into_inner);
    let store = guard.as_mut()?;
    let opened = store
        .crypto
        .decrypt_dm(&peer, &sealed)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|err| format!("{err}"));
    if opened.is_ok() {
        // 收到一条 → 棘轮前进 → 立刻落盘（崩溃后仍能继续这条会话）
        if let Err(err) = store.persist() {
            eprintln!("[e2ee] 保存会话状态失败：{err}");
        }
    }
    Some(opened)
}

/// 队列项解码：裸 envelope（普通消息）或 `{env, echo}`（秘密消息，
/// `echo` 是回显要用的明文）。坏 payload 直接判永久失败——重试一万次
/// 也不会变好。
fn decode_outbox_task(
    payload: &serde_json::Value,
) -> Result<(Envelope, Option<String>), wxlike_core::outbox::SendFailure> {
    if let Ok(env) = serde_json::from_value::<Envelope>(payload.clone()) {
        return Ok((env, None));
    }
    let Some(inner) = payload.get("env") else {
        return Err(wxlike_core::outbox::SendFailure::permanent(
            "payload 解不开: 既不是 envelope 也不是 {env, echo}".to_string(),
        ));
    };
    let env = serde_json::from_value::<Envelope>(inner.clone())
        .map_err(|e| wxlike_core::outbox::SendFailure::permanent(format!("payload 解不开: {e}")))?;
    let echo = payload
        .get("echo")
        .and_then(|e| e.as_str())
        .map(str::to_string);
    Ok((env, echo))
}

/// 消失消息判定：`ttl_ms == 0` 表示不消失。
fn expired(sent_at_ms: Option<&serde_json::Value>, ttl_ms: u64, now: u64) -> bool {
    if ttl_ms == 0 {
        return false;
    }
    match sent_at_ms.and_then(serde_json::Value::as_u64) {
        // 时间戳缺失时**不隐藏**：宁可多显示一条，也不要凭空吞掉历史
        None => false,
        Some(sent) => sent.saturating_add(ttl_ms) <= now,
    }
}

/// Minimal percent-encoder for one query value (the search box can carry
/// anything the user typed, including `&` or CJK).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tauri command wrappers (thin: parse args, call AppState, flatten errors).
//
// They live in a submodule on purpose: tauri marks `pub` commands'
// helper macros with `#[macro_export]`, which hoists them to the crate
// root — for a command defined AT the root that collides with its own
// textual definition (E0255). A nested module keeps the two apart.
// ---------------------------------------------------------------------------

pub mod commands {

    use super::*;

    // -- P4/P5: 通话房间控制面 + 设备验证（前端不碰 HTTP，全部经此桥） --------

    /// 房间快照：`live=false` 表示当前没有通话。
    /// P3 未读数：服务端信箱是唯一真相（`MessageStore::unread`）。
    /// 走 `wxlike-client` 的领域方法，而不是在这里拼 URL——移动端要复用的正是这一层。
    #[tauri::command]
    pub fn conv_unread(
        state: tauri::State<'_, AppState>,
        conv: String,
    ) -> Result<serde_json::Value, String> {
        state.http().conv_unread(&conv)
    }

    /// 把已读游标推到最新（空 body = 读到最新）。返回推送后的快照。
    #[tauri::command]
    pub fn conv_mark_read(
        state: tauri::State<'_, AppState>,
        conv: String,
    ) -> Result<serde_json::Value, String> {
        state.http().mark_conv_read(&conv)
    }

    #[tauri::command]
    pub fn call_state(
        state: tauri::State<'_, AppState>,
        conv: String,
    ) -> Result<serde_json::Value, String> {
        state
            .http()
            .expect_json("GET", &format!("/api/call/{conv}"), None)
    }

    /// 统一入口：`action` ∈ start/join/leave/mute/mute-all/kick/lock/record/
    /// hand/transfer/egress。参数按动作取用，其余忽略。
    #[tauri::command]
    pub fn call_action(
        state: tauri::State<'_, AppState>,
        conv: String,
        action: String,
        target: Option<String>,
        track: Option<String>,
        value: Option<bool>,
        capacity: Option<usize>,
    ) -> Result<serde_json::Value, String> {
        let path = format!("/api/call/{conv}/{action}");
        let mut body = serde_json::Map::new();
        match action.as_str() {
            "mute" | "mute-all" => {
                body.insert(
                    "track".into(),
                    serde_json::json!(track.unwrap_or_else(|| "audio".into())),
                );
                body.insert("muted".into(), serde_json::json!(value.unwrap_or(true)));
                if let Some(t) = target {
                    body.insert("target".into(), serde_json::json!(t));
                }
            }
            "kick" | "transfer" => {
                let t = target.ok_or_else(|| "target is required".to_string())?;
                body.insert("target".into(), serde_json::json!(t));
            }
            "lock" | "record" | "hand" => {
                body.insert("value".into(), serde_json::json!(value.unwrap_or(true)));
            }
            "start" => {
                body.insert("capacity".into(), serde_json::json!(capacity));
            }
            _ => {}
        }
        if action == "egress" {
            return state.http().expect_json("GET", &path, None);
        }
        // 控制类端点一律 204：必须走 expect_empty——`into_json` 会把空 body
        // 当成 decode 错误（"EOF"），这里踩过一次。
        let payload = if body.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(body))
        };
        state.http().expect_empty("POST", &path, payload.as_ref())?;
        Ok(serde_json::json!({ "ok": true }))
    }

    /// 投递一条通话信令（offer/answer/ICE）。`from` 由服务端改写，别信客户端。
    #[tauri::command]
    pub fn call_signal_post(
        state: tauri::State<'_, AppState>,
        conv: String,
        signal: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        state
            .http()
            .expect_json("POST", &format!("/api/call/{conv}/signal"), Some(&signal))
    }

    /// 取走发给我的信令（读即清，服务端队列有界）。
    #[tauri::command]
    pub fn call_signal_take(
        state: tauri::State<'_, AppState>,
        conv: String,
    ) -> Result<serde_json::Value, String> {
        state
            .http()
            .expect_json("GET", &format!("/api/call/{conv}/signal"), None)
    }

    /// 安全码（12×5 数字）+ 对端身份指纹 + 我是否已标记验证。
    #[tauri::command]
    pub fn safety_number(
        state: tauri::State<'_, AppState>,
        peer: String,
    ) -> Result<serde_json::Value, String> {
        state
            .http()
            .expect_json("GET", &format!("/api/secret/{peer}/safety-number"), None)
    }

    /// 口头/扫码比对通过后落一条验证记录。
    #[tauri::command]
    pub fn verify_peer(
        state: tauri::State<'_, AppState>,
        peer: String,
        peer_identity: String,
    ) -> Result<(), String> {
        state.http().expect_empty(
            "PUT",
            &format!("/api/secret/{peer}/verify"),
            Some(&serde_json::json!({ "peer_identity": peer_identity })),
        )
    }

    #[tauri::command]
    pub fn set_server(state: tauri::State<'_, AppState>, base_url: String) -> Result<(), String> {
        state.set_server(&base_url);
        Ok(())
    }

    #[tauri::command]
    pub fn register(
        state: tauri::State<'_, AppState>,
        name: String,
        password: String,
    ) -> Result<serde_json::Value, String> {
        state.register(&name, &password)
    }

    #[tauri::command]
    pub fn login(
        state: tauri::State<'_, AppState>,
        name: String,
        password: String,
    ) -> Result<serde_json::Value, String> {
        state.login(&name, &password)
    }

    #[tauri::command]
    pub fn logout(state: tauri::State<'_, AppState>) -> Result<(), String> {
        state.logout()
    }

    #[tauri::command]
    pub fn me(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
        state.me()
    }

    #[tauri::command]
    pub fn update_profile(
        state: tauri::State<'_, AppState>,
        name: Option<String>,
        avatar: Option<String>,
        signature: Option<String>,
    ) -> Result<serde_json::Value, String> {
        state.update_profile(name.as_deref(), avatar.as_deref(), signature.as_deref())
    }

    #[tauri::command]
    pub fn friends(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
        state.friends()
    }

    #[tauri::command]
    pub fn friend_request(
        state: tauri::State<'_, AppState>,
        to_user_id: String,
        message: String,
    ) -> Result<(), String> {
        state.friend_request(&to_user_id, &message)
    }

    #[tauri::command]
    pub fn friend_requests_incoming(
        state: tauri::State<'_, AppState>,
    ) -> Result<serde_json::Value, String> {
        state.friend_requests_incoming()
    }

    #[tauri::command]
    pub fn friend_requests_outgoing(
        state: tauri::State<'_, AppState>,
    ) -> Result<serde_json::Value, String> {
        state.friend_requests_outgoing()
    }

    #[tauri::command]
    pub fn friend_accept(state: tauri::State<'_, AppState>, req_id: String) -> Result<(), String> {
        state.friend_accept(&req_id)
    }

    #[tauri::command]
    pub fn friend_decline(state: tauri::State<'_, AppState>, req_id: String) -> Result<(), String> {
        state.friend_decline(&req_id)
    }

    #[tauri::command]
    pub fn friend_delete(state: tauri::State<'_, AppState>, user_id: String) -> Result<(), String> {
        state.friend_delete(&user_id)
    }

    #[tauri::command]
    pub fn groups_create(
        state: tauri::State<'_, AppState>,
        name: String,
        member_ids: Vec<String>,
    ) -> Result<String, String> {
        state.groups_create(&name, &member_ids)
    }

    #[tauri::command]
    pub fn groups_mine(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
        state.groups_mine()
    }

    #[tauri::command]
    pub fn group_info(
        state: tauri::State<'_, AppState>,
        group_id: String,
    ) -> Result<serde_json::Value, String> {
        state.group_info(&group_id)
    }

    #[tauri::command]
    pub fn wallet_balance(state: tauri::State<'_, AppState>) -> Result<i64, String> {
        state.wallet_balance()
    }

    #[tauri::command]
    pub fn wallet_transfer(
        state: tauri::State<'_, AppState>,
        to_uid: String,
        amount: i64,
        memo: String,
    ) -> Result<String, String> {
        state.wallet_transfer(&to_uid, amount, &memo)
    }

    #[tauri::command]
    pub fn wallet_redpacket_cmd(
        state: tauri::State<'_, AppState>,
        amount: i64,
        count: i64,
        memo: String,
    ) -> Result<String, String> {
        state.wallet_redpacket(amount, count, &memo)
    }

    #[tauri::command]
    pub fn wallet_claim(
        state: tauri::State<'_, AppState>,
        redpacket_id: String,
    ) -> Result<i64, String> {
        state.wallet_claim(&redpacket_id)
    }

    #[tauri::command]
    pub fn upload_media(
        state: tauri::State<'_, AppState>,
        bytes: Vec<u8>,
        content_type: String,
        sha256: Option<String>,
    ) -> Result<serde_json::Value, String> {
        state.upload_media(bytes, &content_type, sha256.as_deref())
    }

    #[tauri::command]
    pub fn download_media(
        state: tauri::State<'_, AppState>,
        media_id: String,
        thumb: bool,
    ) -> Result<MediaBlob, String> {
        state.download_media(&media_id, thumb)
    }

    #[tauri::command]
    pub fn conversation_id(
        state: tauri::State<'_, AppState>,
        peer: String,
    ) -> Result<String, String> {
        let me = state.user_id()?;
        Ok(direct_conv_id(&me, &peer))
    }

    #[tauri::command]
    pub fn connect_hub(
        app: tauri::AppHandle,
        state: tauri::State<'_, AppState>,
        addr: String,
    ) -> Result<String, String> {
        // 连 hub 用**凭据**，不是 user_id（issue #3）
        let token = state
            .auth_token()
            .ok_or_else(|| "未登录：没有可用的凭据".to_string())?;
        let pins = app
            .path()
            .app_data_dir()
            .map(|dir| dir.join("tofu-pins.json"))
            .unwrap_or_else(|_| std::env::temp_dir().join("wxlike-tofu-pins.json"));
        let out = state.connect_hub(&addr, &token, &pins)?;
        // 连上才起泵：没连接时它只会每秒空转一次，无害但没必要
        state.start_outbox_pump(app.clone());
        Ok(out)
    }

    /// 发送队列状态：还有几条没发出去、哪几条彻底失败（UI 显示"重发/删除"）。
    #[tauri::command]
    pub fn outbox_status(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
        let Some(ob) = state.outbox() else {
            return Ok(serde_json::json!({ "pending": 0, "failed": [] }));
        };
        let pending = ob.pending_count().map_err(|e| e.to_string())?;
        let failed = ob.failed(50).map_err(|e| e.to_string())?;
        let failed: Vec<serde_json::Value> = failed
            .iter()
            .map(|t| {
                serde_json::json!({
                    "task_id": t.task_id,
                    "conv_id": t.conv_id,
                    "attempts": t.attempts,
                    "error": t.last_error,
                    "payload": t.payload,
                })
            })
            .collect();
        Ok(serde_json::json!({ "pending": pending, "failed": failed }))
    }

    /// 用户点"重发"：清零计数，交给后台泵。
    #[tauri::command]
    pub fn outbox_retry(state: tauri::State<'_, AppState>, task_id: String) -> Result<(), String> {
        let ob = state.outbox().ok_or("本地库未打开")?;
        ob.requeue(&task_id, wxlike_core::types::now_ms() as i64)
            .map_err(|e| e.to_string())
    }

    /// 网络诊断：DNS → TCP → HTTP → QUIC，四步顺序短路。
    ///
    /// 目标从当前连接推导（控制面 base + hub 地址），所以用户不需要填任何东西。
    /// 没连过 hub 也能跑——DNS/TCP/HTTP 三步照样能定位"为什么连不上"。
    #[tauri::command]
    pub fn net_diag(
        app: tauri::AppHandle,
        state: tauri::State<'_, AppState>,
    ) -> Result<serde_json::Value, String> {
        let base = state.http_base();
        let rest = base
            .strip_prefix("http://")
            .or_else(|| base.strip_prefix("https://"))
            .unwrap_or(&base)
            .trim_end_matches('/');
        let (host, http_port) = split_host_port(rest, 80);
        let quic_port = state
            .hub_addr()
            .and_then(|a| a.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok()))
            .unwrap_or(9443);

        let pins = app
            .path()
            .app_data_dir()
            .map(|dir| dir.join("tofu-pins.json"))
            .unwrap_or_else(|_| std::env::temp_dir().join("wxlike-tofu-pins.json"));
        let mut opts = wxlike_net::diag::DiagOptions::new(pins);
        // 诊断走的是同一个 `Hello`：要**凭据**，不是 user_id（issue #3）
        opts.token = state.auth_token();
        let target = wxlike_net::diag::DiagTarget::new(host, http_port, quic_port);

        let report = wait(wxlike_net::diag::run(&target, &opts));
        serde_json::to_value(&report).map_err(|e| e.to_string())
    }

    /// 用户点"删除"：放弃这条消息。
    #[tauri::command]
    pub fn outbox_drop(state: tauri::State<'_, AppState>, task_id: String) -> Result<(), String> {
        let ob = state.outbox().ok_or("本地库未打开")?;
        ob.remove(&task_id).map(|_| ()).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub fn send_text(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        to: Vec<String>,
        text: String,
    ) -> Result<(), String> {
        state.send_text(&conv_id, to, &text)
    }

    #[tauri::command]
    pub fn send_image(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        to: Vec<String>,
        media_id: String,
    ) -> Result<(), String> {
        state.send_image(&conv_id, to, &media_id)
    }

    #[tauri::command]
    pub fn send_redpacket_msg(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        to: Vec<String>,
        redpacket_id: String,
        amount: i64,
        count: i64,
        memo: String,
    ) -> Result<(), String> {
        state.send_redpacket(&conv_id, to, &redpacket_id, amount, count, &memo)
    }

    #[tauri::command]
    pub fn start_recv_pump(
        app: tauri::AppHandle,
        state: tauri::State<'_, AppState>,
    ) -> Result<(), String> {
        state.start_recv_pump(app)
    }

    // ---- P1 commands: five message operations + history + secret chat ------

    #[tauri::command]
    pub fn send_typing(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        to: Vec<String>,
    ) -> Result<(), String> {
        state.send_typing(&conv_id, to)
    }

    #[tauri::command]
    pub fn forward_message(
        state: tauri::State<'_, AppState>,
        to_conv: String,
        to: Vec<String>,
        item: serde_json::Value,
    ) -> Result<(), String> {
        state.forward_message(&to_conv, to, &item)
    }

    #[tauri::command]
    pub fn recall_msg(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        target: String,
    ) -> Result<(), String> {
        state.recall_msg(&conv_id, &target)
    }

    #[tauri::command]
    pub fn send_read_receipt(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        up_to: String,
    ) -> Result<(), String> {
        state.send_read_receipt(&conv_id, &up_to)
    }

    #[tauri::command]
    pub fn merge_forward(
        state: tauri::State<'_, AppState>,
        to_conv: String,
        to: Vec<String>,
        items: Vec<serde_json::Value>,
    ) -> Result<(), String> {
        state.merge_forward(&to_conv, to, &items)
    }

    #[tauri::command]
    pub fn load_history(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        before_seq: i64,
        limit: usize,
    ) -> Result<serde_json::Value, String> {
        state.load_history(&conv_id, before_seq, limit)
    }

    #[tauri::command]
    pub fn search_messages(
        state: tauri::State<'_, AppState>,
        q: String,
        limit: Option<usize>,
    ) -> Result<serde_json::Value, String> {
        state.search_messages(&q, limit.unwrap_or(20))
    }

    #[tauri::command]
    pub fn favorite_add(
        state: tauri::State<'_, AppState>,
        item: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        state.favorite_add(&item)
    }

    #[tauri::command]
    pub fn sessions_list(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
        state.sessions_list()
    }

    #[tauri::command]
    pub fn session_revoke(state: tauri::State<'_, AppState>, sid: String) -> Result<(), String> {
        state.session_revoke(&sid)
    }

    #[tauri::command]
    pub fn sessions_revoke_others(
        state: tauri::State<'_, AppState>,
    ) -> Result<serde_json::Value, String> {
        state.sessions_revoke_others()
    }

    #[tauri::command]
    pub fn favorites_list(
        state: tauri::State<'_, AppState>,
        limit: Option<usize>,
    ) -> Result<serde_json::Value, String> {
        state.favorites_list(limit.unwrap_or(20))
    }

    #[tauri::command]
    pub fn favorite_remove(state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
        state.favorite_remove(&id)
    }

    #[tauri::command]
    pub fn secret_enable(state: tauri::State<'_, AppState>) -> Result<String, String> {
        state.secret_enable()
    }

    #[tauri::command]
    pub fn secret_enabled(state: tauri::State<'_, AppState>) -> Result<bool, String> {
        Ok(state.secret_enabled())
    }

    #[tauri::command]
    pub fn secret_configure(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        enabled: bool,
        ttl_ms: Option<u64>,
    ) -> Result<serde_json::Value, String> {
        state.secret_configure(&conv_id, enabled, ttl_ms.unwrap_or(0))
    }

    #[tauri::command]
    pub fn secret_state(
        state: tauri::State<'_, AppState>,
        conv_id: String,
    ) -> Result<serde_json::Value, String> {
        state.secret_state(&conv_id)
    }

    #[tauri::command]
    pub fn send_secret_text(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        to: Vec<String>,
        text: String,
    ) -> Result<(), String> {
        state.send_secret_text(&conv_id, to, &text)
    }

    // ---- P2 commands: rich messages + video + avatar + local library ------

    #[tauri::command]
    pub fn send_voice(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        to: Vec<String>,
        media_id: String,
        duration_ms: u32,
        waveform: Vec<u8>,
    ) -> Result<(), String> {
        state.send_voice(&conv_id, to, &media_id, duration_ms, waveform)
    }

    #[tauri::command]
    pub fn send_file(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        to: Vec<String>,
        media_id: String,
        name: String,
        size: u64,
        sha256: String,
    ) -> Result<(), String> {
        state.send_file(&conv_id, to, &media_id, &name, size, &sha256)
    }

    #[tauri::command]
    pub fn send_sticker(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        to: Vec<String>,
        media_id: String,
        pack: String,
        id: String,
    ) -> Result<(), String> {
        state.send_sticker(&conv_id, to, &media_id, &pack, &id)
    }

    #[tauri::command]
    pub fn upload_video(
        state: tauri::State<'_, AppState>,
        bytes: Vec<u8>,
    ) -> Result<serde_json::Value, String> {
        state.upload_video(bytes)
    }

    #[tauri::command]
    pub fn send_video(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        to: Vec<String>,
        payload: serde_json::Value,
    ) -> Result<(), String> {
        state.send_video(&conv_id, to, &payload)
    }

    #[tauri::command]
    pub fn download_video(
        state: tauri::State<'_, AppState>,
        path: String,
    ) -> Result<MediaBlob, String> {
        state.download_video(&path)
    }

    #[tauri::command]
    pub fn upload_avatar(
        state: tauri::State<'_, AppState>,
        bytes: Vec<u8>,
        content_type: String,
    ) -> Result<String, String> {
        state.upload_avatar(bytes, &content_type)
    }

    /// 打开本地消息库。**只允许应用数据目录内的路径**。
    ///
    /// webview 传什么进来都不该直接交给 SQLite：一旦前端被 XSS，`../../` 就能
    /// 在磁盘任意位置建库/读库。这里按"归一化后必须落在 app_data_dir 内"判断，
    /// 而不是只查前缀字符串（`/data2` 不是 `/data` 的子目录）。
    #[tauri::command]
    pub fn local_open(
        app: tauri::AppHandle,
        state: tauri::State<'_, AppState>,
        path: String,
    ) -> Result<(), String> {
        let base = app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("wxlike-ui"));
        let base = normalize_path(&base);
        let want = normalize_path(std::path::Path::new(&path));
        if !want.starts_with(&base) {
            return Err(format!(
                "local_open 只允许应用数据目录内的库（拒绝 {path}）"
            ));
        }
        state.local_open(&want)
    }

    /// Opens the local message library at the app-data default; called once
    /// at webview boot (tests open an explicit path instead).
    #[tauri::command]
    pub fn local_autopen(
        app: tauri::AppHandle,
        state: tauri::State<'_, AppState>,
    ) -> Result<(), String> {
        let dir = app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("wxlike-ui"));
        state.local_open(&dir.join("messages.db"))
    }

    #[tauri::command]
    pub fn local_ready(state: tauri::State<'_, AppState>) -> Result<bool, String> {
        Ok(state.local_ready())
    }

    #[tauri::command]
    pub fn local_history(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        before_seq: i64,
        limit: usize,
    ) -> Result<serde_json::Value, String> {
        state.local_history(&conv_id, before_seq, limit)
    }

    // -- P5: 本地通话引擎（信令中继 + 真 UDP 媒体） --------------------------

    /// 发起通话：开媒体端口、把 offer 投进中继，返回本地 call_id。
    /// 房间控制面（`call_action` start）仍由调用方先做——两者是不同层次：
    /// 前者管"谁在房里"，这里管"声音怎么过去"。
    #[tauri::command]
    pub fn call_rtc_start(
        app: tauri::AppHandle,
        state: tauri::State<'_, AppState>,
        engine: tauri::State<'_, callrtc::CallEngine>,
        conv: String,
        peers: Vec<String>,
        video: Option<bool>,
    ) -> Result<Vec<String>, String> {
        let me = state.user_id()?;
        let (base, token) = {
            let http = state.http();
            let token = http.token.clone().ok_or("未登录")?;
            (http.base.clone(), token)
        };
        engine.start(
            app,
            callrtc::RelayCfg { base, token },
            me,
            conv,
            peers,
            video.unwrap_or(false),
        )
    }

    /// 接听当前响铃中的来电。
    #[tauri::command]
    pub fn call_rtc_accept(engine: tauri::State<'_, callrtc::CallEngine>) -> Result<(), String> {
        engine.accept();
        Ok(())
    }

    /// 挂断并停掉引擎线程。
    #[tauri::command]
    pub fn call_rtc_hangup(engine: tauri::State<'_, callrtc::CallEngine>) -> Result<(), String> {
        engine.hangup();
        engine.shutdown();
        Ok(())
    }

    /// 引擎状态快照（UI 首屏拿一次，之后靠 `call://rtc` 事件）。
    #[tauri::command]
    pub fn call_rtc_status(
        engine: tauri::State<'_, callrtc::CallEngine>,
    ) -> Result<callrtc::EngineStatus, String> {
        Ok(engine.status())
    }
} // mod commands

/// Runs the app. Kept in the library so `main.rs` stays a two-liner and the
/// command registry is testable next to the code it calls.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new("http://127.0.0.1:8099"))
        .manage(callrtc::CallEngine::default())
        .invoke_handler(tauri::generate_handler![
            commands::set_server,
            commands::conv_unread,
            commands::conv_mark_read,
            commands::call_state,
            commands::call_action,
            commands::call_signal_post,
            commands::call_signal_take,
            commands::call_rtc_start,
            commands::call_rtc_accept,
            commands::call_rtc_hangup,
            commands::call_rtc_status,
            commands::safety_number,
            commands::verify_peer,
            commands::register,
            commands::login,
            commands::logout,
            commands::me,
            commands::update_profile,
            commands::friends,
            commands::friend_request,
            commands::friend_requests_incoming,
            commands::friend_requests_outgoing,
            commands::friend_accept,
            commands::friend_decline,
            commands::friend_delete,
            commands::groups_create,
            commands::groups_mine,
            commands::group_info,
            commands::wallet_balance,
            commands::wallet_transfer,
            commands::wallet_redpacket_cmd,
            commands::wallet_claim,
            commands::upload_media,
            commands::download_media,
            commands::conversation_id,
            commands::connect_hub,
            commands::send_text,
            commands::outbox_status,
            commands::outbox_retry,
            commands::outbox_drop,
            commands::net_diag,
            commands::send_image,
            commands::send_redpacket_msg,
            commands::start_recv_pump,
            commands::send_typing,
            commands::forward_message,
            commands::recall_msg,
            commands::send_read_receipt,
            commands::merge_forward,
            commands::load_history,
            commands::search_messages,
            commands::favorite_add,
            commands::favorites_list,
            commands::sessions_list,
            commands::session_revoke,
            commands::sessions_revoke_others,
            commands::favorite_remove,
            commands::secret_enable,
            commands::secret_enabled,
            commands::secret_configure,
            commands::secret_state,
            commands::send_secret_text,
            commands::send_voice,
            commands::send_file,
            commands::send_sticker,
            commands::upload_video,
            commands::send_video,
            commands::download_video,
            commands::upload_avatar,
            commands::local_open,
            commands::local_autopen,
            commands::local_ready,
            commands::local_history,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `local_open` 的路径守卫：`..` 越界必须被挡，前缀相似但不同目录也要挡。
    #[test]
    fn normalize_path_blocks_traversal() {
        let base = normalize_path(std::path::Path::new("/data/app"));
        assert!(normalize_path(std::path::Path::new("/data/app/messages.db")).starts_with(&base));
        assert!(
            !normalize_path(std::path::Path::new("/data/app/../../etc/passwd")).starts_with(&base)
        );
        assert!(!normalize_path(std::path::Path::new("/data/application/x")).starts_with(&base));
    }
}
