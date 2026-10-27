//! AppState：一个窗口的整个后端状态与业务方法。
//!
//! G00-02 纯移动拆分：内容自 lib.rs 原样搬出，签名零改动；
//! 可见性经 lib.rs 的 pub(crate) 再导出 + `use super::*` 保持。

use super::*;

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
    /// 会话 TTL 缓存（0 = 不消失）。与本地库 `secret_ttls` 表同步，
    /// 这样重启后离线也能继续按期限清理。
    secret_ttls: Arc<Mutex<std::collections::HashMap<String, u64>>>,
    ttl_purge_started: AtomicBool,
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
    /// 收包泵代次：每次 start_recv_pump 抬一代，旧泵在下一条事件/错误后
    /// 退场——重连不再叠出双泵（旧实现重连后两个泵同时跑，重复 `im-in`、
    /// 旧泵断开时还会误报 `im-down`）。
    pump_generation: Arc<AtomicU64>,
    /// 本地防雪崩：给**自动重发**用的频率/流量漏斗。
    /// 用户手点发送不走它（`set_active` 的语义见 `crates/net/src/avalanche.rs`）。
    avalanche: Arc<wxlike_net::avalanche::Avalanche>,
}

/// 从登录/注册响应里取会话凭据：**缺字段就是失败，不许兜成空串**。
///
/// 旧写法 `reply["token"].as_str().unwrap_or_default()` 在服务端没给 token 时
/// 存下 `Some("")`，于是这次"成功登录"之后每个请求都带一个空 bearer：
/// 现象是远程 401 或超时，栈里查不到"登录其实没拿到凭据"这个真因（#293 台账第 4 条）。
/// 登录成不成功只看服务端响应本身，不能只看 `expect_json` 有没有回 `Ok`。
fn session_token(reply: &serde_json::Value, what: &str) -> Result<String, String> {
    let token = reply["token"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            format!(
                "{what}失败：服务端响应里没有 token（响应 2xx 但没有签发凭据，不是网络问题；\
                 已拒绝保存空凭据）"
            )
        })?;
    Ok(token.to_string())
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
            secret_ttls: Arc::new(Mutex::new(std::collections::HashMap::new())),
            ttl_purge_started: AtomicBool::new(false),
            local: Arc::new(Mutex::new(None)),
            outbox: Mutex::new(None),
            outbox_pump: std::sync::atomic::AtomicBool::new(false),
            pump_generation: Arc::new(AtomicU64::new(0)),
            avalanche: Arc::new(wxlike_net::avalanche::Avalanche::new(
                wxlike_net::avalanche::AvalancheConfig::messages(),
            )),
        }
    }

    pub(crate) fn http(&self) -> MutexGuard<'_, Http> {
        self.http.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn next_seq(&self) -> u64 {
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
        let token = session_token(&reply, "注册")?;
        self.http().token = Some(token);
        Ok(reply)
    }

    pub fn login(&self, name: &str, password: &str) -> Result<serde_json::Value, String> {
        let reply = self.http().expect_json(
            "POST",
            "/api/login",
            Some(&serde_json::json!({"name": name, "password": password})),
        )?;
        let token = session_token(&reply, "登录")?;
        self.http().token = Some(token);
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

    /// 直接调一条平台 API（e2e/诊断逃生口：业务方法都该有自己的包装，
    /// 新端点没包好之前测试与工具走这里，别绕过 AppState 私开 HTTP）。
    pub fn raw_json(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        self.http().expect_json(method, path, body)
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

    /// 上传一个对象。`conv_id` 是归属会话（issue #14）：不填就只有上传者能取；
    /// `public` 只给头像这类所有登录用户都要看的资源。
    pub fn upload_media(
        &self,
        bytes: Vec<u8>,
        content_type: &str,
        sha256: Option<&str>,
        conv_id: Option<&str>,
        public: bool,
    ) -> Result<serde_json::Value, String> {
        self.http()
            .upload(&bytes, content_type, sha256, conv_id, public)
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

    pub(crate) fn client(&self) -> Result<Client, String> {
        self.client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| "未连接到服务器".to_string())
    }

    pub(crate) fn send_body(
        &self,
        conv_id: &str,
        to: Vec<String>,
        body: MsgBody,
    ) -> Result<Envelope, String> {
        self.send_body_with_echo(conv_id, to, body, None)
    }

    /// `echo` 是**发送方自己已经知道的明文**（只有秘密聊天需要）：
    /// 自己的密文不能用接收方的路径去解——对端是自己，没有会话，必然失败。
    pub(crate) fn send_body_with_echo(
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
    pub(crate) fn append_own_echo(&self, env: &Envelope, echo: Option<&str>) {
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
    pub(crate) fn queue_for_retry(
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
                &hex_str(&env.msg_id),
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
    pub fn start_outbox_pump<R: tauri::Runtime>(&self, app: tauri::AppHandle<R>) {
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

        // ureq 3：`set` → `header`，`send_bytes` → `send`，响应体要显式取 body。
        // 4xx/5xx 不再是错误（`http_status_as_error(false)`），所以自己看 status。
        let mut resp = self
            .http()
            .req_write("POST", "/api/video")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .header("Content-Length", body.len().to_string())
            .send(&body)
            .map_err(|e| format!("upload video: {e}"))?;
        if !resp.status().is_success() {
            return Err(wxlike_client::upload_status_error(resp));
        }
        resp.body_mut()
            .read_json::<serde_json::Value>()
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
        let resp = self
            .http()
            .req("GET", path)
            .call()
            .map_err(|e| format!("download video: {e}"))?;
        if resp.status() == http::StatusCode::NOT_FOUND {
            return Err("video not found".to_string());
        }
        if !resp.status().is_success() {
            return Err(wxlike_client::upload_status_error(resp));
        }
        let content_type = wxlike_client::content_type_of(&resp);
        let mut bytes = Vec::new();
        // 3.x 的 BodyReader 没有 read_to_end：先读成 Vec 再展开
        let mut reader = resp.into_body().into_reader();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(format!("video read: {e}")),
            }
        }
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
        // 头像：所有登录用户都要能看到，所以标成 public
        let up = self.upload_media(bytes, content_type, None, None, true)?;
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
        // 重启后离线也要能继续清理：TTL 与开关一起落在本地库。
        let persisted = store.load_secret_ttls().unwrap_or_default();
        *self.outbox.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(outbox));
        *self.local.lock().unwrap_or_else(PoisonError::into_inner) = Some(store);
        {
            let mut ttls = self
                .secret_ttls
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            ttls.extend(persisted);
        }
        if let Some(dir) = path.parent() {
            *self.data_dir.lock().unwrap_or_else(PoisonError::into_inner) = Some(dir.to_path_buf());
        }
        self.start_ttl_purge();
        Ok(())
    }

    /// 数据目录。秘密聊天**必须**有它才能持久化密钥，所以没打开本地库时
    /// 明确报错，而不是退回进程内状态（那等于"重启换身份"）。
    pub(crate) fn data_dir(&self) -> Result<std::path::PathBuf, String> {
        self.data_dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| "本地库未打开：秘密聊天需要数据目录保存密钥".to_string())
    }

    /// 控制面地址（`http://host:port`），诊断用。
    pub(crate) fn http_base(&self) -> String {
        self.http
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .base
            .clone()
    }

    pub(crate) fn hub_addr(&self) -> Option<String> {
        self.hub_addr
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn outbox(&self) -> Option<Arc<wxlike_core::outbox::Outbox>> {
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
        // local 守卫只在读页的块内持有：后面的 conv_ttl_ms 缓存未命中时会
        // 走 secret_state → cache_secret_conv → 再锁 local——守卫不先放，
        // 同线程二次加锁就是死锁（重启后首次打开会话必现，审计 R5 实锤）。
        let rows = {
            let guard = self.local.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(store) = guard.as_ref() else {
                return Ok(serde_json::json!({"messages": [], "next_before_seq": null}));
            };
            store
                .page(conv_id, before_seq, limit)
                .map_err(|e| format!("local page: {e:#}"))?
        };
        let ttl = self.conv_ttl_ms(conv_id);
        let now = wxlike_core::types::now_ms();
        let rows: Vec<_> = rows
            .into_iter()
            .filter(|r| !expired(r.to_json().get("sent_at_ms"), ttl, now))
            .collect();
        // 消失消息：到期的那条不再出现在本地历史里（issue #15）。删除动作
        // 由本地库自己的清理负责，这里先保证"看不见"——界面上的承诺是
        // "到点消失"，不是"到点仍在但灰掉"。
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
        let enabled = state["enabled"].as_bool().unwrap_or(enabled);
        let ttl_ms = state["ttl_ms"].as_u64().unwrap_or(ttl_ms);
        self.cache_secret_conv(conv_id, enabled, ttl_ms);
        if enabled && ttl_ms > 0 {
            self.purge_expired_local();
        }
        Ok(state)
    }

    /// 服务端记着的秘密会话状态（开关 + TTL）。
    pub fn secret_state(&self, conv_id: &str) -> Result<serde_json::Value, String> {
        let state = self
            .http()
            .expect_json("GET", &format!("/api/secret/{conv_id}"), None)?;
        let enabled = state["enabled"].as_bool().unwrap_or(false);
        let ttl_ms = state["ttl_ms"].as_u64().unwrap_or(0);
        self.cache_secret_conv(conv_id, enabled, ttl_ms);
        Ok(state)
    }

    pub(crate) fn cache_secret_conv(&self, conv_id: &str, enabled: bool, ttl_ms: u64) {
        let ttl_ms = if enabled { ttl_ms } else { 0 };
        self.secret_convs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(conv_id.to_string(), enabled);
        self.secret_ttls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(conv_id.to_string(), ttl_ms);
        if let Some(store) = self
            .local
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            let _ = store.set_secret_ttl(conv_id, enabled, ttl_ms);
        }
    }

    /// 这个会话是不是秘密聊天（发送/转发闸门用）。
    ///
    /// 命中缓存就返回；否则问一次服务端。**查询失败时返回 false**：这是
    /// 客户端的显示/存储承诺，不是访问控制——服务端抖一下就"所有消息都
    /// 发不出去"是更坏的失败模式。真正的强制在 `secret_settings` 与
    /// 客户端把消息加密这两件事上。
    pub(crate) fn conv_is_secret(&self, conv_id: &str) -> bool {
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

    /// 会话的消失消息 TTL（毫秒，0 = 关）。先看本地缓存，查不到再问服务端；
    /// 两边都不可知时按 0 处理。
    pub(crate) fn conv_ttl_ms(&self, conv_id: &str) -> u64 {
        if let Some(ttl) = self
            .secret_ttls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(conv_id)
        {
            return *ttl;
        }
        self.secret_state(conv_id)
            .ok()
            .and_then(|v| v["ttl_ms"].as_u64())
            .unwrap_or(0)
    }

    /// 按缓存里的 TTL 清掉本地库中已到期的行。返回删除条数。
    pub(crate) fn purge_expired_local(&self) -> usize {
        let ttls: Vec<(String, u64)> = self
            .secret_ttls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, ttl)| **ttl > 0)
            .map(|(conv, ttl)| (conv.clone(), *ttl))
            .collect();
        if ttls.is_empty() {
            return 0;
        }
        let local = self.local.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(store) = local.as_ref() else {
            return 0;
        };
        store
            .purge_expired(&ttls, wxlike_core::types::now_ms())
            .unwrap_or(0)
    }

    /// 本地清除定时器：每 30 秒扫一次。服务端只兜底补发/检索，
    /// 已经落到本机的消息必须由客户端自己按 sent_at + ttl 删掉（#15）。
    pub(crate) fn start_ttl_purge(&self) {
        const PURGE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
        if self.ttl_purge_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let local = self.local.clone();
        let ttls = self.secret_ttls.clone();
        let _ = std::thread::Builder::new()
            .name("wxlike-ttl-purge".into())
            .spawn(move || loop {
                std::thread::sleep(PURGE_INTERVAL);
                let map: Vec<(String, u64)> = {
                    let guard = ttls.lock().unwrap_or_else(PoisonError::into_inner);
                    guard
                        .iter()
                        .filter(|(_, ttl)| **ttl > 0)
                        .map(|(conv, ttl)| (conv.clone(), *ttl))
                        .collect()
                };
                if map.is_empty() {
                    continue;
                }
                let guard = local.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(store) = guard.as_ref() {
                    let _ = store.purge_expired(&map, wxlike_core::types::now_ms());
                }
            });
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
    pub fn start_recv_pump<R: tauri::Runtime>(&self, app: tauri::AppHandle<R>) -> Result<(), String> {
        let Some(client) = self
            .client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return Err("未连接到服务器".into());
        };
        // 代次守卫：新泵起跑即宣告旧泵退场。旧泵在当前 await 返回后自检
        // 退出；它的 Client 已被替换，流本来也会断——不再重复发 `im-in`，
        // 断开误报（`im-down`）也只由当前代发。
        let pump_generation = self.pump_generation.clone();
        let generation = pump_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let secret = self.secret.clone();
        let local = self.local.clone();
        // 方向判定需要"我是谁"：连接时问一次控制面；失败不阻断泵
        //（消息方向退化为全部"入站"，多端同步自己的消息时才受影响）。
        let me = self.user_id().ok();
        tauri::async_runtime::spawn(async move {
            use tauri::Emitter as _;
            loop {
                if pump_generation.load(Ordering::SeqCst) != generation {
                    break; // 已有新泵接管
                }
                match client.next_envelope().await {
                    Ok(env) => {
                        if pump_generation.load(Ordering::SeqCst) != generation {
                            break;
                        }
                        let payload = render_event_with(&env, &secret, None);
                        {
                            let guard = local.lock().unwrap_or_else(PoisonError::into_inner);
                            if let Some(store) = guard.as_ref() {
                                let _ = store.append(&payload);
                            }
                        }
                        let _ = app.emit("im-in", &payload);
                        // 新 UI（契约 v1）的事件面：同一payload再发一份信封。
                        crate::core_bridge::forward_inbound(&app, &payload, me.as_deref());
                    }
                    Err(_) => {
                        if pump_generation.load(Ordering::SeqCst) == generation {
                            let _ = app.emit("im-down", serde_json::json!({"reason": "hub 连接断开"}));
                        }
                        break;
                    }
                }
            }
        });
        Ok(())
    }

    /// 本地库槽位（core 桥与备份导入共用同一把锁）。
    pub(crate) fn local_arc(&self) -> Arc<Mutex<Option<LocalStore>>> {
        self.local.clone()
    }

    /// 备份导入后重开本地库与发送队列（同一文件，两条连接一起换）。
    pub fn reopen_local(&self, path: &std::path::Path) -> Result<(), String> {
        let store = LocalStore::open(path).map_err(|e| format!("local db: {e:#}"))?;
        let outbox = wxlike_core::outbox::Outbox::open(Some(&path.to_string_lossy()))
            .map_err(|e| format!("outbox: {e:#}"))?;
        *self.outbox.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(outbox));
        *self.local.lock().unwrap_or_else(PoisonError::into_inner) = Some(store);
        Ok(())
    }

    /// Event payload with the local secret account applied (sealed messages
    /// come out as cleartext for the UI, or as a marked failure).
    pub(crate) fn render_event(&self, env: &Envelope) -> serde_json::Value {
        render_event_with(env, &self.secret, None)
    }
}
