//! Broadcast hub (legacy, single-node): every envelope relayed to all peers.
//!
//! Superseded by [`crate::mq_hub`] for multi-node routing, kept for tests.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::framing;
use crate::wire::WireFrame;

pub struct Hub {
    pub endpoint: quinn::Endpoint,
    accept_loop: JoinHandle<()>,
}

impl Hub {
    pub fn local_addr(&self) -> anyhow::Result<std::net::SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.accept_loop.abort();
        self.endpoint.close(0u32.into(), b"hub shutdown");
    }
}

/// Bind and run the broadcast hub until the returned `Hub` is dropped.
pub async fn run_hub(addr: std::net::SocketAddr) -> anyhow::Result<Hub> {
    let endpoint = crate::transport::make_server_endpoint(addr)?;
    let peers: Arc<Mutex<HashMap<u64, Arc<Mutex<quinn::SendStream>>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let next_id = Arc::new(Mutex::new(0u64));

    let ep = endpoint.clone();
    let accept_loop = tokio::spawn(async move {
        loop {
            let Some(incoming) = ep.accept().await else { break };
            let Ok(conn) = incoming.await else { continue };
            let peers = peers.clone();
            let next_id = next_id.clone();
            tokio::spawn(async move {
                if let Err(err) = serve_conn(conn, peers, next_id).await {
                    eprintln!("[hub] conn error: {err:#}");
                }
            });
        }
    });

    Ok(Hub { endpoint, accept_loop })
}

async fn serve_conn(
    conn: quinn::Connection,
    peers: Arc<Mutex<HashMap<u64, Arc<Mutex<quinn::SendStream>>>>>,
    next_id: Arc<Mutex<u64>>,
) -> anyhow::Result<()> {
    let (send, mut recv) = conn.accept_bi().await.context("accept control stream")?;
    let id = {
        let mut n = next_id.lock().await;
        let id = *n;
        *n += 1;
        id
    };
    let send = Arc::new(Mutex::new(send));
    peers.lock().await.insert(id, send.clone());

    loop {
        let frame = match framing::read_frame(&mut recv).await {
            Ok(f) => f,
            Err(err) => {
                peers.lock().await.remove(&id);
                if err.kind() == std::io::ErrorKind::UnexpectedEof {
                    return Ok(()); // clean disconnect
                }
                return Err(err.into());
            }
        };
        let Ok(wire) = WireFrame::from_bytes(&frame) else { continue };
        let env = match wire {
            WireFrame::Msg(env) => env,
            WireFrame::Ping | WireFrame::Hello { .. } => continue,
        };
        let _ = env;
        let map = peers.lock().await;
        for (peer_id, peer_send) in map.iter() {
            if *peer_id == id {
                continue; // no echo back to sender
            }
            let mut s = peer_send.lock().await;
            if let Err(err) = framing::write_frame(&mut *s, &frame).await {
                eprintln!("[hub] relay to peer {peer_id} failed: {err}");
            }
        }
    }
}
