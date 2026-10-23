//! MQ-routed hub: multi-node message routing, Rust port of box-im's design.
//!
//! Each hub:
//! - registers itself in the route table and consumes its own queue
//!   (`wxlike:msg:hub:{hub_id}`)
//! - binds connecting users (`user -> hub_id`)
//! - delivers locally when the recipient is here, otherwise publishes to the
//!   recipient hub's queue
//! - parks frames for recipients that are nowhere online (store-and-forward)
//!
//! Inbound rules (WP-03), applied in this order:
//! 1. **sender enforcement** — a frame may only claim the identity it said
//!    `Hello` with, so a client cannot forge someone else's recall (A3);
//! 2. **control frames** — receipts and recalls are routed but never stored as
//!    conversation content; a receipt goes back to the author of the message it
//!    acknowledges (recipients are deduplicated), and a recall is applied only
//!    when the store accepts it, so a forged one leaves no trace;
//! 3. **content dedup** — `msg_id` is checked against an in-hub LRU and, when a
//!    store is configured, against a UNIQUE index; a duplicate is dropped
//!    silently rather than reported as an error.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use wxlike_core::types::{MsgBody, MsgId, NO_MSG_ID};
use wxlike_core::{Envelope, Store};
use wxlike_mq::{hub_queue, MqRouter};

use crate::conn::PeerConn;
use crate::offline::{MemoryOffline, OfflineStore};
use crate::policy::DeliveryPolicy;
use crate::wire::WireFrame;

/// One bound connection of one user (ADR-4 multi-device): the peers table is
/// `user -> {conn_id -> PeerConn}`, so N devices stay live at once and a
/// delivery fans to every one of them.
pub type ConnId = u64;

/// Default dedup cache size required by the task book (>= 100k entries).
pub const DEFAULT_DEDUP_CAP: usize = 100_000;

/// WP-06: fired when an envelope is parked for a user with **no online
/// connection anywhere** (the route table knows about no hub for them).
///
/// The hook receives the envelope so the push layer can build its payload —
/// which by design carries only who sent it, never the content (see
/// `wxlike-push`). Best-effort: an error here never fails delivery, because
/// the message itself is already safely parked in the offline store.
#[async_trait::async_trait]
pub trait OfflinePush: Send + Sync + 'static {
    async fn user_offline(&self, user_id: &str, env: &Envelope);
}

/// Hub knobs. Defaults: in-memory offline inbox (cap 500), 100k dedup cache,
/// no message store (routing only), no reclaim patrol.
#[derive(Clone)]
pub struct MqHubOptions {
    pub offline: Arc<dyn OfflineStore>,
    pub storage: Option<Arc<Store>>,
    /// P3 读扩散信箱。内容帧落 `Store`（归档/扇出）之后同时镜像进它，
    /// 未读与多端同步才有统一真相。`None` = 不镜像。
    pub mailbox: Option<Arc<wxlike_message::MessageStore>>,
    pub dedup_cap: usize,
    /// Group membership source (WP-04). Without it a `conv_id` is never treated
    /// as a group, so group messages would fall back to plain broadcast.
    pub membership: Option<Arc<dyn crate::membership::Membership>>,
    /// Reclaim cross-node entries left unacked for this long. Only meaningful
    /// with an ack-capable backend; `None` disables the patrol. The task book
    /// asks for >= 60s, so `wxlike-server` uses [`DEFAULT_CLAIM_IDLE_MS`].
    pub reclaim_idle_ms: Option<u64>,
    /// WP-06: called when a recipient is offline everywhere. `None` = no push.
    pub push_hook: Option<Arc<dyn OfflinePush>>,
    /// P3 投递策略（拉黑/禁言/陌生人闸门）。`None` = 不过滤（既有测试与
    /// 无关系层部署保持零成本）。
    pub policy: Option<Arc<dyn DeliveryPolicy>>,
    /// **长连接的身份来源**（issue #3）。`Hello` 只带 token，身份由它解析。
    ///
    /// `None` 表示**拒绝所有连接**，不是"不校验"：一个没有认证器的 hub
    /// 就是"任何人报个 user_id 就能冒充"的服务，默默放行比拒绝更危险。
    /// 测试与本地开发用 [`crate::auth::StaticAuth`]；生产用装配层基于
    /// session 表的实现。
    pub auth: Option<Arc<dyn crate::auth::HubAuthenticator>>,
    /// QUIC 证书的持久化目录（issue #12）。`None` = 每次启动生成新证书（测试用）。
    ///
    /// 生产必须给：客户端按 TOFU 记住叶证书指纹，服务端一重启指纹就变，
    /// 旧客户端会遭遇与中间人攻击一模一样的拒绝。
    pub cert_dir: Option<std::path::PathBuf>,
}

/// Contract floor for reclaiming in-flight cross-node entries.
pub const DEFAULT_CLAIM_IDLE_MS: u64 = 60_000;

/// 投递任务保留窗口（issue #11）：一周之后不再补投，行也删掉。
const DELIVERY_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// 单条连接的写超时（issue #9）。
///
/// 写实时连接现在是 **await** 的（不 await 就不知道有没有人收到），但"等"必须
/// 有上界：对端不读流、缓冲满了的时候，无限等会把发送者的入站循环一起拖死。
/// 超时按"没写成功"处理 → 落收件人的离线队列，比卡住强。
const DELIVER_WRITE_TIMEOUT_MS: u64 = 2_000;

impl Default for MqHubOptions {
    fn default() -> Self {
        Self {
            offline: MemoryOffline::new(500),
            storage: None,
            mailbox: None,
            dedup_cap: DEFAULT_DEDUP_CAP,
            membership: None,
            reclaim_idle_ms: None,
            push_hook: None,
            policy: None,
            auth: None,
            cert_dir: None,
        }
    }
}

pub struct MqHub {
    pub hub_id: String,
    pub endpoint: quinn::Endpoint,
    handle: HubHandle,
    tasks: Vec<JoinHandle<()>>,
}

impl MqHub {
    pub fn local_addr(&self) -> anyhow::Result<std::net::SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    /// Server-side injection point: lets the platform service push system
    /// envelopes (friend/group events) through the same routing as client
    /// traffic, without holding a QUIC connection itself.
    pub fn handle(&self) -> HubHandle {
        self.handle.clone()
    }
}

/// 丢弃句柄 = 停止（中止任务 + 关 endpoint），但**不注销路由表**——
/// Drop 无法 await。模拟崩溃（ops_recovery 靠它释放端口重绑）、短命测试
/// 进程都依赖这个语义；要注销必须走 [`MqHub::shutdown`] 或 `leak` 的
/// [`HubLease`]。
impl Drop for MqHub {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        self.endpoint.close(0u32.into(), b"hub shutdown");
    }
}

impl MqHub {
    /// 完整优雅停机：路由表注销（多节点下其他 hub 立刻停止向它投递）→
    /// 中止任务 → 关 endpoint。`Drop` 只能做后两步（无法 await），
    /// 所以注销必须走这里或 [`MqHub::leak`] 的钩子。
    pub async fn shutdown(self) {
        if let Err(err) = self
            .handle
            .state
            .router
            .routes
            .unregister_hub(&self.hub_id)
            .await
        {
            eprintln!("[mq-hub] unregister_hub on shutdown failed: {err:#}");
        }
        for t in &self.tasks {
            t.abort();
        }
        self.endpoint.close(0u32.into(), b"hub shutdown");
    }

    /// M-11（第四轮审计 #32 补批）：常驻到进程退出（server 原先裸调
    /// `mem::forget` 语义的显式化）。返回的 [`HubLease`] 是泄漏之后唯一
    /// 能做的善后——路由表注销：优雅停机时让多节点拓扑立刻忘记本 hub，
    /// 而不是把死地址留到租约过期。
    pub fn leak(self) -> HubLease {
        let lease = HubLease {
            routes: self.handle.state.router.routes.clone(),
            hub_id: self.hub_id.clone(),
        };
        std::mem::forget(self);
        lease
    }
}

type Peers = Arc<RwLock<HashMap<String, HashMap<ConnId, PeerConn>>>>;

/// Everything the routing tasks share.
struct HubState {
    hub_id: String,
    peers: Peers,
    /// Connection id allocator (multi-device: several conns share one user).
    next_conn: std::sync::atomic::AtomicU64,
    router: MqRouter,
    offline: Arc<dyn OfflineStore>,
    storage: Option<Arc<Store>>,
    /// P3 读扩散信箱：内容帧除了落 `Store`（归档/扇出），也镜像进信箱，
    /// 未读与多端同步才有统一真相。`None` = 不镜像（老部署/纯路由测试）。
    mailbox: Option<Arc<wxlike_message::MessageStore>>,
    seen: Arc<tokio::sync::Mutex<MsgIdLru>>,
    reclaim_idle_ms: Option<u64>,
    membership: Option<Arc<dyn crate::membership::Membership>>,
    push_hook: Option<Arc<dyn OfflinePush>>,
    policy: Option<Arc<dyn DeliveryPolicy>>,
    /// 身份来源。**`None` = 拒绝所有连接**（fail closed，见 `crate::auth`）。
    auth: Option<Arc<dyn crate::auth::HubAuthenticator>>,
    /// WP-13: connections / throughput / parked / latency, per hub.
    metrics: crate::metrics::HubMetrics,
}

/// Cloneable handle for routing envelopes into a running hub.
#[derive(Clone)]
pub struct HubHandle {
    state: Arc<HubState>,
}

impl HubHandle {
    pub fn hub_id(&self) -> &str {
        &self.state.hub_id
    }

    /// Number of users currently bound to this hub (tests + metrics).
    pub async fn online_users(&self) -> usize {
        self.state.peers.read().await.len()
    }

    /// How many live connections `user` holds **on this hub** (multi-device
    /// view; the route table only knows the hub, this knows the devices).
    pub async fn conn_count(&self, user: &str) -> usize {
        self.state
            .peers
            .read()
            .await
            .get(user)
            .map(|c| c.len())
            .unwrap_or(0)
    }

    /// Attaches an already-established transport (WS route path, ADR-8):
    /// identical lifecycle to a QUIC connection — Hello, bind, drain, serve.
    pub fn attach_transport(&self, conn: PeerConn) -> tokio::task::JoinHandle<()> {
        let state = self.state.clone();
        tokio::spawn(async move {
            if let Err(err) = serve_transport(state, conn).await {
                eprintln!("[mq-hub] attached conn error: {err:#}");
            }
        })
    }

    /// Whether `user` has a live connection bound on any hub in the mesh
    /// (route table, so it is true across nodes). WP-06/WP-08 presence.
    pub async fn is_online(&self, user: &str) -> bool {
        self.state
            .router
            .routes
            .route_of(user)
            .await
            .map(|r| r.is_some())
            .unwrap_or(false)
    }

    /// 以某个用户身份提交一条消息，走**与 QUIC 帧完全相同**的路径：
    /// 发送者校验 → 投递策略 → 落库 → 镜像信箱 → 扇出。
    ///
    /// 给 HTTP 控制面（`POST /api/conv/{c}/send`）用。刻意不新写一条发送路径：
    /// 两套发送逻辑迟早会在"镜像了没""撤回了没"上分叉。
    pub async fn submit(&self, sender: &str, env: &Envelope) -> anyhow::Result<()> {
        handle_inbound(&self.state, sender, env).await
    }

    pub async fn publish(&self, env: &Envelope) -> anyhow::Result<()> {
        route_envelope(&self.state, env, true).await
    }

    /// Messages stored for a conversation, when this hub has a store attached.
    pub fn messages_of(
        &self,
        conv_id: &str,
    ) -> anyhow::Result<Vec<wxlike_core::store::StoredMessage>> {
        match &self.state.storage {
            Some(store) => store.list_messages(conv_id),
            None => anyhow::bail!("hub has no message store configured"),
        }
    }
}

/// Fixed-capacity FIFO of seen msg_ids: evicts the oldest once full.
struct MsgIdLru {
    cap: usize,
    order: VecDeque<MsgId>,
    seen: HashSet<MsgId>,
}

impl MsgIdLru {
    fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            order: VecDeque::new(),
            seen: HashSet::new(),
        }
    }

    /// 只看不插入。用于"先判断是否已落库，再决定要不要持久化"（issue #11）。
    fn contains(&self, id: MsgId) -> bool {
        self.seen.contains(&id)
    }

    /// 落库**成功之后**才调用。与 `check_and_insert` 的区别是它不返回"是否重复"，
    /// 因为调用方已经判断过了。
    fn insert(&mut self, id: MsgId) {
        if self.seen.insert(id) {
            self.order.push_back(id);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.seen.remove(&old);
                }
            }
        }
    }

    /// True when `id` had already been seen (i.e. this frame is a duplicate).
    fn check_and_insert(&mut self, id: MsgId) -> bool {
        if !self.seen.insert(id) {
            return true;
        }
        self.order.push_back(id);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        false
    }

    fn len(&self) -> usize {
        self.seen.len()
    }
}

/// Bind a QUIC hub that routes via `router` (memory or redis backend).
/// 便捷入口：**用 `StaticAuth`（token 原样当 `user_id`，不校验凭据）**。
///
/// 这是给测试与本地开发用的——它不查 session 表，任何非空字符串都能进来。
/// 生产必须用 [`run_mq_hub_with`] 注入真认证器（见 `crate::auth`）。
/// 为了不让它被误用，每次调用都会打一条 `warn` 日志。
pub async fn run_mq_hub(
    hub_id: impl Into<String>,
    addr: std::net::SocketAddr,
    router: MqRouter,
) -> anyhow::Result<MqHub> {
    let hub_id = hub_id.into();
    tracing::warn!(
        hub = %hub_id,
        "run_mq_hub 使用 StaticAuth（不校验凭据），仅用于测试与本地开发；         生产请用 run_mq_hub_with 注入基于 session 表的认证器"
    );
    run_mq_hub_with(
        hub_id,
        addr,
        router,
        MqHubOptions {
            auth: Some(Arc::new(crate::auth::StaticAuth)),
            ..Default::default()
        },
    )
    .await
}

/// Same, with explicit options (offline store, message store, dedup cap).
pub async fn run_mq_hub_with(
    hub_id: impl Into<String>,
    addr: std::net::SocketAddr,
    router: MqRouter,
    options: MqHubOptions,
) -> anyhow::Result<MqHub> {
    let hub_id = hub_id.into();
    let endpoint = match options.cert_dir.as_deref() {
        Some(dir) => crate::transport::make_server_endpoint_persistent(addr, dir)?,
        None => crate::transport::make_server_endpoint(addr)?,
    };

    router
        .routes
        .register_hub(&hub_id)
        .await
        .context("register hub")?;

    let state = Arc::new(HubState {
        hub_id: hub_id.clone(),
        peers: Arc::new(RwLock::new(HashMap::new())),
        next_conn: std::sync::atomic::AtomicU64::new(1),
        router,
        offline: options.offline.clone(),
        storage: options.storage.clone(),
        mailbox: options.mailbox.clone(),
        seen: Arc::new(Mutex::new(MsgIdLru::new(options.dedup_cap))),
        reclaim_idle_ms: options.reclaim_idle_ms,
        membership: options.membership.clone(),
        push_hook: options.push_hook.clone(),
        policy: options.policy.clone(),
        auth: options.auth.clone(),
        metrics: crate::metrics::HubMetrics::new(hub_id.clone()),
    });

    // accept loop
    let accept_state = state.clone();
    let ep = endpoint.clone();
    let accept_loop = tokio::spawn(async move {
        loop {
            let Some(incoming) = ep.accept().await else {
                break;
            };
            let Ok(conn) = incoming.await else { continue };
            let state = accept_state.clone();
            tokio::spawn(async move {
                if let Err(err) = serve_conn(conn, state).await {
                    eprintln!("[mq-hub] conn error: {err:#}");
                }
            });
        }
    });

    // queue consume loop: frames other hubs routed to us
    let consume_state = state.clone();
    let consume_loop = tokio::spawn(async move {
        let queue = hub_queue(&consume_state.hub_id);
        match consume_state.router.reliable.clone() {
            // at-least-once: ack only after local delivery completed, so a hub
            // that dies mid-delivery leaves the entry for the patrol to reclaim
            Some(rq) => loop {
                match rq.consume_entry(&queue).await {
                    Ok((id, bytes)) => {
                        // **投递成功才 ack**（issue #9）。以前无论有没有人收到都 ack，
                        // 目标 hub 上一旦没人连着，这条就只剩源端归档了。
                        match deliver_bytes(&consume_state, &bytes).await {
                            Ok(()) => {
                                if let Err(err) = rq.ack(&queue, &id).await {
                                    eprintln!("[mq-hub] ack {id} failed: {err:#}");
                                }
                            }
                            Err(err) => {
                                eprintln!(
                                    "[mq-hub] deliver {id} failed, leaving it for reclaim: {err:#}"
                                );
                                // 不 ack：等 lease 到期由 patrol 重新领走
                            }
                        }
                    }
                    Err(err) => {
                        eprintln!("[mq-hub] stream consume error: {err:#}");
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
            },
            None => loop {
                match consume_state.router.mq.consume(&queue).await {
                    Ok(bytes) => {
                        if let Err(err) = deliver_bytes(&consume_state, &bytes).await {
                            // 没有 ack 的队列无法重投，只能留日志（memory 后端仅测试/单机）
                            eprintln!("[mq-hub] deliver failed (no-ack backend): {err:#}");
                        }
                    }
                    Err(err) => {
                        eprintln!("[mq-hub] consume error: {err:#}");
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
            },
        }
    });

    // patrol: reclaim entries a dead consumer never acknowledged
    let patrol_loop = match (state.router.reliable.clone(), state.reclaim_idle_ms) {
        (Some(rq), Some(idle_ms)) => {
            let patrol_state = state.clone();
            let queue = hub_queue(&state.hub_id);
            let period = std::time::Duration::from_millis((idle_ms / 2).max(100));
            Some(tokio::spawn(async move {
                loop {
                    tokio::time::sleep(period).await;
                    match rq.claim_stale(&queue, idle_ms).await {
                        Ok(entries) => {
                            for (id, bytes) in entries {
                                if deliver_bytes(&patrol_state, &bytes).await.is_ok() {
                                    if let Err(err) = rq.ack(&queue, &id).await {
                                        eprintln!("[mq-hub] ack reclaimed {id} failed: {err:#}");
                                    }
                                }
                            }
                        }
                        Err(err) => eprintln!("[mq-hub] reclaim failed: {err:#}"),
                    }
                }
            }))
        }
        _ => None,
    };

    let mut tasks = vec![accept_loop, consume_loop];

    // 路由租约续租（issue #20）。只有带租约的路由表才需要：内存表
    // `lease_ttl_ms() == 0`，起这个循环纯属空转。
    //
    // 没有它，一台崩溃的 hub 会让**所有**曾经连过的用户永远路由到它身上——
    // 消息发进黑洞。有了租约，续租停止 = 路由过期 = 调用方重路由或落离线队列。
    let lease_ms = state.router.routes.lease_ttl_ms();
    if lease_ms > 0 {
        let routes = state.router.routes.clone();
        let peers = state.peers.clone();
        let hub_id = state.hub_id.clone();
        let period = std::time::Duration::from_millis((lease_ms / 3).max(1));
        tasks.push(tokio::spawn(async move {
            loop {
                tokio::time::sleep(period).await;
                // 本 hub 上所有绑着的用户。peers 是权威：连接断了就从这里消失，
                // 续租循环不会给一个已经不在的用户续命。
                let users: Vec<String> = peers.read().await.keys().cloned().collect();
                for user in users {
                    if let Err(err) = routes.renew_user(&user, &hub_id).await {
                        eprintln!("[mq-hub] renew route {user} failed: {err:#}");
                    }
                }
            }
        }));
    }

    // 投递任务的清理（issue #11）：任务行只在"这条消息可能还要补投"时有意义。
    // 留一周足够覆盖客户端重试窗口，再老的删掉——否则表只增不减。
    if let Some(store) = state.storage.clone() {
        let period = DELIVERY_RETENTION_MS as u64 / 4;
        tasks.push(tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(period)).await;
                match store.prune_deliveries(DELIVERY_RETENTION_MS) {
                    Ok(0) => {}
                    Ok(n) => tracing::debug!(rows = n, "pruned delivery outbox"),
                    Err(err) => eprintln!("[mq-hub] prune delivery outbox failed: {err:#}"),
                }
            }
        }));
    }
    tasks.extend(patrol_loop);

    Ok(MqHub {
        hub_id,
        endpoint,
        handle: HubHandle { state },
        tasks,
    })
}

/// Decode and route one payload taken off the queue.
/// 处理一条从 MQ 取来的帧（别的 hub 投给我们的）。
///
/// `Ok` = **已经交出去了**（写进实时连接，或落进收件人的离线队列），可以 ack；
/// `Err` = 没交出去，**不要 ack**——让 lease 到期后由 patrol 重投（issue #9）。
/// 旧实现在这里无条件返回 `()`，于是"目标 hub 上没人连着"也会被 ack，
/// 消息就只剩源端归档了。
async fn deliver_bytes(state: &Arc<HubState>, bytes: &[u8]) -> anyhow::Result<()> {
    let env = match WireFrame::from_bytes(bytes) {
        Ok(WireFrame::Msg(env)) => env,
        Ok(_) => return Ok(()),
        Err(err) => {
            // 坏帧重投一万次也不会变好：吃掉它，别把队列堵死
            eprintln!("[mq-hub] bad frame from mq: {err}");
            return Ok(());
        }
    };
    if env.to.is_empty() {
        // 广播帧没有确定的收件人列表，只能尽力而为（#13 会把广播本身收紧）
        deliver_local(state, &env).await;
        return Ok(());
    }
    let frame = WireFrame::Msg(env.clone()).to_bytes()?;
    let mut failed = Vec::new();
    for rcpt in &env.to {
        // 到这一步说明**源端**认为收件人在本 hub：只在本 hub 投，
        // 不再按路由转发（否则路由抖动时会来回弹）。
        if deliver_here(state, rcpt, &frame, true).await.is_err() {
            failed.push(rcpt.clone());
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("not delivered to {failed:?}")
    }
}

async fn serve_conn(conn: quinn::Connection, state: Arc<HubState>) -> anyhow::Result<()> {
    let (send, recv) = conn.accept_bi().await.context("accept control stream")?;
    serve_transport(state, PeerConn::quic(send, recv)).await
}

/// One bound connection of any carrier (QUIC stream pair or WS): Hello →
/// register → drain parked → serve until the peer goes away (ADR-4/8).
///
/// Multi-device unbind: a closing connection removes only its own conn id;
/// the route row and the user's other devices stay live until the LAST
/// connection of that user on this hub closes.
/// 拒绝握手：**先把原因发出去再关**。
///
/// 发不出去也不影响拒绝本身（对端可能已经走了），所以忽略发送错误——
/// 但绝不因为发送失败就改成放行。
async fn reject_hello(conn: &PeerConn, reason: &str) {
    let frame = WireFrame::HelloReject {
        reason: reason.to_string(),
    };
    if let Ok(bytes) = frame.to_bytes() {
        let _ = conn.send_frame(&bytes).await;
        // **QUIC 的 close 会丢掉未发出的流数据**：`write_all` 只是把帧交给
        // quinn 的发送缓冲，随后 `conn` 被 drop / 连接 close 时缓冲直接丢弃。
        // 实测表现就是客户端只看到 `closed by peer: 0`，拿不到原因。
        // 这里等一小会儿让帧真正上路——拒绝路径本来就要关连接，多 100ms
        // 没有代价；WS 载体是立即发出的，这段等待对它无影响。
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

async fn serve_transport(state: Arc<HubState>, conn: PeerConn) -> anyhow::Result<()> {
    // 首帧必须是 Hello{token}——**客户端不自报身份**（issue #3 / A01）。
    let hello = conn.read_frame().await.context("await hello")?;
    let token = match WireFrame::from_bytes(&hello)? {
        WireFrame::Hello { token } => token,
        _ => anyhow::bail!("first frame was not Hello"),
    };

    // fail closed：没有认证器就拒绝。**不要**在这里"为了兼容"放行——
    // 一个不校验身份的 hub，等于把"知道 user_id 就能冒充"变成协议特性。
    let Some(auth) = state.auth.as_ref() else {
        tracing::error!(
            hub = %state.hub_id,
            "hub has no authenticator configured; refusing every connection"
        );
        reject_hello(&conn, "hub is not accepting connections (no authenticator)").await;
        anyhow::bail!("hub is not accepting connections (no authenticator)");
    };
    let Some(identity) = auth.authenticate(&token) else {
        tracing::warn!(hub = %state.hub_id, "hello rejected: invalid or expired credential");
        reject_hello(&conn, "invalid or expired credential").await;
        anyhow::bail!("invalid credential");
    };
    let user_id = identity.user_id.clone();

    // 告诉客户端它是谁：`user_id` 由服务端给出，客户端不再自己拼。
    conn.send_frame(
        &WireFrame::HelloAck {
            user_id: user_id.clone(),
            session_id: identity.session_id.clone(),
        }
        .to_bytes()?,
    )
    .await
    .context("send hello ack")?;

    let conn_id = state
        .next_conn
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state
        .peers
        .write()
        .await
        .entry(user_id.clone())
        .or_default()
        .insert(conn_id, conn.clone());
    state.metrics.connected();
    tracing::debug!(hub = %state.hub_id, user = %user_id, conn = conn_id, "conn bound");

    state
        .router
        .routes
        .bind_user(&user_id, &state.hub_id)
        .await
        .context("bind user route")?;

    // store-and-forward：**claim → 逐条 ack**，不再"一次取走全部再发"（issue #10）。
    //
    // 旧实现先 `drain`（事务里读全部 + 删全部），然后逐条写连接。发到一半断连
    // 时，剩下的**已经不在表里**了——排队 100 条只发出 1 条，另外 99 条永久丢。
    // 现在每条投递成功才 ack（删），失败就 release（立刻重新可见），
    // 进程被杀也有 LEASE_MS 兜底。
    const CLAIM_BATCH: usize = 64;
    loop {
        let batch = match state.offline.claim(&user_id, CLAIM_BATCH).await {
            Ok(b) => b,
            Err(err) => {
                eprintln!("[mq-hub] offline claim failed: {err:#}");
                break;
            }
        };
        if batch.is_empty() {
            break;
        }
        tracing::info!(
            hub = %state.hub_id,
            user = %user_id,
            count = batch.len(),
            "flushing parked messages"
        );

        let mut delivered = 0usize;
        for (idx, (id, frame)) in batch.iter().enumerate() {
            // AR-6（#30）：过期的秘密会话帧不再补发。QUIC 与 WS 两条链路
            // 都走这条 claim→逐条 ack 的补发路径，一处判定即全覆盖；ack
            // 即丢弃，消息从此不可再取（与 HTTP 检索面的 TTL 过滤同口径）。
            if expired_secret_frame(state.policy.as_deref(), frame).await {
                if let Err(err) = state.offline.ack(*id).await {
                    eprintln!("[mq-hub] offline ack {id} (expired) failed: {err:#}");
                }
                tracing::info!(
                    hub = %state.hub_id,
                    user = %user_id,
                    id,
                    "dropped expired secret-chat frame from offline flush"
                );
                continue;
            }
            match conn.send_frame(frame).await {
                Ok(()) => {
                    // ack 失败不该让已经发出的条目再发一遍；下一轮 claim 会
                    // 因为 lease 未过期而跳过它们，最坏是重复一次。
                    if let Err(err) = state.offline.ack(*id).await {
                        eprintln!("[mq-hub] offline ack {id} failed: {err:#}");
                    }
                    delivered += 1;
                }
                Err(err) => {
                    // 这一条和它后面的全部放回去：连接已经坏了，继续发只会
                    // 一条条失败，还把这些条目白白 lease 住 30 秒。
                    for (id, _) in batch.iter().skip(idx) {
                        if let Err(e) = state.offline.release(*id).await {
                            eprintln!("[mq-hub] offline release {id} failed: {e:#}");
                        }
                    }
                    eprintln!("[mq-hub] parked deliver to {user_id} failed: {err}");
                    break;
                }
            }
        }
        state.metrics.offline_parked(-(delivered as i64));
        if delivered < batch.len() {
            // 连接坏了，本轮结束；剩下的等下次连接
            break;
        }
    }

    let res = loop {
        let frame = match conn.read_frame().await {
            Ok(f) => f,
            Err(err) => {
                if is_disconnect(&err) {
                    break Ok(());
                }
                break Err(err);
            }
        };
        let Ok(wire) = WireFrame::from_bytes(&frame) else {
            continue;
        };
        let env = match wire {
            WireFrame::Msg(env) => env,
            WireFrame::Ping => {
                // 心跳顺带复查凭据：HTTP 那边退出登录/远程下线之后，
                // 这条 QUIC/WS 连接不该还能收消息。最坏延迟 = 一个心跳周期。
                if state.auth.as_ref().is_some_and(|a| a.is_revoked(&token)) {
                    tracing::info!(hub = %state.hub_id, user = %user_id, "credential revoked; closing long connection");
                    break Err(anyhow::anyhow!("credential revoked"));
                }
                continue;
            }
            WireFrame::Hello { .. }
            | WireFrame::HelloAck { .. }
            | WireFrame::HelloReject { .. } => continue,
        };
        let started = std::time::Instant::now();
        let outcome = handle_inbound(&state, &user_id, &env).await;
        state.metrics.message_routed();
        state.metrics.delivery_latency(started.elapsed());
        tracing::trace!(
            hub = %state.hub_id,
            from = %user_id,
            conv = %env.conv_id,
            elapsed_us = started.elapsed().as_micros() as u64,
            "envelope routed"
        );
        if let Err(err) = outcome {
            eprintln!("[mq-hub] inbound error: {err:#}");
        }
    };

    // remove exactly this connection; unbind the route only when the user's
    // last connection on this hub is gone (ADR-4)
    let mut keep_bound = false;
    {
        let mut peers = state.peers.write().await;
        if let Some(conns) = peers.get_mut(&user_id) {
            conns.remove(&conn_id);
            if conns.is_empty() {
                peers.remove(&user_id);
            } else {
                keep_bound = true;
            }
        }
    }
    state.metrics.disconnected();
    if keep_bound {
        tracing::debug!(hub = %state.hub_id, user = %user_id, conn = conn_id, "conn unbound (devices remain)");
    } else {
        tracing::debug!(hub = %state.hub_id, user = %user_id, "user unbound");
        let _ = state
            .router
            .routes
            .unbind_user(&user_id, &state.hub_id)
            .await;
    }
    res
}

/// QUIC close surfaces as UnexpectedEof on the stream; WS close as a generic
/// error message. Both mean "the peer is gone", not "the transport is broken".
fn is_disconnect(err: &anyhow::Error) -> bool {
    let text = format!("{err:#}");
    text.contains("EOF") || text.contains("closed") || text.contains("connection lost")
}

/// 用户能合法发出的控制帧：回执（`receipt`）、撤回（`Recall` body）、typing。
///
/// 这三类以前也带 `Notice`/`System` 的形状（回执的 body 就是空 Notice），
/// 所以"禁掉 Notice"必须放行它们，否则读回执和撤回会一起失灵。
fn is_user_control_frame(env: &Envelope) -> bool {
    env.receipt.is_some() || env.is_recall() || env.is_typing()
}

/// 只有服务端能发的帧形状：`Notice`（界面上渲染成系统文案）。
///
/// **不能**用 `env.kind == System` 判断：`kind_of()` 把 `Custom`（typing、@、
/// 红包、通话信令）也归成 `System`，按 kind 拦会把用户自己的功能帧一起打死。
/// 形状才是契约：系统文案只有 `Notice` 这一种。
fn is_server_only_frame(env: &Envelope) -> bool {
    matches!(env.body, wxlike_core::MsgBody::Notice(_))
}

/// 直聊 `d-<a>~<b>` 的两个成员；不是直聊 id 就返回 `None`。
///
/// 只认这一种形状：`d-` 前缀 + 恰好一个 `~` + 两端非空。看不懂就当普通会话，
/// 交给成员表/收件人校验，不在这里猜。
fn direct_members(conv_id: &str) -> Option<(&str, &str)> {
    let rest = conv_id.strip_prefix("d-")?;
    let (a, b) = rest.split_once('~')?;
    if a.is_empty() || b.is_empty() || b.contains('~') || a == b {
        return None;
    }
    Some((a, b))
}

/// Everything a hub does with one frame arriving from a connected client.
async fn handle_inbound(
    state: &Arc<HubState>,
    bound_user: &str,
    env: &Envelope,
) -> anyhow::Result<()> {
    // 1. a client speaks only as itself
    if env.sender != bound_user {
        eprintln!(
            "[mq-hub] dropped frame from {bound_user} claiming sender {}",
            env.sender
        );
        return Ok(());
    }

    // 1.5 用户通道不接受"系统事件"（issue #13）。
    //
    // `Notice` 与 `kind = System` 在客户端是用来渲染"XX 已成为好友""群已解散"
    // 这类服务端通知的。允许普通账号发它，等于让任何人往别人界面上伪造系统
    // 文案——而且这类帧以前还**跳过** send_gate 与关系过滤。
    //
    // 服务端自己的通知走 `HubHandle::publish`（`route_envelope` 直投），
    // 不经过这里，所以"真的系统通知"不受影响。
    if !is_user_control_frame(env) && is_server_only_frame(env) {
        eprintln!(
            "[mq-hub] dropped server-only frame from {bound_user} (kind={:?})",
            env.kind
        );
        return Ok(());
    }

    // 2. group scoping, before anything is persisted: a message from someone who
    // is not a member is dropped and never stored (WP-04 A4).
    // P3：**成员名单也归信箱**（`member_snapshot`）。平台在每次成员变更时全量
    // 重置快照（建群/拉人/踢人/退群），这里直接读它。
    //
    // 信箱里没有成员时（老群、或部署没接信箱）退回平台成员表——这是迁移期
    // 的兜底，不是第二套真相：一旦成员变更过一次，快照就补上了。
    let from_mailbox = match (&state.mailbox, env.conv_id.starts_with("d-")) {
        (Some(mailbox), false) => mailbox
            .members((wxlike_message::OwnerType::Group, &env.conv_id))
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let members = if from_mailbox.is_empty() {
        match &state.membership {
            Some(membership) => membership.members_of(&env.conv_id).await?,
            None => None,
        }
    } else {
        Some(from_mailbox)
    };

    // 2.2 直聊的三元组一致性（issue #13）：sender / to / conv_id 必须自洽。
    //
    // `d-<a>~<b>` 里只有 a、b 两个人。以前客户端可以在这条会话里填任意 `to`，
    // 把"私聊"当成定向投递的跳板；也可以把 `to` 填成自己，制造一条谁也收不到
    // 的幽灵消息。控制帧（回执/撤回）的 `to` 由 hub 自己填，不做这项检查。
    if let Some((a, b)) = direct_members(&env.conv_id) {
        let other = if env.sender == a {
            Some(b)
        } else if env.sender == b {
            Some(a)
        } else {
            None
        };
        let Some(other) = other else {
            eprintln!(
                "[mq-hub] dropped frame from {} who is not in direct conv {}",
                env.sender, env.conv_id
            );
            return Ok(());
        };
        if !is_user_control_frame(env) && (env.to.len() != 1 || env.to[0] != other) {
            eprintln!(
                "[mq-hub] dropped direct frame with recipients {:?} (conv {} expects [{other}])",
                env.to, env.conv_id
            );
            return Ok(());
        }
    }

    // 2.3 用户通道不允许"空收件人广播"（issue #13）。
    //
    // 空 `to` 以前意味着"发给本 hub 所有在线用户 + 其他 hub 各一份"。群消息
    // 靠成员表扇出（`members` 有值），所以这里只拦非群的内容帧。服务端自己的
    // 广播走 `HubHandle::publish`，不经过这条路径。
    if !is_user_control_frame(env) && env.to.is_empty() && members.is_none() {
        eprintln!(
            "[mq-hub] dropped broadcast content frame from {} (conv {})",
            env.sender, env.conv_id
        );
        return Ok(());
    }

    // 2.5 P3 delivery policy (blocklist / mute / stranger gate) — content and
    // typing frames only; receipts, recalls and system (Notice) frames are
    // exempt. Dropped frames are not persisted, parked or fanned out.
    let mut env = if let Some(policy) = &state.policy {
        if !env.is_recall()
            && env.receipt.is_none()
            && !matches!(env.body, wxlike_core::MsgBody::Notice(_))
        {
            // P4 发送闸门（风控 + 内容审核）：先看"这个人此刻能不能发、
            // 这条内容能不能投"，再做关系裁决。挂起/拒绝的帧一律丢弃——
            // 与拉黑同一处理哲学：不落库、不暂存、不扇出。
            let text = match &env.body {
                wxlike_core::MsgBody::Text(t) => t.as_str(),
                _ => "",
            };
            match policy.send_gate(&env.sender, &env.conv_id, text).await {
                crate::SendGate::Allow => {}
                other => {
                    tracing::info!(
                        hub = %state.hub_id,
                        from = %env.sender,
                        conv = %env.conv_id,
                        gate = ?other,
                        "send gate dropped frame"
                    );
                    return Ok(());
                }
            }
            match apply_delivery_policy(state, policy.as_ref(), env, &members).await? {
                PolicyOutcome::Deliver(owned) => owned,
                PolicyOutcome::Drop => {
                    return Ok(());
                }
            }
        } else {
            env.clone()
        }
    } else {
        env.clone()
    };
    if let Some(members) = &members {
        if !members.iter().any(|m| m == &env.sender) {
            eprintln!(
                "[mq-hub] {} is not a member of group {}; frame dropped",
                env.sender, env.conv_id
            );
            return Ok(());
        }
    }

    // Transient signals (typing indicators) are the transitional form of the
    // P3 `high_priority` channel: routed to whoever is online right now,
    // never persisted, never parked — a user who connects later must not see
    // "对方正在输入" from an hour ago (contract §3.5).
    if env.is_typing() {
        return route_or_fanout(state, &env, members, false).await;
    }

    // Control frames (receipts, recalls) are never conversation content: they
    // are routed, and the dedup cache keeps a replay from notifying twice.
    if env.receipt.is_some() {
        if env.msg_id != NO_MSG_ID && state.seen.lock().await.check_and_insert(env.msg_id) {
            return Ok(());
        }
        let author = match (&state.storage, env.refers_to()) {
            (Some(store), Some(target)) => store.sender_of(&target)?,
            _ => None,
        };
        // addressed to the original author; `to` is kept as a hint but must not
        // duplicate the author, or they would be notified twice.
        let mut recipients: Vec<String> = Vec::with_capacity(env.to.len() + 1);
        if let Some(author) = author {
            recipients.push(author);
        }
        for rcpt in &env.to {
            if !recipients.contains(rcpt) {
                recipients.push(rcpt.clone());
            }
        }
        if recipients.is_empty() {
            eprintln!("[mq-hub] receipt for an unknown message, dropping");
            return Ok(());
        }
        let mut directed = env.clone();
        directed.to = recipients;
        return route_envelope(state, &directed, true).await;
    }

    if env.is_recall() {
        if env.msg_id != NO_MSG_ID && state.seen.lock().await.check_and_insert(env.msg_id) {
            return Ok(());
        }
        // Only effective when the store accepts it: an unknown id or a recall
        // of someone else's message is dropped before it can reach peers, so a
        // forged recall leaves no trace at all (WP-03 A3).
        // 信箱里的那份也要撤（时间线与未读都以它为准）
        if let (Some(mailbox), Some(target)) = (&state.mailbox, env.refers_to()) {
            let owner = if env.conv_id.starts_with("d-") {
                wxlike_message::OwnerType::Direct
            } else {
                wxlike_message::OwnerType::Group
            };
            let _ = mailbox.withdraw((owner, &env.conv_id), &hex::encode(target), &env.sender);
        }
        let permitted = match (&state.storage, env.refers_to()) {
            (Some(store), Some(target)) => store.withdraw(&target, &env.sender)?,
            // no store on this hub: forward, peers apply it to their own copies
            (None, _) => true,
            (Some(_), None) => false,
        };
        if !permitted {
            eprintln!("[mq-hub] recall refused (unknown or not the author's own)");
            return Ok(());
        }
        return route_or_fanout(state, &env, members, true).await;
    }

    // 3. content frames: **persist, then mark seen, then route**（issue #11）
    //
    // 旧顺序是"先 seen.insert，再 append，最后 fanout"，两个后果：
    // - `append` 失败时 seen 已经标记 → 客户端用同一个 msg_id 重试会被当成
    //   重复直接 return，消息永久丢失；
    // - `append` 返回 None（已落库，比如 hub 重启后）也直接 return，跳过 fanout
    //   → 上一次扇出失败的那批收件人永远补不上。
    //
    // 现在：落库成功才标记；标记命中说明"消息已在库里"，**跳过落库但继续扇出**
    // （收件人按 msg_id 去重，重复投递不会变成两条）。
    if env.msg_id != NO_MSG_ID {
        let already_persisted = state.seen.lock().await.contains(env.msg_id);
        if already_persisted {
            // 消息已在库里 → 不再落库，但**投递还要补**（issue #11）。
            //
            // `fanout_content` 只投 `delivery_outbox` 里还是 pending 的收件人：
            // 全部投递过 → 一个都不发（`ten_replays` 那条测试要的就是这个），
            // 上次有收件人失败 → 正好补齐。旧实现在这里直接 return，
            // 于是"客户端用正确的幂等 id 重试"反而补不上失败的那几个。
            let msg_key = hex::encode(env.msg_id);
            let recipients = content_recipients(&env, members.as_deref());
            if let Some(store) = &state.storage {
                store.ensure_deliveries(&msg_key, &recipients)?;
            }
            tracing::debug!(
                hub = %state.hub_id,
                msg = %msg_key,
                "duplicate msg_id: persisted already, retrying undelivered recipients"
            );
            return fanout_content(state, &env, &recipients, &msg_key).await;
        }
        if let Some(store) = &state.storage {
            // 失败会 `?` 出去，**seen 还没插入**——同一个 msg_id 可以重试。
            store.append(&env, false)?;
        }
        // 镜像进信箱（读扩散的唯一真相）。`d-` 前缀 = 直聊，其余按群处理；
        // 频道的消息不走这里（由 platform::send_channel_message 直接写信箱）。
        //
        // 顺带把**信箱序列号**写回 envelope：扇出到每个收件人的帧都带同一个
        // 会话内序号（1,2,3…），客户端因此有了服务端权威的排序，与未读、
        // ack、sync_plan 用的是同一套编号。之前 seq 是发送方填的（通常 0），
        // 客户端只能按到达顺序排——多端之间会不一致。
        if let Some(mailbox) = &state.mailbox {
            if let Some((type_id, payload)) = wxlike_message::mirror_of(&env) {
                let owner = if env.conv_id.starts_with("d-") {
                    wxlike_message::OwnerType::Direct
                } else {
                    wxlike_message::OwnerType::Group
                };
                let msg_id = hex::encode(env.msg_id);
                match mailbox.append(
                    (owner, &env.conv_id),
                    &env.sender,
                    &type_id,
                    &msg_id,
                    &payload,
                ) {
                    Ok(Some(seq)) => env.seq = seq,
                    // 重复 msg_id：信箱已有这一条，沿用它的序号
                    Ok(None) => {
                        if let Ok(seq) = mailbox.head_seq((owner, &env.conv_id)) {
                            env.seq = seq;
                        }
                    }
                    // 镜像失败不该阻断投递：归档已成功，用户仍能收到消息
                    Err(err) => eprintln!("[mq-hub] mailbox mirror failed: {err}"),
                }
            }
        }
        // **落库完成之后**才标记：这样"落库失败 → 客户端重试"仍然能进来。
        state.seen.lock().await.insert(env.msg_id);
    }

    // 内容帧：登记每个收件人的投递任务，然后**只投还没投出去的人**。
    //
    // 这样同一条 msg_id 重放时：已收到的人不会再收一遍（`ten_replays` 那条
    // 测试盯的就是这个），而上次失败的收件人会被补齐（issue #11）。
    let recipients = content_recipients(&env, members.as_deref());
    // 没有 msg_id 的老帧（v1）没有投递任务可查：空串 = "不走 outbox"。
    // 直接 `hex::encode(NO_MSG_ID)` 会得到一串 0，查出来是空列表，
    // 于是被当成"全都投递过"而一条都不发。
    let msg_key = if env.msg_id == NO_MSG_ID {
        String::new()
    } else {
        hex::encode(env.msg_id)
    };
    if let (Some(store), false) = (&state.storage, env.msg_id == NO_MSG_ID) {
        store.ensure_deliveries(&msg_key, &recipients)?;
    }
    fanout_content(state, &env, &recipients, &msg_key).await
}

/// 内容帧的收件人：群按成员表（去掉发送者），直聊按 `to`。
fn content_recipients(env: &Envelope, members: Option<&[String]>) -> Vec<String> {
    match members {
        Some(members) => members
            .iter()
            .filter(|m| *m != &env.sender)
            .cloned()
            .collect(),
        None => env.to.clone(),
    }
}

/// 把内容帧交给每个**还没收到**的收件人，成功的当场记进投递任务。
///
/// 并发写（每个 peer 有自己的写锁），但每个收件人的结果单独记账——这正是
/// 旧实现缺的那一半：要么整批成功，要么整批不知道。
async fn fanout_content(
    state: &Arc<HubState>,
    env: &Envelope,
    recipients: &[String],
    msg_key: &str,
) -> anyhow::Result<()> {
    let mut seen = std::collections::HashSet::new();
    let unique: Vec<String> = recipients
        .iter()
        .filter(|r| seen.insert((*r).clone()))
        .cloned()
        .collect();
    if unique.is_empty() {
        return Ok(());
    }
    // 有归档才有任务表；没有归档（纯路由）就退回"每个人都投一遍"。
    let pending = match &state.storage {
        Some(store) if !msg_key.is_empty() => match store.pending_deliveries(msg_key) {
            Ok(list) if list.is_empty() => return Ok(()), // 全部投递过：重放不再重发
            Ok(list) => list,
            Err(err) => {
                eprintln!("[mq-hub] read delivery outbox failed: {err:#}");
                unique
            }
        },
        _ => unique,
    };

    let tasks = pending.iter().map(|rcpt| {
        let state = state.clone();
        let rcpt = rcpt.clone();
        let frame = match WireFrame::Msg(mention_variant(env, &rcpt)).to_bytes() {
            Ok(frame) => frame,
            Err(err) => return tokio::spawn(async move { (rcpt, Err(err)) }),
        };
        tokio::spawn(async move {
            let outcome = deliver_via_route(&state, &rcpt, &frame, true).await;
            (rcpt, outcome)
        })
    });

    let mut first_error = None;
    for task in futures_util::future::join_all(tasks).await {
        let (rcpt, outcome) = match task {
            Ok(pair) => pair,
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(anyhow::anyhow!("fan-out task panicked: {err}"));
                }
                continue;
            }
        };
        match outcome {
            Ok(()) => {
                if let Some(store) = &state.storage {
                    if !msg_key.is_empty() {
                        if let Err(err) = store.mark_delivered(msg_key, &rcpt) {
                            eprintln!("[mq-hub] mark delivered {msg_key}/{rcpt} failed: {err:#}");
                        }
                    }
                }
            }
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
    }
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// Group conversations fan out by the member table; everything else keeps the
/// directed/broadcast semantics. `park_offline` = also queue for recipients
/// that are away (content frames); transient signals skip the inbox.
async fn route_or_fanout(
    state: &Arc<HubState>,
    env: &Envelope,
    members: Option<Vec<String>>,
    park_offline: bool,
) -> anyhow::Result<()> {
    match members {
        Some(members) => {
            let recipients: Vec<String> = members
                .iter()
                .filter(|m| **m != env.sender)
                .cloned()
                .collect();
            fanout(state, env, &recipients, park_offline).await
        }
        None => route_envelope(state, env, park_offline).await,
    }
}

/// Deliver one envelope to each recipient, rebuilding the frame per recipient
/// because an `@mention` is marked only for the people it names.
///
/// Recipients are written concurrently: every peer owns its own `SendStream`
/// mutex, so serializing them just made a single message's fan-out cost grow
/// with group size (200 members ≈ one message's worth of delivery latency,
/// which WP-04 A3's p99 budget cannot absorb).
async fn fanout(
    state: &Arc<HubState>,
    env: &Envelope,
    recipients: &[String],
    park_offline: bool,
) -> anyhow::Result<()> {
    let mut seen = std::collections::HashSet::new();
    let unique: Vec<String> = recipients
        .iter()
        .filter(|r| seen.insert((*r).clone()))
        .cloned()
        .collect();

    let tasks = unique.iter().map(|rcpt| {
        let state = state.clone();
        let rcpt = rcpt.clone();
        let frame = match WireFrame::Msg(mention_variant(env, &rcpt)).to_bytes() {
            Ok(f) => f,
            Err(err) => return tokio::spawn(async move { Err(err) }),
        };
        tokio::spawn(async move { deliver_via_route(&state, &rcpt, &frame, park_offline).await })
    });

    let mut first_error = None;
    for task in futures_util::future::join_all(tasks).await {
        match task {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(anyhow::anyhow!("fan-out task panicked: {err}"));
                }
            }
        }
    }
    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// 按路由把一帧交给一个用户（控制帧与瞬时信号走这里；内容帧走
/// [`fanout_content`]，因为内容帧还要记投递任务）。
///
/// 三条去路：
/// - 在本 hub：写实时连接；写不出去（没连或写失败）且允许离线时落他的离线队列；
/// - 在别的 hub：投给那个 hub 的队列；
/// - 谁都不知道他在哪：落离线队列（`park_offline` 为假时丢弃——typing 这类
///   瞬时信号本来就不该存）。
///
/// 返回 `Err` 表示**没交出去**：调用方据此决定要不要 ack / 记投递失败。
async fn deliver_via_route(
    state: &Arc<HubState>,
    user: &str,
    frame: &[u8],
    park_offline: bool,
) -> anyhow::Result<()> {
    match state.router.routes.route_of(user).await? {
        Some(h) if h == state.hub_id => deliver_here(state, user, frame, park_offline).await,
        Some(h) => {
            state
                .router
                .mq
                .publish(&hub_queue(&h), frame.to_vec())
                .await
        }
        None if park_offline => park(state, user, frame).await,
        // transient signal (typing): a user who is not online simply misses it
        None => Ok(()),
    }
}

/// 只在本 hub 投：实时连接优先，写不出去再落离线队列。
async fn deliver_here(
    state: &Arc<HubState>,
    user: &str,
    frame: &[u8],
    park_offline: bool,
) -> anyhow::Result<()> {
    if deliver_to(state, user, frame).await > 0 {
        return Ok(());
    }
    if !park_offline {
        return Ok(());
    }
    park(state, user, frame).await
}

/// 落进收件人的离线队列（durable inbox，claim/ack + lease）。
async fn park(state: &Arc<HubState>, user: &str, frame: &[u8]) -> anyhow::Result<()> {
    state.offline.enqueue(user, frame.to_vec()).await?;
    state.metrics.offline_parked(1);
    tracing::debug!(hub = %state.hub_id, user, "recipient offline, frame parked");
    if let Some(hook) = state.push_hook.clone() {
        let env = WireFrame::from_bytes(frame).ok().and_then(|f| match f {
            WireFrame::Msg(env) => Some(env),
            _ => None,
        });
        if let Some(env) = env {
            // fire-and-forget: push failure must not roll back the park
            let user = user.to_string();
            tokio::spawn(async move { hook.user_offline(&user, &env).await });
        }
    }
    Ok(())
}

/// The `@mention` payload carries the plain text plus who was named. Non-named
/// members receive the plain text; named members receive the marked form.
fn mention_variant(env: &Envelope, rcpt: &str) -> Envelope {
    let mut out = env.clone();
    out.to = vec![rcpt.to_string()];
    if let MsgBody::Custom { kind, payload } = &env.body {
        if kind == wxlike_core::custom_kind::MENTION {
            let named: Vec<String> = payload
                .get("user_ids")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let text = payload
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if !named.iter().any(|n| n == rcpt) {
                // not addressed to this member: strip the mention marker
                out.body = MsgBody::Text(text);
            }
        }
    }
    out
}

/// Deliver locally if recipients are here; publish to their hub otherwise.
/// Empty `to` = broadcast: everyone local except sender, plus every other hub.
/// `park_offline` = queue directed frames whose recipient is away.
async fn route_envelope(
    state: &Arc<HubState>,
    env: &Envelope,
    park_offline: bool,
) -> anyhow::Result<()> {
    if env.to.is_empty() {
        // broadcast: everyone local (minus the sender) plus every other hub
        deliver_local(state, env).await;
        let frame = WireFrame::Msg(env.clone()).to_bytes()?;
        for other in state.router.routes.list_hubs().await? {
            if other != state.hub_id {
                state
                    .router
                    .mq
                    .publish(&hub_queue(&other), frame.clone())
                    .await?;
            }
        }
        return Ok(());
    }
    // directed traffic uses the same per-recipient path as group fan-out:
    // concurrent writes, parked when the recipient is nowhere online
    fanout(state, env, &env.to, park_offline).await
}

/// Write to every local connection except the sender's. Multi-device: each
/// user may hold several conns and all of them get the frame (client-side
/// dedup by msg_id keeps echoes harmless).
async fn deliver_local(state: &Arc<HubState>, env: &Envelope) {
    let frame = match WireFrame::Msg(env.clone()).to_bytes() {
        Ok(f) => f,
        Err(_) => return,
    };
    let targets: Vec<PeerConn> = {
        let map = state.peers.read().await;
        map.iter()
            .filter(|(user, _)| **user != env.sender)
            .filter(|(user, _)| env.to.is_empty() || env.to.contains(user))
            .flat_map(|(_, conns)| conns.values().cloned())
            .collect()
    };
    let tasks: Vec<_> = targets
        .into_iter()
        .map(|conn| {
            let frame = frame.clone();
            tokio::spawn(async move {
                if let Err(err) = conn.send_frame(&frame).await {
                    eprintln!("[mq-hub] local deliver failed: {err}");
                }
            })
        })
        .collect();
    futures_util::future::join_all(tasks).await;
}

/// Write one frame to EVERY live connection of `user` (multi-device fan-out,
/// ADR-4). A user with two phones sees each message on both.
///
/// 返回**写成功的连接数**。以前这里是 `spawn` 之后立刻返回，调用方永远
/// 不知道有没有人收到——#9 的"确认了但没人收到"就是这么来的。
async fn deliver_to(state: &Arc<HubState>, user: &str, frame: &[u8]) -> usize {
    let conns: Vec<PeerConn> = {
        let map = state.peers.read().await;
        match map.get(user) {
            Some(c) => c.values().cloned().collect(),
            None => return 0,
        }
    };
    let mut written = 0;
    for conn in conns {
        let write = tokio::time::timeout(
            std::time::Duration::from_millis(DELIVER_WRITE_TIMEOUT_MS),
            conn.send_frame(frame),
        )
        .await;
        match write {
            Ok(Ok(())) => written += 1,
            Ok(Err(err)) => eprintln!("[mq-hub] deliver to {user} failed: {err}"),
            Err(_) => {
                eprintln!("[mq-hub] deliver to {user} timed out after {DELIVER_WRITE_TIMEOUT_MS}ms")
            }
        }
    }
    written
}

/// Dedup cache occupancy, exposed for tests and metrics.
pub async fn dedup_len(handle: &HubHandle) -> usize {
    handle.state.seen.lock().await.len()
}

/// P3 策略裁决：返回 Deliver（可能是收件集收窄后的副本）或 Drop。
///
/// * 群帧：查一次 `muted`（全员禁言对管理员的豁免在实现方内部裁决）；
/// * 直发帧：逐收件人查 `blocked` + `direct_allowed`，收件集只保留通过者；
///   全部被拦 → Drop（不落库、不暂存——被拉黑者的消息无痕迹）。
///
/// AR-6（#30）：这一帧是否已过秘密会话的 TTL（不再补发）。
/// 只有内容帧受约束；回执/撤回/系统帧豁免（撤回要在过期后仍能到达）。
async fn expired_secret_frame(
    policy: Option<&dyn crate::policy::DeliveryPolicy>,
    raw: &[u8],
) -> bool {
    let Some(policy) = policy else {
        return false;
    };
    // 离线库存的是序列化帧；解不开的帧不归 TTL 管（原样补发，由解码侧报错）
    let Ok(WireFrame::Msg(env)) = bincode::deserialize::<WireFrame>(raw) else {
        return false;
    };
    if env.is_recall() || env.receipt.is_some() {
        return false;
    }
    if matches!(env.body, wxlike_core::MsgBody::Notice(_)) {
        return false;
    }
    let Some(ttl_ms) = policy.secret_ttl_ms(&env.conv_id).await else {
        return false;
    };
    env.sent_at_ms.saturating_add(ttl_ms) < wxlike_core::types::now_ms()
}

async fn apply_delivery_policy(
    state: &Arc<HubState>,
    policy: &dyn DeliveryPolicy,
    env: &Envelope,
    members: &Option<Vec<String>>,
) -> anyhow::Result<PolicyOutcome> {
    match members {
        // 群帧：禁言裁决一次
        Some(_) => {
            if policy.muted(&env.conv_id, &env.sender).await {
                tracing::info!(
                    hub = %state.hub_id,
                    from = %env.sender,
                    conv = %env.conv_id,
                    "muted sender frame dropped"
                );
                return Ok(PolicyOutcome::Drop);
            }
            Ok(PolicyOutcome::Deliver(env.clone()))
        }
        // 直发帧：逐收件人
        None if !env.to.is_empty() => {
            let mut kept = Vec::with_capacity(env.to.len());
            for rcpt in &env.to {
                if policy.blocked(&env.sender, rcpt).await {
                    tracing::info!(
                        hub = %state.hub_id,
                        from = %env.sender,
                        to = %rcpt,
                        "blocked frame dropped"
                    );
                    continue;
                }
                if !policy.direct_allowed(&env.sender, rcpt).await {
                    tracing::info!(
                        hub = %state.hub_id,
                        from = %env.sender,
                        to = %rcpt,
                        "stranger-gate frame dropped"
                    );
                    continue;
                }
                kept.push(rcpt.clone());
            }
            if kept.is_empty() {
                return Ok(PolicyOutcome::Drop);
            }
            if kept.len() == env.to.len() {
                return Ok(PolicyOutcome::Deliver(env.clone()));
            }
            let mut adjusted = env.clone();
            adjusted.to = kept;
            Ok(PolicyOutcome::Deliver(adjusted))
        }
        // 广播（to 为空）：历史语义，不做关系过滤
        _ => Ok(PolicyOutcome::Deliver(env.clone())),
    }
}

enum PolicyOutcome {
    Deliver(Envelope),
    Drop,
}

/// [`MqHub::leak`] 的停机钩子：泄漏后 hub 本体不可达，唯一善后是路由表
/// 注销。进程意外崩溃时没有它，残留条目由 MQ 后端的租约回收兜底。
pub struct HubLease {
    routes: std::sync::Arc<dyn wxlike_mq::RouteTable>,
    hub_id: String,
}

impl HubLease {
    /// 从路由表注销本 hub（幂等；失败不 panic，由调用方记日志）。
    pub async fn unregister(&self) -> anyhow::Result<()> {
        self.routes.unregister_hub(&self.hub_id).await
    }
}
