//! `WxlikeClient`：移动端能调用的全部能力。
//!
//! 这里没有协议细节，只有三件事：**权限判定 → 发请求 → 把结果变成可渲染的行**。
//! 每条命令都从 `wxlike_client::execute::Executor` 走，所以移动端和 CLI/MCP/桌面端
//! 共用同一份 catalog 与同一个 `Policy`（`crates/cmd`）。想加能力就先去 catalog 加一条，
//! 而不是在这里多开一个 URL。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use serde_json::{json, Value};
use wxlike_client::execute::Executor;
use wxlike_client::Http;
use wxlike_cmd::policy::Policy;
use wxlike_core::outbox::{Outbox, OutboxTask, SendFailure};

use crate::cache::Cache;
use crate::error::{classify, WxlikeError};
use crate::types::{
    ChatMessage, ConversationList, ConversationRow, FlushReport, LiveInfo, LiveState, MessagePage,
    OutboxEntry, OutboxState, OutboxSummary, SendOutcome, SessionInfo, UnreadState,
};

/// 队列里有活干时的补发间隔（与桌面端 `start_outbox_pump` 同口径）。
const BUSY_TICK: Duration = Duration::from_millis(1_000);
/// 没事做时的轮询间隔。移动端不能像桌面端一样 1 秒一跳——待机耗电是真机上
/// 最先被用户发现的毛病（参照 `refs/mars` 的长短连与自适应心跳）。
const IDLE_TICK: Duration = Duration::from_secs(30);
/// 一轮最多发几条。
const FLUSH_BATCH: u32 = 20;
/// `Outbox::due` 只有"到点没有"的语义，没有"列出全部待发送"。给它一个足够大的
/// `now` 就等于"全都到点了"——UI 的待发送计数不能漏掉还在退避窗口里的消息。
const DUE_ALL: i64 = i64::MAX / 2;
/// 扫队列的上限：一屏渲染用不了更多，超出的只计入 `pending` 数字。
const QUEUE_SCAN: u32 = 200;

/// token 要原样进 `Authorization` 头，所以带 CR/LF 的一律当坏数据拒掉——
/// 否则服务端（或本地库被人动过）返回一行 `x\r\nX-Foo: 1` 就是在给请求头
/// 注入内容。CRLF 之外的字符由服务端自己负责。
fn usable_token(token: &str) -> bool {
    !token.is_empty() && !token.contains('\r') && !token.contains('\n')
}

/// 直聊会话 id 的算法在 `crates/client`（它是协议的一部分）。
///
/// 导出来是为了让 Dart 不用照着抄一遍——抄错就是"同一个人两个会话"。
#[uniffi::export]
pub fn direct_conv_id(a: String, b: String) -> String {
    wxlike_client::direct_conv_id(&a, &b)
}

#[derive(uniffi::Object)]
pub struct WxlikeClient {
    base: String,
    token: Mutex<Option<String>>,
    my_id: Mutex<String>,
    /// `connect()` 成功后为 true；网络错误时由 Rust 置回 false。
    online: AtomicBool,
    /// 补发线程的停机标志。
    stop: Arc<AtomicBool>,
    /// 停机时通过这个把线程从 `recv_timeout` 里叫醒。
    wake: Mutex<Sender<()>>,
    pump_error: Mutex<String>,
    policy: Policy,
    outbox: Outbox,
    cache: Cache,
}

impl Drop for WxlikeClient {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self
            .wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(());
    }
}

/// 不进绑定的内部动作：拼 HTTP、读凭据、写队列状态。
impl WxlikeClient {
    fn now_ms() -> i64 {
        wxlike_core::types::now_ms() as i64
    }

    fn http(&self) -> Http {
        let mut http = Http::new(self.base.clone());
        http.token = self
            .token
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        http
    }

    fn executor(&self) -> Executor {
        Executor::new(self.http(), self.policy.clone())
    }

    fn my_id(&self) -> String {
        self.my_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_token(&self, token: Option<String>) {
        *self.token.lock().unwrap_or_else(PoisonError::into_inner) = token;
    }

    fn token(&self) -> Option<String> {
        self.token
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_my_id(&self, user_id: &str) -> Result<(), WxlikeError> {
        *self.my_id.lock().unwrap_or_else(PoisonError::into_inner) = user_id.to_string();
        self.cache.set_meta("user_id", user_id)?;
        Ok(())
    }

    fn set_online(&self, online: bool) {
        self.online.store(online, Ordering::SeqCst);
        // 状态一变就跳一拍：断网要立刻降到空闲节拍，恢复要立刻开始补发
        let _ = self
            .wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(());
    }

    fn note_pump_error(&self, message: &str) {
        *self
            .pump_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = message.to_string();
    }

    fn clear_pump_error(&self) {
        self.pump_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// `register` / `login` 的同一条路。它们不在命令 catalog 里：那是 agent 能力表，
    /// 而建立凭据是客户端自己的动作。仍然由 Rust 发——Dart 全程碰不到 token。
    fn credentials(
        &self,
        name: &str,
        password: &str,
        login: bool,
    ) -> Result<SessionInfo, WxlikeError> {
        let path = if login { "/api/login" } else { "/api/register" };
        let reply = self
            .http()
            .expect_json(
                "POST",
                path,
                Some(&json!({ "name": name, "password": password })),
            )
            .map_err(|err| classify(&err))?;
        let user_id = reply["user_id"].as_str().unwrap_or_default();
        let token = reply["token"].as_str().unwrap_or_default();
        if user_id.is_empty() || !usable_token(token) {
            return Err(WxlikeError::unexpected(format!(
                "{path} 回来的凭据不完整或不可用（user_id / token 缺一个）"
            )));
        }
        self.set_token(Some(token.to_string()));
        self.set_my_id(user_id)?;
        self.cache.set_meta("token", token)?;
        self.set_online(true);
        Ok(SessionInfo {
            user_id: user_id.to_string(),
            server: self.base.clone(),
            signed_in: true,
        })
    }

    /// 每个会话还有几条没发出去。
    fn pending_by_conv(&self) -> Result<HashMap<String, i64>, WxlikeError> {
        let mut map: HashMap<String, i64> = HashMap::new();
        for task in self.scan_pending()? {
            *map.entry(task.conv_id).or_insert(0) += 1;
        }
        Ok(map)
    }

    fn scan_pending(&self) -> Result<Vec<OutboxTask>, WxlikeError> {
        self.outbox
            .due(DUE_ALL, QUEUE_SCAN)
            .map_err(|e| WxlikeError::unexpected(e.to_string()))
    }

    fn failed_tasks(&self) -> Result<Vec<OutboxTask>, WxlikeError> {
        self.outbox
            .failed(QUEUE_SCAN)
            .map_err(|e| WxlikeError::unexpected(e.to_string()))
    }

    /// 只负责"发一次"，不碰队列状态——这样 `Outbox::flush_with` 能直接把它当 sender 用。
    ///
    /// 幂等键就是 `task_id`：服务端用它派生 `msg_id`，所以重试同一个任务
    /// 不会产生第二条消息。
    fn try_send(&self, task: &OutboxTask) -> Result<String, SendFailure> {
        let conv = task
            .payload
            .get("conv")
            .and_then(Value::as_str)
            .unwrap_or(&task.conv_id);
        let text = task
            .payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if conv.is_empty() {
            return Err(SendFailure::permanent("队列里的这条没有目标会话"));
        }
        match self.executor().run(
            "msg.send",
            &json!({ "conv": conv, "text": text, "idempotency_key": task.task_id }),
        ) {
            Ok(reply) => Ok(reply["msg_id"].as_str().unwrap_or_default().to_string()),
            // 用构造器而不是字面量：`SendFailure` 以后再加字段（比如 `throttled`），
            // 这里不会再因为少一个字段而编译不过。
            Err(err) => Err(if classify(&err).retryable() {
                SendFailure::retryable(err)
            } else {
                SendFailure::permanent(err)
            }),
        }
    }

    /// 发一条并落地：成功 → `sent` + 本地占位行；失败 → 按错误语义决定重试还是判死。
    ///
    /// 队列状态写不进去时不咽下去，把它拼进 `error`：那条消息**已经**在服务端了，
    /// UI 必须同时知道"发出去了"和"本地状态没落上"。
    fn send_now(&self, task: &OutboxTask) -> SendOutcome {
        let mut notes: Vec<String> = Vec::new();
        match self.try_send(task) {
            Ok(msg_id) => {
                if let Err(err) = self.outbox.mark_sent(&task.task_id) {
                    // 下一轮会用同一个幂等键重发，服务端返回第一次的结果，
                    // 所以"重复发"在这里是安全的，"看不见失败"才危险。
                    notes.push(format!("已发送但队列状态没写成 sent（会幂等重发）: {err}"));
                }
                let mut row = message_of(task, &self.my_id());
                row.msg_id = msg_id.clone();
                if let Err(err) = self.cache.put_sent(&row) {
                    notes.push(format!("已发送但本地缓存没写上: {err}"));
                }
                SendOutcome {
                    task_id: task.task_id.clone(),
                    state: OutboxState::Sent,
                    msg_id,
                    error: notes.join("; "),
                }
            }
            Err(failure) => {
                let state = if failure.retryable {
                    match self.outbox.record_failure(
                        &task.task_id,
                        &failure.message,
                        Self::now_ms(),
                    ) {
                        // None = 退避次数用尽，队列自己已经标成 failed
                        Ok(None) => OutboxState::Failed,
                        Ok(Some(_)) => OutboxState::Pending,
                        Err(err) => {
                            notes.push(format!("退避状态没写上: {err}"));
                            OutboxState::Pending
                        }
                    }
                } else {
                    if let Err(err) = self.outbox.mark_failed(&task.task_id, &failure.message) {
                        notes.push(format!("失败状态没写上: {err}"));
                    }
                    OutboxState::Failed
                };
                notes.insert(0, failure.message);
                SendOutcome {
                    task_id: task.task_id.clone(),
                    state,
                    msg_id: String::new(),
                    error: notes.join("; "),
                }
            }
        }
    }
}

/// 绑定面。方法名就是 Dart/Kotlin 侧看到的名字（Dart 侧转成 camelCase）。
#[uniffi::export]
impl WxlikeClient {
    /// 建一个客户端：`base` 是服务端 HTTP 地址，`db_path` 是应用私有目录里的一个文件。
    ///
    /// 构造器是**可失败**的：库打不开必须让调用方看见。（用 constructor 而不是
    /// 顶层工厂函数，还有一层现实原因：`uniffi-bindgen-dart` 0.2.0 生成的
    /// "返回对象的顶层函数"少了 handle→对象的提升，Dart 侧编不过。）
    #[uniffi::constructor]
    pub fn new(base: String, db_path: String) -> Result<Arc<Self>, WxlikeError> {
        let path = PathBuf::from(&db_path);
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| {
                    WxlikeError::unexpected(format!("create {}: {e}", dir.display()))
                })?;
            }
        }
        let outbox = Outbox::open(Some(db_path.as_str()))
            .map_err(|e| WxlikeError::unexpected(e.to_string()))?;
        let cache = Cache::open(&path)?;
        let cached_token = cache.meta("token")?;
        let token = match cached_token.as_deref() {
            Some(token) if usable_token(token) => cached_token,
            // 存量的坏 token（空、或带 CR/LF）留在库里只会每次启动都撞同一条拒绝
            // 路径，直接删掉：带着它发请求是风险，留着它是噪声。
            Some(_) => {
                cache.set_meta("token", "")?;
                None
            }
            None => None,
        };
        let my_id = cache.meta("user_id")?.unwrap_or_default();

        let (wake_tx, wake_rx) = std::sync::mpsc::channel();
        let client = Arc::new(WxlikeClient {
            base,
            token: Mutex::new(token),
            my_id: Mutex::new(my_id),
            online: AtomicBool::new(false),
            stop: Arc::new(AtomicBool::new(false)),
            wake: Mutex::new(wake_tx),
            pump_error: Mutex::new(String::new()),
            // 移动端是"用户自己的客户端"：会话边界由服务端的鉴权与成员判定给，
            // 本地这层只关掉破坏性命令（撤回/踢人留给桌面端与后台）。
            policy: Policy::new(["*"], true, false),
            outbox,
            cache,
        });
        spawn_pump(Arc::downgrade(&client), client.stop.clone(), wake_rx);
        Ok(client)
    }

    // ---------------------------------------------------------------- 凭据

    /// 注册新账号并直接带着 token。
    pub fn register(&self, name: String, password: String) -> Result<SessionInfo, WxlikeError> {
        self.credentials(&name, &password, false)
    }

    /// 登录。token 落本地库，下次 `create_client` 直接恢复。
    pub fn login(&self, name: String, password: String) -> Result<SessionInfo, WxlikeError> {
        self.credentials(&name, &password, true)
    }

    /// 当前凭据状态（不联网）。启动时用 `signed_in` 决定进登录屏还是会话列表。
    pub fn session(&self) -> SessionInfo {
        SessionInfo {
            user_id: self.my_id(),
            server: self.base.clone(),
            signed_in: self.token().is_some(),
        }
    }

    /// 退出登录：清凭据，**也清本地消息与队列**。
    pub fn logout(&self) -> Result<(), WxlikeError> {
        self.set_online(false);
        self.cache.wipe()?;
        // 队列里没发出去的消息属于上一个人，不能留给下一个登录者
        let stuck: Vec<String> = self
            .scan_pending()?
            .into_iter()
            .map(|t| t.task_id)
            .chain(self.failed_tasks()?.into_iter().map(|t| t.task_id))
            .collect();
        for task_id in stuck {
            self.outbox
                .remove(&task_id)
                .map_err(|e| WxlikeError::unexpected(e.to_string()))?;
        }
        self.outbox
            .purge_sent(i64::MAX)
            .map_err(|e| WxlikeError::unexpected(e.to_string()))?;
        self.set_token(None);
        self.set_my_id("")?;
        Ok(())
    }

    // ---------------------------------------------------------------- 连接

    /// 验证 token、确认"我是谁"、开始补发离线期间攒下的消息。
    ///
    /// 移动端的实时到达由推送（B4）+ 主动拉 `history`/`unread` 承担，不走 QUIC：
    /// `crates/net` 把 hub、axum 和客户端塞在同一个 crate 里，链接它等于把
    /// 一个 web server 装进手机的 .so。
    ///
    /// 连不上时返回 `Err(Network)` **并且**把状态置为离线：调用方既要看到失败原因，
    /// 也要能在顶栏显示"离线"。
    pub fn connect(&self) -> Result<LiveInfo, WxlikeError> {
        if self.token().is_none() {
            return Err(WxlikeError::unauthorized(
                "还没有登录：先 register 或 login",
            ));
        }
        match self.executor().run("me.info", &json!({})) {
            Ok(me) => {
                let user_id = me["user_id"].as_str().unwrap_or_default().to_string();
                if user_id.is_empty() {
                    self.set_online(false);
                    return Err(WxlikeError::unexpected("/api/me 里没有 user_id"));
                }
                self.set_my_id(&user_id)?;
                self.set_online(true);
                // 离线期间攒下的先补一轮，不等 30 秒的空闲节拍
                self.flush_outbox()?;
                self.live_info()
            }
            Err(err) => {
                let classified = classify(&err);
                if classified.retryable() {
                    self.set_online(false);
                }
                Err(classified)
            }
        }
    }

    /// 主动断开：只停"在线"，不清凭据。补发线程转入空闲节拍。
    pub fn disconnect(&self) {
        self.set_online(false);
    }

    pub fn live_info(&self) -> Result<LiveInfo, WxlikeError> {
        let state = if self.token().is_none() {
            LiveState::SignedOut
        } else if self.online.load(Ordering::SeqCst) {
            LiveState::Online
        } else {
            LiveState::Offline
        };
        Ok(LiveInfo {
            state,
            user_id: self.my_id(),
            server: self.base.clone(),
            pending: i64::from(
                self.outbox
                    .pending_count()
                    .map_err(|e| WxlikeError::unexpected(e.to_string()))?,
            ),
            pump_error: self
                .pump_error
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        })
    }

    // ---------------------------------------------------------------- 读

    /// 会话列表。断网时返回本地缓存的那一份并标 `from_cache`；服务端明确拒绝才抛错。
    pub fn conversations(&self) -> Result<ConversationList, WxlikeError> {
        let pending = self.pending_by_conv()?;
        match self.executor().run("conv.list", &json!({})) {
            Ok(value) => {
                let mut conversations = Vec::new();
                for item in value.as_array().cloned().unwrap_or_default() {
                    let conv = item["conv"].as_str().unwrap_or_default().to_string();
                    conversations.push(ConversationRow {
                        kind: item["kind"].as_str().unwrap_or_default().to_string(),
                        title: item["title"].as_str().unwrap_or_default().to_string(),
                        members: item["members"].as_i64().unwrap_or(0),
                        // null / 缺字段 = 没查到，不是"0 条未读"
                        unread: item["unread"].as_i64().unwrap_or(-1),
                        pending: *pending.get(&conv).unwrap_or(&0),
                        conv,
                    });
                }
                self.cache.put_conversations(&conversations)?;
                Ok(ConversationList {
                    conversations,
                    from_cache: false,
                })
            }
            Err(err) => {
                let classified = classify(&err);
                if !classified.retryable() {
                    return Err(classified);
                }
                self.set_online(false);
                let mut conversations = self.cache.conversations(&self.my_id())?;
                for row in &mut conversations {
                    row.pending = *pending.get(&row.conv).unwrap_or(&0);
                }
                Ok(ConversationList {
                    conversations,
                    from_cache: true,
                })
            }
        }
    }

    /// 一页历史。`before_seq = 0` 表示"最新的一页"（与服务端 `conv.history` 同语义）。
    ///
    /// 连不上时退回本地缓存并把 `from_cache` 置真——这是 B3 的验收项（飞行模式下
    /// 打开应用还能看到历史），不是"悄悄返回空列表"。
    pub fn history(
        &self,
        conv: String,
        before_seq: i64,
        limit: i64,
    ) -> Result<MessagePage, WxlikeError> {
        let my_id = self.my_id();
        let params = json!({ "conv": conv, "before_seq": before_seq, "limit": limit });
        match self.executor().run("conv.history", &params) {
            Ok(page) => {
                let messages: Vec<ChatMessage> = page["messages"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|row| row_to_message(row, &my_id, &conv))
                    .collect();
                self.cache.put_messages(&messages);
                Ok(MessagePage {
                    next_before_seq: page["next_before_seq"].as_i64().unwrap_or(-1),
                    messages,
                    from_cache: false,
                })
            }
            Err(err) => {
                let classified = classify(&err);
                if !classified.retryable() {
                    return Err(classified);
                }
                self.set_online(false);
                let cursor = if before_seq <= 0 {
                    i64::MAX
                } else {
                    before_seq
                };
                let messages = self.cache.page_messages(&conv, cursor, limit, &my_id)?;
                Ok(MessagePage {
                    // 缓存页里最旧的一条再往前一格；空页 = 没有更多
                    next_before_seq: messages.first().map(|m| m.seq - 1).unwrap_or(-1),
                    messages,
                    from_cache: true,
                })
            }
        }
    }

    /// 未读快照。真相在服务端信箱（`crates/message`），这里只做搬运。
    pub fn unread(&self, conv: String) -> Result<UnreadState, WxlikeError> {
        let view = self
            .executor()
            .run("conv.unread", &json!({ "conv": conv }))
            .map_err(|err| classify(&err))?;
        Ok(view_to_unread(&view))
    }

    /// 把已读游标推到 `upto_seq`；`upto_seq <= 0` = 推到最新。
    pub fn mark_read(&self, conv: String, upto_seq: i64) -> Result<UnreadState, WxlikeError> {
        let params = if upto_seq > 0 {
            json!({ "conv": conv, "seq": upto_seq })
        } else {
            json!({ "conv": conv })
        };
        let view = self
            .executor()
            .run("conv.ack", &params)
            .map_err(|err| classify(&err))?;
        Ok(view_to_unread(&view))
    }

    /// 注册（或替换）这个设备的推送 token。
    ///
    /// 服务端路由是 `POST /api/devices`（按 `(user, platform)` 唯一，重复注册替换
    /// 旧 token），不在命令 catalog 里——它是设备自己的事，不是一条 agent 能力。
    pub fn register_push_token(&self, platform: String, token: String) -> Result<(), WxlikeError> {
        if token.trim().is_empty() {
            return Err(WxlikeError::unexpected("推送 token 是空的"));
        }
        self.http()
            .expect_empty(
                "POST",
                "/api/devices",
                Some(&json!({ "platform": platform, "device_token": token })),
            )
            .map_err(|err| classify(&err))
    }

    // ---------------------------------------------------------------- 发送

    /// 发一条文本：**先入队再发**，所以进程在半路被系统杀掉，消息还在队列里。
    ///
    /// 返回的 `state` 才是真相：
    /// * `Sent` —— 服务端已接收（`msg_id` 有值）；
    /// * `Pending` —— 没发出去，队列会退避重试（UI 显示"待发送"）；
    /// * `Failed` —— 服务端明确拒绝，不再自动重试（UI 显示原因 + 重试按钮）。
    ///
    /// 只有两种情况抛错：`Policy` 在入队之前就拒了这条命令，或本地库写不进去。
    pub fn send_text(&self, conv: String, text: String) -> Result<SendOutcome, WxlikeError> {
        let params = json!({ "conv": conv, "text": text });
        // 权限先于入队：被拒的命令不该在队列里留一条永远发不出去的僵尸行
        self.policy
            .authorize("msg.send", &params)
            .map_err(|deny| classify(&deny.to_string()))?;
        let task_id = self
            .outbox
            .enqueue(&conv, "text", &params)
            .map_err(|e| WxlikeError::unexpected(format!("enqueue: {e}")))?;
        let task = OutboxTask {
            task_id,
            conv_id: conv,
            kind: "text".to_string(),
            payload: params,
            attempts: 0,
            next_retry_at: Self::now_ms(),
            last_error: None,
        };
        Ok(self.send_now(&task))
    }

    /// 跑一轮补发（到点的任务逐条发；碰到可重试的失败就停，见 `Outbox::flush_with`）。
    pub fn flush_outbox(&self) -> Result<FlushReport, WxlikeError> {
        let report = self
            .outbox
            .flush_with(Self::now_ms(), FLUSH_BATCH, |task| {
                self.try_send(task).map(|_| ())
            })
            .map_err(|e| WxlikeError::unexpected(format!("flush: {e}")))?;
        Ok(FlushReport {
            sent: i64::from(report.sent),
            retrying: i64::from(report.retrying),
            failed: i64::from(report.failed),
        })
    }

    /// 队列现状：待发送/失败的行 + 计数（UI 的"待发送"角标与重试按钮）。
    pub fn outbox_status(&self) -> Result<OutboxSummary, WxlikeError> {
        let mut entries: Vec<OutboxEntry> = Vec::new();
        for task in self.scan_pending()? {
            entries.push(entry_of(task, OutboxState::Pending));
        }
        for task in self.failed_tasks()? {
            entries.push(entry_of(task, OutboxState::Failed));
        }
        Ok(OutboxSummary {
            pending: i64::from(
                self.outbox
                    .pending_count()
                    .map_err(|e| WxlikeError::unexpected(e.to_string()))?,
            ),
            failed: entries
                .iter()
                .filter(|e| e.state == OutboxState::Failed)
                .count() as i64,
            entries,
        })
    }

    /// 用户点"重试"：清零退避计数，立刻再发这一条。
    pub fn retry_outbox(&self, task_id: String) -> Result<OutboxSummary, WxlikeError> {
        self.outbox
            .requeue(&task_id, Self::now_ms())
            .map_err(|e| WxlikeError::unexpected(format!("requeue: {e}")))?;
        let task = self
            .scan_pending()?
            .into_iter()
            .find(|t| t.task_id == task_id)
            .ok_or_else(|| WxlikeError::unexpected(format!("队列里没有 {task_id}")))?;
        self.send_now(&task);
        self.outbox_status()
    }

    /// 用户放弃这条消息。
    pub fn drop_outbox(&self, task_id: String) -> Result<bool, WxlikeError> {
        self.outbox
            .remove(&task_id)
            .map_err(|e| WxlikeError::unexpected(format!("drop: {e}")))
    }
}

/// 补发线程：一条就够，跟着客户端活。
///
/// 它只在 `online` 时真发——离线时反复撞墙会把 5 次退避额度烧光，用户还没开机
/// 消息就"失败"了（`Outbox::MAX_ATTEMPTS`）。空闲节拍拉长到 30 秒是为了待机省电。
fn spawn_pump(weak: Weak<WxlikeClient>, stop: Arc<AtomicBool>, wake: Receiver<()>) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            // 客户端已经释放：线程跟着退，别留一个没有主人的轮询
            let Some(this) = weak.upgrade() else { break };
            let online = this.online.load(Ordering::SeqCst);
            let pending = match this.outbox.pending_count() {
                Ok(count) => count > 0,
                Err(err) => {
                    this.note_pump_error(&format!("队列读不出来: {err}"));
                    false
                }
            };
            let tick = if online && pending {
                BUSY_TICK
            } else {
                IDLE_TICK
            };
            if online && pending {
                match this.flush_outbox() {
                    Ok(_) => this.clear_pump_error(),
                    Err(err) => this.note_pump_error(&err.to_string()),
                }
            }
            // 用 recv_timeout 而不是 sleep：停机与状态变化要能立刻打断这一觉
            drop(this);
            match wake.recv_timeout(tick) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    });
}

fn entry_of(task: OutboxTask, state: OutboxState) -> OutboxEntry {
    OutboxEntry {
        // Failed 不会再自动重试，没有"下一次"
        next_retry_at: if state == OutboxState::Pending {
            task.next_retry_at
        } else {
            0
        },
        attempts: i64::from(task.attempts),
        last_error: task.last_error.unwrap_or_default(),
        text: task
            .payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        task_id: task.task_id,
        conv: task.conv_id,
        kind: task.kind,
        state,
    }
}

/// 队列任务变成一条可渲染的消息（只在服务端确认接收后才写进本地库）。
fn message_of(task: &OutboxTask, my_id: &str) -> ChatMessage {
    ChatMessage {
        conv: task.conv_id.clone(),
        // 真实 seq 要等服务端信箱分配；这条从历史里回来时按 msg_id 就地更新
        seq: 0,
        msg_id: String::new(),
        sender: my_id.to_string(),
        kind: task.kind.clone(),
        text: task
            .payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        media_id: String::new(),
        sent_at_ms: wxlike_core::types::now_ms() as i64,
        withdrawn: false,
        mine: true,
    }
}

fn view_to_unread(view: &Value) -> UnreadState {
    UnreadState {
        conv: view["conv"].as_str().unwrap_or_default().to_string(),
        owner: view["owner"].as_str().unwrap_or_default().to_string(),
        head_seq: view["head_seq"].as_i64().unwrap_or(0),
        read_seq: view["read_seq"].as_i64().unwrap_or(0),
        unread: view["unread"].as_i64().unwrap_or(0),
    }
}

/// 服务端的一行 → 可渲染的消息。
///
/// "哪种 kind 该显示成什么"是业务规则，写在 Rust：桌面端、网页端、移动端必须
/// 在同一条 recall 上给出同一句话。
fn row_to_message(row: &Value, my_id: &str, fallback_conv: &str) -> ChatMessage {
    let kind = row["kind"].as_str().unwrap_or("text").to_string();
    let sender = row["sender"].as_str().unwrap_or_default().to_string();
    let mine = !my_id.is_empty() && sender == my_id;
    let given = row["text"].as_str().unwrap_or_default().to_string();
    let text = match kind.as_str() {
        // 撤回行没有正文（服务端只给 `target`），这里按"谁撤的"补文案
        "recall" if given.is_empty() => {
            if mine {
                "你撤回了一条消息".to_string()
            } else {
                "对方撤回了一条消息".to_string()
            }
        }
        "image" => String::new(),
        // 自定义类型（E2EE、红包…）在 v1 的移动端还没有渲染器：明说，不留空白
        other if given.is_empty() => format!("[{other}]"),
        _ => given,
    };
    ChatMessage {
        conv: row["conv_id"].as_str().unwrap_or(fallback_conv).to_string(),
        seq: row["seq"].as_i64().unwrap_or(0),
        msg_id: row["msg_id"].as_str().unwrap_or_default().to_string(),
        sender,
        kind,
        text,
        media_id: row["media_id"].as_str().unwrap_or_default().to_string(),
        sent_at_ms: row["sent_at_ms"].as_i64().unwrap_or(0),
        withdrawn: row["withdrawn"].as_bool().unwrap_or(false),
        mine,
    }
}
