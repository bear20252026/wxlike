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
            let Some(incoming) = ep.accept().await else {
                break;
            };
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

    Ok(Hub {
        endpoint,
        accept_loop,
    })
}

async fn serve_conn(
    conn: quinn::Connection,
    peers: Arc<Mutex<HashMap<u64, Arc<Mutex<quinn::SendStream>>>>>,
    next_id: Arc<Mutex<u64>>,
) -> anyhow::Result<()> {
    let (send, mut recv) = conn.accept_bi().await.context("accept control stream")?;
    let id = next_peer_id(&next_id).await;
    let send = Arc::new(Mutex::new(send));
    peers.lock().await.insert(id, send.clone());

    loop {
        match next_relay_frame(&mut recv).await {
            Ok(Some(frame)) => relay_frame(&peers, id, &frame).await,
            // clean disconnect
            Ok(None) => {
                peers.lock().await.remove(&id);
                return Ok(());
            }
            Err(err) => {
                peers.lock().await.remove(&id);
                return Err(err);
            }
        }
    }
}

/// 分配一个连接号。
async fn next_peer_id(next_id: &Arc<Mutex<u64>>) -> u64 {
    let mut n = next_id.lock().await;
    let id = *n;
    *n += 1;
    id
}

/// 读下一个可转发的帧；`None` 表示对端干净断开。
///
/// 这个 hub 是**广播转发器**（`crates/net/tests` 的用例），不做认证：它不绑定
/// 身份、不读离线队列、不落库，所以冒充在这里没有收益。Ping/Hello/v3 control
/// 在它这里也没有语义（没有身份、路由与离线库），直接跳过。生产路径走
/// `mq_hub`（带 `HubAuthenticator`）。
async fn next_relay_frame(recv: &mut quinn::RecvStream) -> anyhow::Result<Option<Vec<u8>>> {
    loop {
        let frame = match framing::read_frame(&mut *recv).await {
            Ok(frame) => frame,
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        match WireFrame::from_bytes(&frame) {
            Ok(WireFrame::Msg(_)) => return Ok(Some(frame)),
            _ => continue,
        }
    }
}

/// 把一帧广播给除发送者外的所有在线 peer。
async fn relay_frame(
    peers: &Arc<Mutex<HashMap<u64, Arc<Mutex<quinn::SendStream>>>>>,
    id: u64,
    frame: &[u8],
) {
    let map = peers.lock().await;
    for (peer_id, peer_send) in map.iter() {
        if *peer_id == id {
            continue; // no echo back to sender
        }
        let mut s = peer_send.lock().await;
        if let Err(err) = framing::write_frame(&mut *s, frame).await {
            eprintln!("[hub] relay to peer {peer_id} failed: {err}");
        }
    }
}
