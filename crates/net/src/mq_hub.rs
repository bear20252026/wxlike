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
use wxlike_core::types::{MsgId, NO_MSG_ID};
use wxlike_core::{Envelope, Store};
use wxlike_mq::{hub_queue, MqRouter};

use crate::framing;
use crate::offline::{MemoryOffline, OfflineStore};
use crate::wire::WireFrame;

/// Default dedup cache size required by the task book (>= 100k entries).
pub const DEFAULT_DEDUP_CAP: usize = 100_000;

/// Hub knobs. Defaults: in-memory offline inbox (cap 500), 100k dedup cache,
/// no message store (routing only).
#[derive(Clone)]
pub struct MqHubOptions {
    pub offline: Arc<dyn OfflineStore>,
    pub storage: Option<Arc<Store>>,
    pub dedup_cap: usize,
}

impl Default for MqHubOptions {
    fn default() -> Self {
        Self {
            offline: MemoryOffline::new(500),
            storage: None,
            dedup_cap: DEFAULT_DEDUP_CAP,
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

type SendStream = Arc<Mutex<quinn::SendStream>>;
type Peers = Arc<RwLock<HashMap<String, SendStream>>>;

/// Everything the routing tasks share.
struct HubState {
    hub_id: String,
    peers: Peers,
    router: MqRouter,
    offline: Arc<dyn OfflineStore>,
    storage: Option<Arc<Store>>,
    seen: Arc<Mutex<MsgIdLru>>,
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

    pub async fn publish(&self, env: &Envelope) -> anyhow::Result<()> {
        route_envelope(&self.state, env).await
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
        router,
        offline: options.offline.clone(),
        storage: options.storage.clone(),
        seen: Arc::new(Mutex::new(MsgIdLru::new(options.dedup_cap))),
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
        loop {
            match consume_state.router.mq.consume(&queue).await {
                Ok(bytes) => match WireFrame::from_bytes(&bytes) {
                    Ok(WireFrame::Msg(env)) => deliver_local(&consume_state, &env).await,
                    Ok(_) => {}
                    Err(err) => eprintln!("[mq-hub] bad frame from mq: {err}"),
                },
                Err(err) => {
                    eprintln!("[mq-hub] consume error: {err:#}");
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }
    });

    Ok(MqHub {
        hub_id,
        endpoint,
        handle: HubHandle { state },
        tasks: vec![accept_loop, consume_loop],
    })
}

async fn serve_conn(conn: quinn::Connection, state: Arc<HubState>) -> anyhow::Result<()> {
    let (send, mut recv) = conn.accept_bi().await.context("accept control stream")?;

    // first frame must be Hello{user_id}
    let hello = framing::read_frame(&mut recv)
        .await
        .context("await hello")?;
    let user_id = match WireFrame::from_bytes(&hello)? {
        WireFrame::Hello { user_id } => user_id,
        _ => anyhow::bail!("first frame was not Hello"),
    };

    let send = Arc::new(Mutex::new(send));
    state
        .peers
        .write()
        .await
        .insert(user_id.clone(), send.clone());

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
                eprintln!(
                    "[mq-hub] flushing {} parked message(s) to {user_id}",
                    frames.len()
                );
                let mut s = send.lock().await;
                for frame in frames {
                    if let Err(err) = framing::write_frame(&mut *s, &frame).await {
                        eprintln!("[mq-hub] parked deliver to {user_id} failed: {err}");
                        break;
                    }
                }
            }
        }
        Err(err) => eprintln!("[mq-hub] offline drain failed: {err:#}"),
    }

    let res = loop {
        let frame = match framing::read_frame(&mut recv).await {
            Ok(f) => f,
            Err(err) => {
                if err.kind() == std::io::ErrorKind::UnexpectedEof {
                    break Ok(());
                }
                break Err(err.into());
            }
        };
        let Ok(wire) = WireFrame::from_bytes(&frame) else {
            continue;
        };
        let env = match wire {
            WireFrame::Msg(env) => env,
            WireFrame::Ping | WireFrame::Hello { .. } => continue,
        };
        if let Err(err) = handle_inbound(&state, &user_id, &env).await {
            eprintln!("[mq-hub] inbound error: {err:#}");
        }
    };

    // only unbind if this stream is still the user's current one
    let current = state.peers.read().await.get(&user_id).cloned();
    if current.map(|s| Arc::ptr_eq(&s, &send)).unwrap_or(false) {
        state.peers.write().await.remove(&user_id);
        let _ = state
            .router
            .routes
            .unbind_user(&user_id, &state.hub_id)
            .await;
    }
    res
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
        return route_envelope(state, &directed).await;
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
        return route_envelope(state, env).await;
    }

    // 2. content frames: dedup, then persist, then route
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

    route_envelope(state, env).await
}

/// Deliver locally if recipients are here; publish to their hub otherwise.
/// Empty `to` = broadcast: everyone local except sender, plus every other hub.
async fn route_envelope(state: &Arc<HubState>, env: &Envelope) -> anyhow::Result<()> {
    let frame = WireFrame::Msg(env.clone()).to_bytes()?;

    if env.to.is_empty() {
        deliver_local(state, env).await;
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

    let mut remote_hubs = std::collections::BTreeSet::new();
    let mut handled = HashSet::new();
    for rcpt in &env.to {
        // a recipient listed twice must be notified once
        if !handled.insert(rcpt.as_str()) {
            continue;
        }
        match state.router.routes.route_of(rcpt).await? {
            Some(h) if h == state.hub_id => {
                deliver_to(state, rcpt, &frame).await;
            }
            Some(h) => {
                remote_hubs.insert(h);
            }
            None => {
                // recipient never connected anywhere: park for their first Hello
                state.offline.enqueue(rcpt, frame.clone()).await?;
            }
        }
    }
    for h in remote_hubs {
        state
            .router
            .mq
            .publish(&hub_queue(&h), frame.clone())
            .await?;
    }
    Ok(())
}

async fn deliver_local(state: &Arc<HubState>, env: &Envelope) {
    let frame = match WireFrame::Msg(env.clone()).to_bytes() {
        Ok(f) => f,
        Err(_) => return,
    };
    let map = state.peers.read().await;
    for (user, send) in map.iter() {
        if *user == env.sender {
            continue; // no echo to sender
        }
        if !env.to.is_empty() && !env.to.contains(user) {
            continue;
        }
        let mut s = send.lock().await;
        if let Err(err) = framing::write_frame(&mut *s, &frame).await {
            eprintln!("[mq-hub] local deliver to {user} failed: {err}");
        }
    }
}

async fn deliver_to(state: &Arc<HubState>, user: &str, frame: &[u8]) {
    let send = state.peers.read().await.get(user).cloned();
    if let Some(send) = send {
        let mut s = send.lock().await;
        if let Err(err) = framing::write_frame(&mut *s, frame).await {
            eprintln!("[mq-hub] deliver to {user} failed: {err}");
        }
    }
}

/// Dedup cache occupancy, exposed for tests and metrics.
pub async fn dedup_len(handle: &HubHandle) -> usize {
    handle.state.seen.lock().await.len()
}
