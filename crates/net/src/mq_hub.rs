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
use crate::delivery::{InflightLedger, Pending};
use crate::offline::{MemoryOffline, OfflineStore};
use crate::policy::{DeliveryPolicy, Limits, TokenBucket};
use crate::proto::Negotiated;
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
    /// G03-04 要求 3：frame/连接/订阅/用户四级上限。默认值与既有行为一致，
    /// 只有显式收紧的部署才改变裁决。
    pub limits: Limits,
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
            limits: Limits::default(),
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

/// 主动断开连接的句柄。QUIC 用整个连接（`close` 立即断开，所有流 reset）；
/// WS 载体没有等价的单句柄，走 [`PeerConn::close`]。
pub(crate) enum ConnCloser {
    Quic(quinn::Connection),
}

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
    /// G03-04：四级速率上限的生效值。
    limits: Limits,
    /// G03-02：写了但还没被客户端确认落库的帧（按连接、有上界）。
    inflight: Arc<InflightLedger>,
    /// 每用户发送令牌桶（`limits.max_msgs_per_user_per_min`）。
    user_rate: Arc<RwLock<HashMap<String, TokenBucket>>>,
    /// 每连接订阅数（`limits.max_subscriptions_per_conn`）。
    subscriptions: Arc<RwLock<HashMap<ConnId, usize>>>,
    /// 续租循环需要的世代号：`user -> generation`（G03-04 要求 1）。
    route_generations: Arc<RwLock<HashMap<String, u64>>>,
    /// conn_id -> 设备认领（v3 `ClientHello`），供按设备撤销（G02-09）。
    conn_devices: Arc<RwLock<HashMap<ConnId, String>>>,
    /// conn_id -> 主动断开句柄。QUIC 立即 `Connection::close`；WS 无句柄，
    /// 走 `PeerConn::close`（发 Close 帧）。
    conn_closers: Arc<RwLock<HashMap<ConnId, ConnCloser>>>,
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

    /// 断开某用户在本 hub 上的**全部**连接（多设备，ADR-4），返回断开的连接数。
    /// G02-09 封禁/撤销会话的进程内效果：会话行已被域删掉，这里把还连着的
    /// WS/QUIC 泵一并拔掉，否则"后台封了、前端还收消息"。
    pub async fn disconnect_user(&self, user: &str) -> usize {
        let conns: Vec<ConnId> = {
            let peers = self.state.peers.read().await;
            peers
                .get(user)
                .map(|m| m.keys().copied().collect())
                .unwrap_or_default()
        };
        for conn_id in conns.iter().copied() {
            disconnect_conn(&self.state, user, conn_id).await;
        }
        conns.len()
    }

    /// 只断开该用户绑在某设备上的连接（设备撤销精确打击）。
    /// `device_id` 来自 `ClientHello`（记在 `conn_devices` 表），
    /// 未认领设备的连接（空 device_id）不会被误伤。返回是否真的断开了连接。
    pub async fn disconnect_device(&self, user: &str, device_id: &str) -> bool {
        let conn_id = {
            let devices = self.state.conn_devices.read().await;
            devices
                .iter()
                .find(|(_, d)| d.as_str() == device_id)
                .map(|(c, _)| *c)
        };
        match conn_id {
            Some(c) => {
                disconnect_conn(&self.state, user, c).await;
                true
            }
            None => false,
        }
    }

    /// Attaches an already-established transport (WS route path, ADR-8):
    /// identical lifecycle to a QUIC connection — Hello, bind, drain, serve.
    pub fn attach_transport(&self, conn: PeerConn) -> tokio::task::JoinHandle<()> {
        let state = self.state.clone();
        tokio::spawn(async move {
            // WS 载体无 ConnCloser：主动撤销走 `PeerConn::close`（发 Close 帧）。
            if let Err(err) = serve_transport(state, conn, None).await {
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

    let state = build_hub_state(hub_id.clone(), router, options);

    let mut tasks = vec![
        spawn_accept_loop(endpoint.clone(), state.clone()),
        spawn_consume_loop(state.clone()),
    ];
    if let Some(task) = spawn_route_renewal(state.clone()) {
        tasks.push(task);
    }
    if let Some(task) = spawn_delivery_prune(state.clone()) {
        tasks.push(task);
    }
    if let Some(task) = spawn_reclaim_patrol(state.clone()) {
        tasks.push(task);
    }

    Ok(MqHub {
        hub_id,
        endpoint,
        handle: HubHandle { state },
        tasks,
    })
}

/// 把 `MqHubOptions` 的每个字段搬进共享状态；`cert_dir` 只用于建 endpoint。
fn build_hub_state(hub_id: String, router: MqRouter, options: MqHubOptions) -> Arc<HubState> {
    let MqHubOptions {
        offline,
        storage,
        mailbox,
        dedup_cap,
        membership,
        reclaim_idle_ms,
        push_hook,
        policy,
        auth,
        cert_dir: _,
        limits,
    } = options;
    Arc::new(HubState {
        peers: Arc::new(RwLock::new(HashMap::new())),
        next_conn: std::sync::atomic::AtomicU64::new(1),
        router,
        offline,
        storage,
        mailbox,
        seen: Arc::new(Mutex::new(MsgIdLru::new(dedup_cap))),
        reclaim_idle_ms,
        membership,
        push_hook,
        policy,
        auth,
        inflight: Arc::new(InflightLedger::with_cap(limits.max_inflight_per_conn)),
        limits,
        user_rate: Arc::new(RwLock::new(HashMap::new())),
        subscriptions: Arc::new(RwLock::new(HashMap::new())),
        route_generations: Arc::new(RwLock::new(HashMap::new())),
        conn_devices: Arc::new(RwLock::new(HashMap::new())),
        conn_closers: Arc::new(RwLock::new(HashMap::new())),
        metrics: crate::metrics::HubMetrics::new(hub_id.clone()),
        hub_id,
    })
}

/// QUIC accept loop：每个进来的连接单独起一个 `serve_conn` 任务。
fn spawn_accept_loop(endpoint: quinn::Endpoint, state: Arc<HubState>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let Some(incoming) = endpoint.accept().await else {
                break;
            };
            let Ok(conn) = incoming.await else { continue };
            let state = state.clone();
            tokio::spawn(async move {
                if let Err(err) = serve_conn(conn, state).await {
                    eprintln!("[mq-hub] conn error: {err:#}");
                }
            });
        }
    })
}

/// queue consume loop：消费其他 hub 路由到本 hub 的帧。
fn spawn_consume_loop(state: Arc<HubState>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let queue = hub_queue(&state.hub_id);
        match state.router.reliable.clone() {
            Some(rq) => consume_reliable(state, rq, queue).await,
            None => consume_memory(state, queue).await,
        }
    })
}

/// at-least-once 后端：**投递成功才 ack**（issue #9），失败留给 patrol 重领。
async fn consume_reliable(
    state: Arc<HubState>,
    rq: Arc<dyn wxlike_mq::ReliableQueue>,
    queue: String,
) {
    loop {
        match rq.consume_entry(&queue).await {
            Ok((id, bytes)) => deliver_and_ack(&state, &rq, &queue, id, bytes).await,
            Err(err) => {
                eprintln!("[mq-hub] stream consume error: {err:#}");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

async fn deliver_and_ack(
    state: &Arc<HubState>,
    rq: &Arc<dyn wxlike_mq::ReliableQueue>,
    queue: &str,
    id: String,
    bytes: Vec<u8>,
) {
    match deliver_bytes(state, &bytes).await {
        Ok(()) => {
            if let Err(err) = rq.ack(queue, &id).await {
                eprintln!("[mq-hub] ack {id} failed: {err:#}");
            }
        }
        Err(err) => {
            eprintln!("[mq-hub] deliver {id} failed, leaving it for reclaim: {err:#}");
            // 不 ack：等 lease 到期由 patrol 重新领走
        }
    }
}

/// 无 ack 能力的 memory 后端（仅测试/单机）：投递失败只能留日志。
async fn consume_memory(state: Arc<HubState>, queue: String) {
    loop {
        match state.router.mq.consume(&queue).await {
            Ok(bytes) => {
                if let Err(err) = deliver_bytes(&state, &bytes).await {
                    eprintln!("[mq-hub] deliver failed (no-ack backend): {err:#}");
                }
            }
            Err(err) => {
                eprintln!("[mq-hub] consume error: {err:#}");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

/// patrol: reclaim entries a dead consumer never acknowledged.
fn spawn_reclaim_patrol(state: Arc<HubState>) -> Option<JoinHandle<()>> {
    let (rq, idle_ms) = (state.router.reliable.clone()?, state.reclaim_idle_ms?);
    let queue = hub_queue(&state.hub_id);
    let period = std::time::Duration::from_millis((idle_ms / 2).max(100));
    Some(tokio::spawn(async move {
        loop {
            tokio::time::sleep(period).await;
            if let Err(err) = reclaim_stale(&state, &rq, &queue, idle_ms).await {
                eprintln!("[mq-hub] reclaim failed: {err:#}");
            }
        }
    }))
}

async fn reclaim_stale(
    state: &Arc<HubState>,
    rq: &Arc<dyn wxlike_mq::ReliableQueue>,
    queue: &str,
    idle_ms: u64,
) -> anyhow::Result<()> {
    for (id, bytes) in rq.claim_stale(queue, idle_ms).await? {
        if deliver_bytes(state, &bytes).await.is_ok() {
            if let Err(err) = rq.ack(queue, &id).await {
                eprintln!("[mq-hub] ack reclaimed {id} failed: {err:#}");
            }
        }
    }
    Ok(())
}

/// 路由租约续租（issue #20）。只有带租约的路由表才需要：内存表
/// `lease_ttl_ms() == 0`，起这个循环纯属空转。
///
/// 没有它，一台崩溃的 hub 会让**所有**曾经连过的用户永远路由到它身上——
/// 消息发进黑洞。有了租约，续租停止 = 路由过期 = 调用方重路由或落离线队列。
fn spawn_route_renewal(state: Arc<HubState>) -> Option<JoinHandle<()>> {
    let lease_ms = state.router.routes.lease_ttl_ms();
    if lease_ms == 0 {
        return None;
    }
    let routes = state.router.routes.clone();
    let peers = state.peers.clone();
    let gens = state.route_generations.clone();
    let hub_id = state.hub_id.clone();
    let period = std::time::Duration::from_millis((lease_ms / 3).max(1));
    Some(tokio::spawn(async move {
        loop {
            tokio::time::sleep(period).await;
            renew_routes_once(&routes, &peers, &gens, &hub_id).await;
        }
    }))
}

/// 给本 hub 上还绑着的每个用户续租一次。`peers` 是权威：连接断开的用户
/// 已经不在表里，续租循环不会给他们续命。
async fn renew_routes_once(
    routes: &Arc<dyn wxlike_mq::RouteTable>,
    peers: &Peers,
    gens: &Arc<RwLock<HashMap<String, u64>>>,
    hub_id: &str,
) {
    let users: Vec<String> = peers.read().await.keys().cloned().collect();
    for user in users {
        renew_one_route(routes, gens, hub_id, &user).await;
    }
}

async fn renew_one_route(
    routes: &Arc<dyn wxlike_mq::RouteTable>,
    gens: &Arc<RwLock<HashMap<String, u64>>>,
    hub_id: &str,
    user: &str,
) {
    // With generations the renew is a CAS: a hub that lost the row
    // (its connection was replaced) must not keep it alive.
    let gen = gens.read().await.get(user).copied().unwrap_or(0);
    let renewed = if gen == 0 {
        routes.renew_user(user, hub_id).await.map(|_| true)
    } else {
        routes.renew_route(user, hub_id, gen).await
    };
    match renewed {
        Ok(true) => {}
        Ok(false) => {
            gens.write().await.remove(user);
            tracing::debug!(hub = %hub_id, user, "route lease renew refused: no longer ours");
        }
        Err(err) => eprintln!("[mq-hub] renew route {user} failed: {err:#}"),
    }
}

/// 投递任务的清理（issue #11）：任务行只在"这条消息可能还要补投"时有意义。
/// 留一周足够覆盖客户端重试窗口，再老的删掉——否则表只增不减。
fn spawn_delivery_prune(state: Arc<HubState>) -> Option<JoinHandle<()>> {
    let store = state.storage.clone()?;
    let period = DELIVERY_RETENTION_MS as u64 / 4;
    Some(tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(period)).await;
            match store.prune_deliveries(DELIVERY_RETENTION_MS) {
                Ok(0) => {}
                Ok(n) => tracing::debug!(rows = n, "pruned delivery outbox"),
                Err(err) => eprintln!("[mq-hub] prune delivery outbox failed: {err:#}"),
            }
        }
    }))
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
    serve_transport(
        state,
        PeerConn::quic(send, recv),
        Some(ConnCloser::Quic(conn)),
    )
    .await
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

/// 连接握手产物：凭据、设备认领、协议世代。
struct Hello {
    token: String,
    device_id: String,
    negotiated: Negotiated,
}

/// 一条已认证连接的身份切片（只借用，不复制字符串）。
struct Session<'a> {
    user_id: &'a str,
    token: &'a str,
    device_id: &'a str,
    negotiated: Negotiated,
}

/// 读首帧并协商协议。拒绝路径统一先发原因再断开。
async fn accept_hello(state: &Arc<HubState>, conn: &PeerConn) -> anyhow::Result<Hello> {
    // 首帧必须是 Hello{token}——**客户端不自报身份**（issue #3 / A01）。
    // v3 多一个前置 `ClientHello{device_id}`：设备认领自己，身份仍然只从凭据解析。
    let first = conn.read_frame().await.context("await hello")?;
    if first.len() > state.limits.max_frame_bytes {
        state.metrics.limit_dropped("max_frame_bytes");
        reject_hello(conn, "handshake frame too large").await;
        anyhow::bail!("hello frame {} bytes over the ceiling", first.len());
    }
    let mut negotiated = Negotiated::V2;
    let mut device_id = String::new();
    let token = match WireFrame::from_bytes(&first)? {
        WireFrame::ClientHello {
            device_id: dev,
            protocol,
            caps,
        } => {
            negotiated = match Negotiated::negotiate(protocol, caps) {
                Ok(n) => n,
                Err(declared) => {
                    // 封闭失败路径：先给原因，再断，不做"降级放行"
                    reject_hello(conn, &format!("unsupported protocol {declared}")).await;
                    anyhow::bail!("peer declared protocol {declared}, cannot serve");
                }
            };
            device_id = dev;
            let next = conn.read_frame().await.context("await credential")?;
            match WireFrame::from_bytes(&next)? {
                WireFrame::Hello { token } => token,
                _ => anyhow::bail!("ClientHello must be followed by a Hello credential"),
            }
        }
        WireFrame::Hello { token } => token,
        _ => anyhow::bail!("first frame was not Hello"),
    };
    Ok(Hello {
        token,
        device_id,
        negotiated,
    })
}

/// fail closed：没有认证器就拒绝。**不要**在这里"为了兼容"放行——
/// 一个不校验身份的 hub，等于把"知道 user_id 就能冒充"变成协议特性。
async fn authenticate_hello(
    state: &Arc<HubState>,
    conn: &PeerConn,
    hello: &Hello,
) -> anyhow::Result<crate::auth::HubIdentity> {
    let Some(auth) = state.auth.as_ref() else {
        tracing::error!(
            hub = %state.hub_id,
            "hub has no authenticator configured; refusing every connection"
        );
        reject_hello(conn, "hub is not accepting connections (no authenticator)").await;
        anyhow::bail!("hub is not accepting connections (no authenticator)");
    };
    let Some(identity) = auth.authenticate(&hello.token) else {
        tracing::warn!(hub = %state.hub_id, "hello rejected: invalid or expired credential");
        reject_hello(conn, "invalid or expired credential").await;
        anyhow::bail!("invalid credential");
    };
    let user_id = identity.user_id.clone();

    // 设备数上限（G03-04 要求 3）。判断依据是在册连接数，不是启发式：
    // peers 就是权威表，满了还放会把单个用户的写放大成 DoS 面。
    {
        let peers = state.peers.read().await;
        let held = peers.get(&user_id).map(|m| m.len()).unwrap_or(0);
        if held >= state.limits.max_conns_per_user {
            state.metrics.limit_dropped("max_conns_per_user");
            reject_hello(conn, "too many live connections for this account").await;
            anyhow::bail!("connection ceiling for {user_id}: {held}");
        }
    }

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
    if hello.negotiated.version >= crate::proto::PROTOCOL_V3 {
        // v3 的第二张回执：这条连接认领的是哪个设备、哪个协议世代。
        conn.send_frame(
            &WireFrame::Auth {
                user_id: user_id.clone(),
                device_id: hello.device_id.clone(),
                session_id: identity.session_id.clone(),
                version: hello.negotiated.version,
            }
            .to_bytes()?,
        )
        .await
        .context("send v3 auth")?;
    }
    state.metrics.protocol_negotiated(hello.negotiated.version);
    Ok(identity)
}

/// 把连接登记进 peers / 设备表 / closer 表，并绑定带世代的路由。
async fn register_connection(
    state: &Arc<HubState>,
    conn: &PeerConn,
    conn_id: ConnId,
    user_id: &str,
    device_id: &str,
    closer: Option<ConnCloser>,
) -> anyhow::Result<u64> {
    state
        .peers
        .write()
        .await
        .entry(user_id.to_string())
        .or_default()
        .insert(conn_id, conn.clone());
    if !device_id.is_empty() {
        state
            .conn_devices
            .write()
            .await
            .insert(conn_id, device_id.to_string());
    }
    if let Some(c) = closer {
        state.conn_closers.write().await.insert(conn_id, c);
    }
    state.metrics.connected();
    state
        .user_rate
        .write()
        .await
        .entry(user_id.to_string())
        .or_insert_with(|| {
            TokenBucket::new(
                state.limits.max_msgs_per_user_per_min,
                state.limits.max_msgs_per_user_per_min,
                wxlike_core::types::now_ms(),
            )
        });

    // 路由带世代绑定（G03-04 要求 1）。不支持世代的表返回 0，走原来的
    // `unbind_user` 路径；支持的表（内存/Redis）在断连时做 compare-and-clear，
    // 一条迟到的旧连接就删不掉新连接刚写下的路由。
    let generation = if state.router.routes.generations() {
        state
            .router
            .routes
            .bind_route(user_id, &state.hub_id)
            .await
            .context("bind user route")?
    } else {
        state
            .router
            .routes
            .bind_user(user_id, &state.hub_id)
            .await
            .context("bind user route")?;
        0
    };
    if generation != 0 {
        state
            .route_generations
            .write()
            .await
            .insert(user_id.to_string(), generation);
    }
    Ok(generation)
}

/// 订阅只记账，授权判定在 `handle_inbound` 的成员/策略检查里。
async fn handle_subscribe(
    state: &Arc<HubState>,
    conn_id: ConnId,
    user_id: &str,
    scopes: &[String],
) -> anyhow::Result<()> {
    let mut subs = state.subscriptions.write().await;
    let held = subs.entry(conn_id).or_insert(0usize);
    if *held + scopes.len() > state.limits.max_subscriptions_per_conn {
        state.metrics.limit_dropped("max_subscriptions_per_conn");
        anyhow::bail!("subscription ceiling exceeded");
    }
    *held += scopes.len();
    drop(subs);
    tracing::debug!(hub = %state.hub_id, user = %user_id, conn = conn_id, scopes = scopes.len(), "v3 subscribe");
    Ok(())
}

/// 客户端确认落库：退休 inflight、清离线行、推进设备信箱游标，再补发队列。
async fn handle_stored_ack(
    state: &Arc<HubState>,
    conn: &PeerConn,
    session: &Session<'_>,
    conn_id: ConnId,
    conv_id: &str,
    stored_cursor: u64,
) -> anyhow::Result<()> {
    let retired = state.inflight.confirm(conn_id, conv_id, stored_cursor);
    if retired.is_empty() {
        return Ok(());
    }
    state.metrics.inflight_unacked(-(retired.len() as i64));
    for p in &retired {
        if let Err(err) = state.offline.ack(p.store_id).await {
            eprintln!("[mq-hub] stored-ack retire {} failed: {err:#}", p.store_id);
        }
    }
    // 信箱侧的设备游标：这条设备的"已落库"进度，与账号已读无关
    if let Some(mailbox) = &state.mailbox {
        if let Err(err) =
            mailbox.stored_ack(session.user_id, session.device_id, conv_id, stored_cursor)
        {
            eprintln!("[mq-hub] stored_ack {conv_id}@{stored_cursor} failed: {err:#}");
        }
    }
    // 窗口腾出来之后把还在队列里的补上（否则要等下一次重连）
    let flushed = flush_parked(
        state,
        conn,
        conn_id,
        session.user_id,
        session.device_id,
        session.negotiated,
    )
    .await;
    tracing::trace!(hub = %state.hub_id, user = %session.user_id, conv = %conv_id, stored_cursor, retired = retired.len(), flushed, "stored ack");
    Ok(())
}

/// v3 断线续传：先补发离线队列，再按设备游标回一个缺口提示。
async fn handle_resume(
    state: &Arc<HubState>,
    conn: &PeerConn,
    session: &Session<'_>,
    conn_id: ConnId,
    conv_id: &str,
    cursor: u64,
) -> anyhow::Result<()> {
    let flushed = flush_parked(
        state,
        conn,
        conn_id,
        session.user_id,
        session.device_id,
        session.negotiated,
    )
    .await;
    let hint = state.mailbox.as_ref().and_then(|m| {
        m.sync_hint(conv_id, session.user_id, session.device_id, cursor)
            .ok()
            .flatten()
    });
    if let Some((from_seq, to_seq, gap)) = hint {
        conn.send_frame(
            &WireFrame::Event {
                conv_id: conv_id.to_string(),
                from_seq,
                to_seq,
                gap,
            }
            .to_bytes()?,
        )
        .await
        .context("send event hint")?;
    }
    tracing::debug!(hub = %state.hub_id, user = %session.user_id, conv = %conv_id, cursor, flushed, "v3 resume");
    Ok(())
}

/// 处理控制帧；`Ok(Some(env))` 才是需要继续路由的内容帧。
async fn dispatch_control(
    state: &Arc<HubState>,
    conn: &PeerConn,
    session: &Session<'_>,
    conn_id: ConnId,
    wire: WireFrame,
) -> anyhow::Result<Option<Envelope>> {
    match wire {
        WireFrame::Msg(env) => {
            // 用户级速率上限（G03-04 要求 3）：超了丢帧并计量，
            // 不把连接挂起（挂起等于让攻击者占用任务）。
            let allowed = {
                let mut rates = state.user_rate.write().await;
                match rates.get_mut(session.user_id) {
                    Some(bucket) => bucket.spend(wxlike_core::types::now_ms(), 1),
                    None => true,
                }
            };
            if !allowed {
                state.metrics.limit_dropped("max_msgs_per_user_per_min");
                tracing::warn!(hub = %state.hub_id, user = %session.user_id, "inbound rate ceiling hit");
                return Ok(None);
            }
            Ok(Some(env))
        }
        WireFrame::Ping => Ok(None),
        WireFrame::Hello { .. }
        | WireFrame::HelloAck { .. }
        | WireFrame::HelloReject { .. }
        | WireFrame::ClientHello { .. } => {
            // 重复握手帧：忽略。身份只在首帧建立，再声明一次不改变任何事实
            Ok(None)
        }
        WireFrame::Subscribe { scopes } => {
            handle_subscribe(state, conn_id, session.user_id, &scopes).await?;
            Ok(None)
        }
        WireFrame::Ack {
            conv_id,
            stored_cursor,
        } => {
            handle_stored_ack(state, conn, session, conn_id, &conv_id, stored_cursor).await?;
            Ok(None)
        }
        WireFrame::Resume { conv_id, cursor } => {
            handle_resume(state, conn, session, conn_id, &conv_id, cursor).await?;
            Ok(None)
        }
        WireFrame::Event { .. } | WireFrame::Auth { .. } => {
            // 服务端下行帧的形状：客户端不得冒充 hub 广播范围提示
            tracing::warn!(hub = %state.hub_id, user = %session.user_id, "server-only control frame from client; ignored");
            Ok(None)
        }
    }
}

async fn serve_transport(
    state: Arc<HubState>,
    conn: PeerConn,
    closer: Option<ConnCloser>,
) -> anyhow::Result<()> {
    let conn_id = state
        .next_conn
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let hello = accept_hello(&state, &conn).await?;
    let identity = authenticate_hello(&state, &conn, &hello).await?;
    let user_id = identity.user_id.clone();
    let generation =
        register_connection(&state, &conn, conn_id, &user_id, &hello.device_id, closer).await?;

    let session = Session {
        user_id: &user_id,
        token: &hello.token,
        device_id: &hello.device_id,
        negotiated: hello.negotiated,
    };
    flush_parked(
        &state,
        &conn,
        conn_id,
        session.user_id,
        session.device_id,
        session.negotiated,
    )
    .await;

    let res = pump_frames(&state, &conn, &session, conn_id).await;
    release_conn(&state, session.user_id, conn_id, generation).await;
    res
}

/// 入站读取结果：正常帧、对端正常断开。
enum Incoming {
    Frame(Vec<u8>),
    End,
}

async fn next_incoming(state: &Arc<HubState>, conn: &PeerConn) -> anyhow::Result<Incoming> {
    let frame = match conn.read_frame().await {
        Ok(f) => f,
        Err(err) => {
            if is_disconnect(&err) {
                return Ok(Incoming::End);
            }
            return Err(err);
        }
    };
    if frame.len() > state.limits.max_frame_bytes {
        state.metrics.limit_dropped("max_frame_bytes");
        anyhow::bail!("inbound frame over the configured ceiling");
    }
    Ok(Incoming::Frame(frame))
}

/// 解码并做 v3 控制帧的世代校验；坏帧返回 `None`（静默跳过）。
fn decode_wire(session: &Session<'_>, frame: &[u8]) -> anyhow::Result<Option<WireFrame>> {
    let Ok(wire) = WireFrame::from_bytes(frame) else {
        return Ok(None);
    };
    // v3 控制帧只属于已经协商出 v3 的连接；v2 对端发来它们说明实现有 bug，
    // 静默忽略会把协议错误变成长期兼容层——直接断。
    if session.negotiated.version < crate::proto::PROTOCOL_V3 && crate::proto::is_v3_control(&wire)
    {
        anyhow::bail!("v3 control frame on a v2 connection; closing");
    }
    Ok(Some(wire))
}

/// 心跳与每条入站帧都复查凭据：HTTP 那边退出登录/远程下线之后，
/// 这条 QUIC/WS 连接最多只能再活一帧的时间（原来是一个心跳周期）。
fn ensure_not_revoked(state: &Arc<HubState>, session: &Session<'_>) -> anyhow::Result<()> {
    if state
        .auth
        .as_ref()
        .is_some_and(|a| a.is_revoked(session.token))
    {
        tracing::info!(hub = %state.hub_id, user = %session.user_id, "credential revoked; closing long connection");
        anyhow::bail!("credential revoked");
    }
    Ok(())
}

/// 入站主循环：读帧 → 校验 → 控制帧就地处理 / 内容帧交给 `handle_inbound`。
async fn pump_frames(
    state: &Arc<HubState>,
    conn: &PeerConn,
    session: &Session<'_>,
    conn_id: ConnId,
) -> anyhow::Result<()> {
    loop {
        let frame = match next_incoming(state, conn).await? {
            Incoming::Frame(frame) => frame,
            Incoming::End => break Ok(()),
        };
        let Some(wire) = decode_wire(session, &frame)? else {
            continue;
        };
        ensure_not_revoked(state, session)?;
        let Some(env) = dispatch_control(state, conn, session, conn_id, wire).await? else {
            continue;
        };
        let started = std::time::Instant::now();
        let outcome = handle_inbound(state, session.user_id, &env).await;
        state.metrics.message_routed();
        state.metrics.delivery_latency(started.elapsed());
        tracing::trace!(
            hub = %state.hub_id,
            from = %session.user_id,
            conv = %env.conv_id,
            elapsed_us = started.elapsed().as_micros() as u64,
            "envelope routed"
        );
        if let Err(err) = outcome {
            eprintln!("[mq-hub] inbound error: {err:#}");
        }
    }
}

/// 一条连接结束时把它从 hub 摘掉（ADR-4 多设备：只摘自己，用户最后一条连接
/// 消失才解绑路由）。**幂等**：主动撤销（[`HubHandle::disconnect_user`]）与
/// 对端自然断开可能各调一次，重复调用必须是 no-op。
async fn release_conn(state: &Arc<HubState>, user_id: &str, conn_id: ConnId, generation: u64) {
    // 先清主动撤销侧的登记：`ConnCloser::Quic` 持有整个 `quinn::Connection`，
    // 不在这里 drop 掉的话，连接对端永远收不到关闭（自然断开路径同样要走）。
    state.conn_closers.write().await.remove(&conn_id);
    state.conn_devices.write().await.remove(&conn_id);
    let mut keep_bound = false;
    {
        let mut peers = state.peers.write().await;
        if let Some(conns) = peers.get_mut(user_id) {
            conns.remove(&conn_id);
            if conns.is_empty() {
                peers.remove(user_id);
            } else {
                keep_bound = true;
            }
        }
    }
    state.subscriptions.write().await.remove(&conn_id);
    if !keep_bound {
        state.user_rate.write().await.remove(user_id);
        if generation != 0 {
            state.route_generations.write().await.remove(user_id);
        }
    }
    // G03-02：这条连接写了但没被确认落库的帧，全部放回离线队列。
    // 不释放就等于"已经交出去但没人收到"，正是本卡要避免的丢失窗口。
    let owed = state.inflight.discard(conn_id);
    if !owed.is_empty() {
        state.metrics.inflight_unacked(-(owed.len() as i64));
        for p in &owed {
            if let Err(err) = state.offline.release(p.store_id).await {
                eprintln!(
                    "[mq-hub] release on disconnect {} failed: {err:#}",
                    p.store_id
                );
            }
        }
        tracing::info!(hub = %state.hub_id, user = %user_id, conn = %conn_id, count = owed.len(), "unacked writes requeued");
    }
    state.metrics.disconnected();
    if keep_bound {
        tracing::debug!(hub = %state.hub_id, user = %user_id, conn = %conn_id, "conn unbound (devices remain)");
    } else {
        tracing::debug!(hub = %state.hub_id, user = %user_id, "user unbound");
        let cleared = if generation == 0 {
            let _ = state
                .router
                .routes
                .unbind_user(user_id, &state.hub_id)
                .await;
            true
        } else {
            // compare-and-clear：别的连接已经接手时，这里什么都不做
            state
                .router
                .routes
                .clear_route(user_id, &state.hub_id, generation)
                .await
                .unwrap_or(false)
        };
        if generation != 0 && !cleared {
            tracing::info!(hub = %state.hub_id, user = %user_id, "stale route unlink refused: a newer generation holds it");
        }
    }
}

/// 主动断开一条连接（G02-09 设备撤销的进程内效果）。先清自己登记的两张表
/// （`conn_devices`/`conn_closers`），再断链：QUIC 用 `Connection::close`（立即
/// 断开、所有流 reset），WS 载体无句柄则走 [`PeerConn::close`]（发 Close 帧）。
/// 最后调 [`release_conn`] 收尾，与对端自然断开走同一套逻辑。
///
/// 返回该连接是否真的存在过（不存在 = 幂等命中 / 已经断开了）。
async fn disconnect_conn(state: &Arc<HubState>, user_id: &str, conn_id: ConnId) -> bool {
    let closer = state.conn_closers.write().await.remove(&conn_id);
    state.conn_devices.write().await.remove(&conn_id);
    let peer = {
        let mut peers = state.peers.write().await;
        peers.get_mut(user_id).and_then(|m| m.remove(&conn_id))
    };
    let existed = peer.is_some();
    match closer {
        Some(ConnCloser::Quic(conn)) => {
            conn.close(0u32.into(), b"revoked by administrator");
        }
        None => {
            if let Some(p) = peer.as_ref() {
                p.close().await;
            }
        }
    }
    let generation = state
        .route_generations
        .read()
        .await
        .get(user_id)
        .copied()
        .unwrap_or(0);
    release_conn(state, user_id, conn_id, generation).await;
    existed
}

/// Parse a parked frame into the ledger key: which conversation, which seq.
///
/// A frame we cannot parse is still tracked (`Pending::opaque`), but it can only
/// be retired by the store lease — a cursor ack must never silently credit it.
fn pending_of(store_id: i64, frame: &[u8]) -> Pending {
    match WireFrame::from_bytes(frame) {
        Ok(WireFrame::Msg(env)) => Pending::sequenced(store_id, env.conv_id, env.seq),
        _ => Pending::opaque(store_id),
    }
}

/// Flush what is parked for `user` on this connection.
///
/// **v2**: a frame leaves the offline store when the write succeeded (the old
/// behaviour, kept for peers that cannot ack). **v3 with `StoredAck`**: the row
/// stays until the client sends `Ack{stored_cursor}`; a write only moves it into
/// the connection's bounded in-flight window.
///
/// Frames are claimed in batches and **never drained in one transaction**
/// (issue #10): taking everything out before sending is what lost the tail of a
/// half-finished flush.
async fn flush_parked(
    state: &Arc<HubState>,
    conn: &PeerConn,
    conn_id: ConnId,
    user_id: &str,
    device_id: &str,
    negotiated: Negotiated,
) -> usize {
    const CLAIM_BATCH: usize = 64;
    let mut total = 0usize;
    loop {
        let batch = match state.offline.claim(user_id, CLAIM_BATCH).await {
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
            device = %device_id,
            count = batch.len(),
            stored_ack = negotiated.stored_ack,
            "flushing parked messages"
        );
        let delivered = flush_batch(state, conn, conn_id, user_id, &batch, negotiated).await;
        state.metrics.offline_parked(-(delivered as i64));
        total += delivered;
        if delivered < batch.len() {
            // the connection is gone; the rest waits for the next Hello
            break;
        }
    }
    total
}

/// Write one claimed batch, retiring or tracking each row as it goes.
///
/// Returns how many rows were written. Anything from the first failed write on
/// is released immediately: it stays leased for 30 s otherwise, for nothing.
async fn flush_batch(
    state: &Arc<HubState>,
    conn: &PeerConn,
    conn_id: ConnId,
    user_id: &str,
    batch: &[(i64, Vec<u8>)],
    negotiated: Negotiated,
) -> usize {
    let mut delivered = 0usize;
    for (idx, (id, frame)) in batch.iter().enumerate() {
        // AR-6（#30）：过期的秘密会话帧不再补发，ack 即丢弃（与 HTTP 检索面的
        // TTL 过滤同口径）。QUIC 与 WS 都走这一条补发路径，一处判定即全覆盖。
        if expired_secret_frame(state.policy.as_deref(), frame).await {
            drop_parked(state, *id, user_id, "expired secret-chat frame").await;
            continue;
        }
        if conn.send_frame(frame).await.is_err() {
            release_from(state, batch, idx).await;
            eprintln!("[mq-hub] parked deliver to {user_id} failed: socket write");
            return delivered;
        }
        delivered += 1;
        track_written(state, conn_id, *id, frame, negotiated).await;
    }
    delivered
}

/// Put an unwritten tail back on the queue.
///
/// The rest of the batch is unreachable once one write fails: releasing it now is
/// what keeps a dead socket from holding 64 rows leased for 30 seconds.
async fn release_from(state: &Arc<HubState>, batch: &[(i64, Vec<u8>)], from: usize) {
    for (id, _) in batch.iter().skip(from) {
        if let Err(e) = state.offline.release(*id).await {
            eprintln!("[mq-hub] offline release {id} failed: {e:#}");
        }
    }
}

/// A write succeeded: either hand the row to the in-flight window (v3) or retire
/// it on the spot (v2, the pre-stored-ack behaviour).
async fn track_written(
    state: &Arc<HubState>,
    conn_id: ConnId,
    id: i64,
    frame: &[u8],
    negotiated: Negotiated,
) {
    if !negotiated.stored_ack {
        // ack 失败不该让已经发出的条目再发一遍；下一轮 claim 会因为 lease 未过期
        // 而跳过它们，最坏是重复一次。
        if let Err(err) = state.offline.ack(id).await {
            eprintln!("[mq-hub] offline ack {id} failed: {err:#}");
        }
        return;
    }
    state.metrics.inflight_unacked(1);
    let overflow = state.inflight.record(conn_id, pending_of(id, frame));
    if overflow.is_empty() {
        return;
    }
    // the client stopped acking: put the oldest rows back rather than dropping
    state.metrics.inflight_unacked(-(overflow.len() as i64));
    state.metrics.limit_dropped("max_inflight_per_conn");
    for p in &overflow {
        let _ = state.offline.release(p.store_id).await;
    }
    tracing::warn!(hub = %state.hub_id, conn = conn_id, count = overflow.len(), "inflight window overflow: oldest requeued");
}

/// Drop a parked row for a reason that is not a client acknowledgement.
async fn drop_parked(state: &Arc<HubState>, id: i64, user_id: &str, why: &str) {
    if let Err(err) = state.offline.ack(id).await {
        eprintln!("[mq-hub] offline ack {id} ({why}) failed: {err:#}");
    }
    tracing::info!(hub = %state.hub_id, user = %user_id, id, why, "dropped parked frame");
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
#[derive(Debug)]
pub struct QuoteReplayConflict;

impl std::fmt::Display for QuoteReplayConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("idempotency key reused with different parameters")
    }
}

impl std::error::Error for QuoteReplayConflict {}

fn check_quoted_replay(store: &Store, env: &Envelope) -> anyhow::Result<()> {
    let key = env.msg_id.to_vec();
    let stored = store
        .by_msg_ids(std::slice::from_ref(&key))?
        .remove(&key)
        .context("persisted message missing during replay check")?;
    let prev = stored.envelope;
    if (prev.quote_of.is_some() || env.quote_of.is_some())
        && (prev.sender != env.sender
            || prev.conv_id != env.conv_id
            || prev.body != env.body
            || prev.quote_of != env.quote_of)
    {
        return Err(QuoteReplayConflict.into());
    }
    Ok(())
}

async fn handle_inbound(
    state: &Arc<HubState>,
    bound_user: &str,
    env: &Envelope,
) -> anyhow::Result<()> {
    if !sender_allowed(bound_user, env) {
        return Ok(());
    }
    let members = resolve_members(state, env).await?;
    if !direct_shape_ok(env) || !broadcast_shape_ok(env, &members) {
        return Ok(());
    }
    let Some(mut env) = gate_content(state, env.clone(), &members).await? else {
        return Ok(());
    };

    // Transient signals (typing indicators) are the transitional form of the
    // P3 `high_priority` channel: routed to whoever is online right now,
    // never persisted, never parked — a user who connects later must not see
    // "对方正在输入" from an hour ago (contract §3.5).
    if env.is_typing() {
        return route_or_fanout(state, &env, members, false).await;
    }
    // Control frames (receipts, recalls) are never conversation content.
    if env.receipt.is_some() {
        return handle_receipt(state, &env).await;
    }
    if env.is_recall() {
        return handle_recall(state, &env, members).await;
    }
    handle_content(state, &mut env, members.as_deref()).await
}

/// 1 + 1.5：客户端只能以自己声明的身份说话，用户通道也不接受系统事件。
fn sender_allowed(bound_user: &str, env: &Envelope) -> bool {
    if env.sender != bound_user {
        eprintln!(
            "[mq-hub] dropped frame from {bound_user} claiming sender {}",
            env.sender
        );
        return false;
    }
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
        return false;
    }
    true
}

/// 2. 群成员名单：优先信箱快照，缺失时退回平台成员表（迁移期兜底）。
async fn resolve_members(
    state: &Arc<HubState>,
    env: &Envelope,
) -> anyhow::Result<Option<Vec<String>>> {
    let from_mailbox = match (&state.mailbox, env.conv_id.starts_with("d-")) {
        (Some(mailbox), false) => mailbox
            .members((wxlike_message::OwnerType::Group, &env.conv_id))
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    if !from_mailbox.is_empty() {
        return Ok(Some(from_mailbox));
    }
    match &state.membership {
        Some(membership) => membership.members_of(&env.conv_id).await,
        None => Ok(None),
    }
}

/// 2.2 直聊三元组：sender 必须在 `d-a~b` 里，内容帧的 `to` 必须是另一个人。
fn direct_shape_ok(env: &Envelope) -> bool {
    let Some((a, b)) = direct_members(&env.conv_id) else {
        return true;
    };
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
        return false;
    };
    if !is_user_control_frame(env) && (env.to.len() != 1 || env.to[0] != other) {
        eprintln!(
            "[mq-hub] dropped direct frame with recipients {:?} (conv {} expects [{other}])",
            env.to, env.conv_id
        );
        return false;
    }
    true
}

/// 2.3 用户通道不允许空收件人广播；群消息靠成员表扇出。
fn broadcast_shape_ok(env: &Envelope, members: &Option<Vec<String>>) -> bool {
    if !is_user_control_frame(env) && env.to.is_empty() && members.is_none() {
        eprintln!(
            "[mq-hub] dropped broadcast content frame from {} (conv {})",
            env.sender, env.conv_id
        );
        return false;
    }
    true
}

/// 2.5 P3 投递策略：send gate → 关系裁决 → 群成员复核。
/// 返回 `None` 表示丢弃（不落库、不暂存、不扇出）。
async fn gate_content(
    state: &Arc<HubState>,
    env: Envelope,
    members: &Option<Vec<String>>,
) -> anyhow::Result<Option<Envelope>> {
    let owned = if let Some(policy) = &state.policy {
        if env.is_recall()
            || env.receipt.is_some()
            || matches!(env.body, wxlike_core::MsgBody::Notice(_))
        {
            env
        } else {
            if !run_send_gate(state, policy.as_ref(), &env).await {
                return Ok(None);
            }
            match apply_delivery_policy(state, policy.as_ref(), env, members).await? {
                PolicyOutcome::Deliver(owned) => owned,
                PolicyOutcome::Drop => return Ok(None),
            }
        }
    } else {
        env
    };
    if let Some(members) = members {
        if !members.iter().any(|m| m == &owned.sender) {
            eprintln!(
                "[mq-hub] {} is not a member of group {}; frame dropped",
                owned.sender, owned.conv_id
            );
            return Ok(None);
        }
    }
    Ok(Some(owned))
}

/// P4 发送闸门（风控 + 内容审核）：先看"这个人此刻能不能发、这条内容能不能投"。
async fn run_send_gate(state: &Arc<HubState>, policy: &dyn DeliveryPolicy, env: &Envelope) -> bool {
    let text = match &env.body {
        wxlike_core::MsgBody::Text(t) => t.as_str(),
        _ => "",
    };
    let limiter = policy.rate_limiter_name();
    match policy.send_gate(&env.sender, &env.conv_id, text).await {
        crate::SendGate::Allow => true,
        // Synapse `should_sleep` 同型：先退避再继续投递。上限存在的
        // 理由是无上限 sleep 会把连接读循环交给策略方任意挂起。
        crate::SendGate::Sleep { delay_ms } if delay_ms <= crate::MAX_SEND_GATE_SLEEP_MS => {
            state.metrics.rate_limit_sleep(limiter);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            true
        }
        other => {
            state.metrics.rate_limit_reject(limiter);
            tracing::info!(
                hub = %state.hub_id,
                from = %env.sender,
                conv = %env.conv_id,
                gate = ?other,
                limiter,
                "send gate dropped frame"
            );
            false
        }
    }
}

/// 回执：只回原消息作者（`to` 仅作提示，去重后不再多通知一次）。
async fn handle_receipt(state: &Arc<HubState>, env: &Envelope) -> anyhow::Result<()> {
    if env.msg_id != NO_MSG_ID && state.seen.lock().await.check_and_insert(env.msg_id) {
        return Ok(());
    }
    let author = match (&state.storage, env.refers_to()) {
        (Some(store), Some(target)) => store.sender_of(&target)?,
        _ => None,
    };
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
    route_envelope(state, &directed, true).await
}

/// 撤回：先撤信箱副本，再让归档裁决（非作者/未知 id 直接丢弃）。
async fn handle_recall(
    state: &Arc<HubState>,
    env: &Envelope,
    members: Option<Vec<String>>,
) -> anyhow::Result<()> {
    if env.msg_id != NO_MSG_ID && state.seen.lock().await.check_and_insert(env.msg_id) {
        return Ok(());
    }
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
    route_or_fanout(state, env, members, true).await
}

/// 3. 内容帧：持久化 → 标记 seen → 登记投递 → 只投还没收到的人。
async fn handle_content(
    state: &Arc<HubState>,
    env: &mut Envelope,
    members: Option<&[String]>,
) -> anyhow::Result<()> {
    if env.msg_id != NO_MSG_ID {
        if state.seen.lock().await.contains(env.msg_id) {
            return replay_content(state, env, members).await;
        }
        persist_content(state, env).await?;
        // **落库完成之后**才标记：这样"落库失败 → 客户端重试"仍然能进来。
        state.seen.lock().await.insert(env.msg_id);
    }

    let recipients = content_recipients(env, members);
    // 没有 msg_id 的老帧（v1）没有投递任务可查：空串 = "不走 outbox"。
    let msg_key = if env.msg_id == NO_MSG_ID {
        String::new()
    } else {
        hex::encode(env.msg_id)
    };
    if let (Some(store), false) = (&state.storage, env.msg_id == NO_MSG_ID) {
        store.ensure_deliveries(&msg_key, &recipients)?;
    }
    fanout_content(state, env, &recipients, &msg_key).await
}

/// 已落库的重放：跳过 append/mirror，只补投上次失败的收件人。
async fn replay_content(
    state: &Arc<HubState>,
    env: &Envelope,
    members: Option<&[String]>,
) -> anyhow::Result<()> {
    let msg_key = hex::encode(env.msg_id);
    let recipients = content_recipients(env, members);
    if let Some(store) = &state.storage {
        check_quoted_replay(store, env)?;
        store.ensure_deliveries(&msg_key, &recipients)?;
    }
    tracing::debug!(
        hub = %state.hub_id,
        msg = %msg_key,
        "duplicate msg_id: persisted already, retrying undelivered recipients"
    );
    fanout_content(state, env, &recipients, &msg_key).await
}

/// 归档 + 信箱镜像。`append` 的唯一索引决定并发胜者；镜像失败不阻断投递。
async fn persist_content(state: &Arc<HubState>, env: &mut Envelope) -> anyhow::Result<()> {
    if let Some(store) = &state.storage {
        // 唯一索引决定并发胜者，冲突帧必须在镜像与投递前退出。
        if store.append(env, false)?.is_none() {
            check_quoted_replay(store, env)?;
        }
    }
    // 镜像进信箱（读扩散的唯一真相）。`d-` 前缀 = 直聊，其余按群处理；
    // 频道的消息不走这里（由 platform::send_channel_message 直接写信箱）。
    //
    // 顺带把**信箱序列号**写回 envelope：扇出到每个收件人的帧都带同一个
    // 会话内序号（1,2,3…），客户端因此有了服务端权威的排序，与未读、
    // ack、sync_plan 用的是同一套编号。之前 seq 是发送方填的（通常 0），
    // 客户端只能按到达顺序排——多端之间会不一致。
    if let Some(mailbox) = &state.mailbox {
        if let Some((type_id, payload)) = wxlike_message::mirror_of(env) {
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
    Ok(())
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
