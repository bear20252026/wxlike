//! MQ-routed hub: multi-node message routing, Rust port of box-im's design.
//!
//! Each hub:
//! - registers itself in the route table and consumes its own queue
//!   (`wxlike:msg:hub:{hub_id}`)
//! - binds connecting users (`user -> hub_id`) in the route table
//! - delivers locally when the recipient is here, otherwise publishes to the
//!   recipient hub's queue
//!
//! Delivery semantics: at-most-once scaffold (Redis BRPOP / in-memory deque).
//! Store-and-forward for offline users and at-least-once acks are the next
//! step (see README).

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use wxlike_core::Envelope;
use wxlike_mq::{hub_queue, MqRouter};

use crate::framing;
use crate::offline::{MemoryOffline, OfflineStore};
use crate::wire::WireFrame;

/// Hub knobs beyond the router. Defaults: in-memory offline inbox (cap 500).
#[derive(Clone)]
pub struct MqHubOptions {
    pub offline: Arc<dyn OfflineStore>,
}

impl Default for MqHubOptions {
    fn default() -> Self {
        Self {
            offline: MemoryOffline::new(500),
        }
    }
}

pub struct MqHub {
    pub hub_id: String,
    pub endpoint: quinn::Endpoint,
    tasks: Vec<JoinHandle<()>>,
}

impl MqHub {
    pub fn local_addr(&self) -> anyhow::Result<std::net::SocketAddr> {
        Ok(self.endpoint.local_addr()?)
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

type Peers = Arc<RwLock<HashMap<String, Arc<Mutex<quinn::SendStream>>>>>;

/// Bind a QUIC hub that routes via `router` (memory or redis backend).
pub async fn run_mq_hub(
    hub_id: impl Into<String>,
    addr: std::net::SocketAddr,
    router: MqRouter,
) -> anyhow::Result<MqHub> {
    run_mq_hub_with(hub_id, addr, router, MqHubOptions::default()).await
}

/// Same, with explicit options (offline store etc.).
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

    // local user -> stream
    let peers: Peers = Arc::new(RwLock::new(HashMap::new()));

    // accept loop
    let accept_peers = peers.clone();
    let accept_router = router.clone();
    let accept_id = hub_id.clone();
    let accept_offline = options.offline.clone();
    let ep = endpoint.clone();
    let accept_loop = tokio::spawn(async move {
        loop {
            let Some(incoming) = ep.accept().await else {
                break;
            };
            let Ok(conn) = incoming.await else { continue };
            let peers = accept_peers.clone();
            let router = accept_router.clone();
            let hub_id = accept_id.clone();
            let offline = accept_offline.clone();
            tokio::spawn(async move {
                if let Err(err) = serve_conn(conn, hub_id, peers, router, offline).await {
                    eprintln!("[mq-hub] conn error: {err:#}");
                }
            });
        }
    });

    // queue consume loop: messages from other hubs destined to us
    let consume_router = router.clone();
    let consume_peers = peers.clone();
    let consume_id = hub_id.clone();
    let consume_loop = tokio::spawn(async move {
        let queue = hub_queue(&consume_id);
        loop {
            match consume_router.mq.consume(&queue).await {
                Ok(bytes) => {
                    let frame = match WireFrame::from_bytes(&bytes) {
                        Ok(f) => f,
                        Err(err) => {
                            eprintln!("[mq-hub] bad frame from mq: {err}");
                            continue;
                        }
                    };
                    if let WireFrame::Msg(env) = frame {
                        deliver_local(&consume_peers, &env).await;
                    }
                }
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
        tasks: vec![accept_loop, consume_loop],
    })
}

async fn serve_conn(
    conn: quinn::Connection,
    hub_id: String,
    peers: Peers,
    router: MqRouter,
    offline: Arc<dyn OfflineStore>,
) -> anyhow::Result<()> {
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
    peers.write().await.insert(user_id.clone(), send.clone());

    router
        .routes
        .bind_user(&user_id, &hub_id)
        .await
        .context("bind user route")?;

    // store-and-forward: flush everything parked while the user was away
    match offline.drain(&user_id).await {
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
        if let Err(err) = route_envelope(&hub_id, &peers, &router, &offline, &env).await {
            eprintln!("[mq-hub] route error: {err:#}");
        }
    };

    peers.write().await.remove(&user_id);
    let _ = router.routes.unbind_user(&user_id, &hub_id).await;
    res
}

/// Deliver locally if recipients are here; publish to their hub otherwise.
/// Empty `to` = broadcast: everyone local except sender, plus every other hub.
async fn route_envelope(
    hub_id: &str,
    peers: &Peers,
    router: &MqRouter,
    offline: &Arc<dyn OfflineStore>,
    env: &Envelope,
) -> anyhow::Result<()> {
    let frame = WireFrame::Msg(env.clone()).to_bytes()?;

    if env.to.is_empty() {
        // broadcast: local + fan out to all other hubs
        deliver_local(peers, env).await;
        for other in router.routes.list_hubs().await? {
            if other != hub_id {
                router.mq.publish(&hub_queue(&other), frame.clone()).await?;
            }
        }
        return Ok(());
    }

    let mut remote_hubs = std::collections::BTreeSet::new();
    for rcpt in &env.to {
        match router.routes.route_of(rcpt).await? {
            Some(h) if h == hub_id => {
                deliver_to(peers, rcpt, &frame).await;
            }
            Some(h) => {
                remote_hubs.insert(h);
            }
            None => {
                // recipient never connected anywhere: park for their first Hello
                offline.enqueue(rcpt, frame.clone()).await?;
            }
        }
    }
    for h in remote_hubs {
        router.mq.publish(&hub_queue(&h), frame.clone()).await?;
    }
    Ok(())
}

async fn deliver_local(peers: &Peers, env: &Envelope) {
    let frame = match WireFrame::Msg(env.clone()).to_bytes() {
        Ok(f) => f,
        Err(_) => return,
    };
    let map = peers.read().await;
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

async fn deliver_to(peers: &Peers, user: &str, frame: &[u8]) {
    let send = peers.read().await.get(user).cloned();
    if let Some(send) = send {
        let mut s = send.lock().await;
        if let Err(err) = framing::write_frame(&mut *s, frame).await {
            eprintln!("[mq-hub] deliver to {user} failed: {err}");
        }
    }
}
