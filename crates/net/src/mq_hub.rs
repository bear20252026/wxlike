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
}

/// Contract floor for reclaiming in-flight cross-node entries.
pub const DEFAULT_CLAIM_IDLE_MS: u64 = 60_000;

impl Default for MqHubOptions {
    fn default() -> Self {
        Self {
            offline: MemoryOffline::new(500),
            storage: None,
            dedup_cap: DEFAULT_DEDUP_CAP,
            membership: None,
            reclaim_idle_ms: None,
            push_hook: None,
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

impl Drop for MqHub {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
        self.endpoint.close(0u32.into(), b"hub shutdown");
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
    seen: Arc<tokio::sync::Mutex<MsgIdLru>>,
    reclaim_idle_ms: Option<u64>,
    membership: Option<Arc<dyn crate::membership::Membership>>,
    push_hook: Option<Arc<dyn OfflinePush>>,
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
pub async fn run_mq_hub(
    hub_id: impl Into<String>,
    addr: std::net::SocketAddr,
    router: MqRouter,
) -> anyhow::Result<MqHub> {
    run_mq_hub_with(hub_id, addr, router, MqHubOptions::default()).await
}

/// Same, with explicit options (offline store, message store, dedup cap).
pub async fn run_mq_hub_with(
    hub_id: impl Into<String>,
    addr: std::net::SocketAddr,
    router: MqRouter,
    options: MqHubOptions,
) -> anyhow::Result<MqHub> {
    let hub_id = hub_id.into();
    let endpoint = crate::transport::make_server_endpoint(addr)?;

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
        seen: Arc::new(Mutex::new(MsgIdLru::new(options.dedup_cap))),
        reclaim_idle_ms: options.reclaim_idle_ms,
        membership: options.membership.clone(),
        push_hook: options.push_hook.clone(),
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
                        deliver_bytes(&consume_state, &bytes).await;
                        if let Err(err) = rq.ack(&queue, &id).await {
                            eprintln!("[mq-hub] ack {id} failed: {err:#}");
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
                    Ok(bytes) => deliver_bytes(&consume_state, &bytes).await,
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
                                deliver_bytes(&patrol_state, &bytes).await;
                                if let Err(err) = rq.ack(&queue, &id).await {
                                    eprintln!("[mq-hub] ack reclaimed {id} failed: {err:#}");
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
    tasks.extend(patrol_loop);

    Ok(MqHub {
        hub_id,
        endpoint,
        handle: HubHandle { state },
        tasks,
    })
}

/// Decode and route one payload taken off the queue.
async fn deliver_bytes(state: &Arc<HubState>, bytes: &[u8]) {
    match WireFrame::from_bytes(bytes) {
        Ok(WireFrame::Msg(env)) => deliver_local(state, &env).await,
        Ok(_) => {}
        Err(err) => eprintln!("[mq-hub] bad frame from mq: {err}"),
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
async fn serve_transport(state: Arc<HubState>, conn: PeerConn) -> anyhow::Result<()> {
    // first frame must be Hello{user_id} — identical on every carrier
    let hello = conn.read_frame().await.context("await hello")?;
    let user_id = match WireFrame::from_bytes(&hello)? {
        WireFrame::Hello { user_id } => user_id,
        _ => anyhow::bail!("first frame was not Hello"),
    };

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

    // store-and-forward: flush everything parked while the user was away
    match state.offline.drain(&user_id).await {
        Ok(frames) => {
            if !frames.is_empty() {
                state.metrics.offline_parked(-(frames.len() as i64));
                tracing::info!(
                    hub = %state.hub_id,
                    user = %user_id,
                    count = frames.len(),
                    "flushing parked messages"
                );
                for frame in frames {
                    if let Err(err) = conn.send_frame(&frame).await {
                        eprintln!("[mq-hub] parked deliver to {user_id} failed: {err}");
                        break;
                    }
                }
            }
        }
        Err(err) => eprintln!("[mq-hub] offline drain failed: {err:#}"),
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
            WireFrame::Ping | WireFrame::Hello { .. } => continue,
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

    // 2. group scoping, before anything is persisted: a message from someone who
    // is not a member is dropped and never stored (WP-04 A4).
    let members = match &state.membership {
        Some(membership) => membership.members_of(&env.conv_id).await?,
        None => None,
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
        return route_or_fanout(state, env, members, false).await;
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
        return route_or_fanout(state, env, members, true).await;
    }

    // 3. content frames: dedup, then persist, then route
    if env.msg_id != NO_MSG_ID {
        let duplicate = state.seen.lock().await.check_and_insert(env.msg_id);
        if duplicate {
            return Ok(()); // re-delivery is not an error
        }
        if let Some(store) = &state.storage {
            if store.append(env, false)?.is_none() {
                return Ok(()); // already persisted (e.g. hub restart)
            }
        }
    }

    route_or_fanout(state, env, members, true).await
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
    let unique: Vec<&String> = recipients.iter().filter(|r| seen.insert(*r)).collect();

    let tasks = unique.iter().map(|rcpt| {
        let state = state.clone();
        let rcpt = (*rcpt).clone();
        let owned_env = env.clone();
        let frame = match WireFrame::Msg(mention_variant(env, &rcpt)).to_bytes() {
            Ok(f) => f,
            Err(err) => return tokio::spawn(async move { Err(err) }),
        };
        tokio::spawn(async move {
            match state.router.routes.route_of(&rcpt).await? {
                Some(h) if h == state.hub_id => {
                    deliver_to(&state, &rcpt, &frame).await;
                }
                Some(h) => state.router.mq.publish(&hub_queue(&h), frame).await?,
                None => {
                    if !park_offline {
                        // transient signal (typing): a user who is not online
                        // simply misses it; there is nothing to store
                        return Ok(());
                    }
                    // nowhere online: park for later, then wake their devices
                    state.offline.enqueue(&rcpt, frame).await?;
                    state.metrics.offline_parked(1);
                    tracing::debug!(
                        hub = %state.hub_id,
                        user = %rcpt,
                        conv = %owned_env.conv_id,
                        "recipient offline, frame parked"
                    );
                    if let Some(hook) = state.push_hook.clone() {
                        // fire-and-forget: push failure must not roll back the
                        // park, and must not delay the rest of the fan-out
                        tokio::spawn(async move {
                            hook.user_offline(&rcpt, &owned_env).await;
                        });
                    }
                }
            }
            Ok(())
        })
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
async fn deliver_to(state: &Arc<HubState>, user: &str, frame: &[u8]) {
    let conns: Vec<PeerConn> = {
        let map = state.peers.read().await;
        match map.get(user) {
            Some(c) => c.values().cloned().collect(),
            None => return,
        }
    };
    for conn in conns {
        let frame = frame.to_vec();
        tokio::spawn(async move {
            if let Err(err) = conn.send_frame(&frame).await {
                eprintln!("[mq-hub] deliver failed: {err}");
            }
        });
    }
}

/// Dedup cache occupancy, exposed for tests and metrics.
pub async fn dedup_len(handle: &HubHandle) -> usize {
    handle.state.seen.lock().await.len()
}
