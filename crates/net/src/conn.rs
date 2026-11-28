//! P3 Transport abstraction (ADR-8): one frame-pipe contract, two carriers.
//!
//! The hub must not care whether a frame arrived over QUIC (desktop/mobile)
//! or a WebSocket (browsers have no QUIC). [`FrameTx`]/[`FrameRx`] are that
//! contract; [`PeerConn`] is the hub-facing half-connection (one tx half for
//! fan-out writers, one rx half owned by the serve loop).
//!
//! Framing differs by carrier and both are hidden here:
//! * QUIC streams are frame-delimited with the length prefix
//!   ([`crate::framing`]);
//! * WebSocket messages are self-delimited — one binary message == one frame.
//!
//! The wire protocol (bincode `WireFrame`, first frame = Hello) is identical
//! on both carriers, so a client can switch transport without touching any
//! other layer.

use anyhow::Context as _;
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Write half of a connection, in frames.
#[async_trait::async_trait]
pub trait FrameTx: Send {
    async fn send_frame(&mut self, frame: &[u8]) -> anyhow::Result<()>;

    /// 主动关闭本连接的发送侧（管理端撤销会话/设备时用）。默认空实现：
    /// 不支持主动关闭的载体只需自然断开。
    async fn close(&mut self) {}
}

/// Read half of a connection, in frames. `Err` = connection over.
#[async_trait::async_trait]
pub trait FrameRx: Send {
    async fn read_frame(&mut self) -> anyhow::Result<Vec<u8>>;
}

pub type SharedTx = Arc<Mutex<Box<dyn FrameTx>>>;
pub type SharedRx = Arc<Mutex<Box<dyn FrameRx>>>;

/// Hub-facing connection: clonable halves sharing one lock each (fan-out
/// writers take the tx lock per delivery; the serve loop holds the rx lock
/// for its lifetime). `Clone` = another handle to the SAME connection, which
/// is what the peers table and the serve loop each hold.
#[derive(Clone)]
pub struct PeerConn {
    tx: SharedTx,
    rx: SharedRx,
    /// Best-effort transport peer address, when the carrier exposes one.
    /// The hub uses it for the pre-auth per-IP budget only; it is never an
    /// identity or an authorization input.
    peer_ip: Option<IpAddr>,
}

impl PeerConn {
    pub fn quic(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self {
            tx: Arc::new(Mutex::new(Box::new(QuicTx { send }))),
            rx: Arc::new(Mutex::new(Box::new(QuicRx { recv }))),
            peer_ip: None,
        }
    }

    /// Takes ownership of the halves (the WS route splits an upgraded socket
    /// and hands both halves over).
    pub fn new(tx: Box<dyn FrameTx>, rx: Box<dyn FrameRx>) -> Self {
        Self {
            tx: Arc::new(Mutex::new(tx)),
            rx: Arc::new(Mutex::new(rx)),
            peer_ip: None,
        }
    }

    /// Attach the carrier's observed peer address. In-process test pipes have
    /// no address and simply skip the per-IP budget.
    #[must_use]
    pub fn with_peer_ip(mut self, peer_ip: IpAddr) -> Self {
        self.peer_ip = Some(peer_ip);
        self
    }

    pub fn peer_ip(&self) -> Option<IpAddr> {
        self.peer_ip
    }

    pub async fn send_frame(&self, frame: &[u8]) -> anyhow::Result<()> {
        self.tx
            .lock()
            .await
            .send_frame(frame)
            .await
            .context("send frame")
    }

    pub async fn read_frame(&self) -> anyhow::Result<Vec<u8>> {
        self.rx
            .lock()
            .await
            .read_frame()
            .await
            .context("read frame")
    }

    /// 主动关闭发送侧：管理端撤销时让对端立刻收到结束信号。
    ///
    /// QUIC 只有发送流句柄，`finish` 是优雅结束；要立即断开整个连接用
    /// `quinn::Connection::close`（由 hub 的 `ConnCloser` 持有，见 `mq_hub`）。
    pub async fn close(&self) {
        self.tx.lock().await.close().await;
    }
}

struct QuicTx {
    send: quinn::SendStream,
}

#[async_trait::async_trait]
impl FrameTx for QuicTx {
    async fn send_frame(&mut self, frame: &[u8]) -> anyhow::Result<()> {
        crate::framing::write_frame(&mut self.send, frame).await?;
        Ok(())
    }

    async fn close(&mut self) {
        // 优雅结束发送方向（发 FIN）；立即断开整个连接走 ConnCloser。
        let _ = self.send.finish();
    }
}

struct QuicRx {
    recv: quinn::RecvStream,
}

#[async_trait::async_trait]
impl FrameRx for QuicRx {
    async fn read_frame(&mut self) -> anyhow::Result<Vec<u8>> {
        Ok(crate::framing::read_frame(&mut self.recv).await?)
    }
}

// ---------------------------------------------------------------------------
// WebSocket carrier
// ---------------------------------------------------------------------------

pub mod ws {
    use super::{FrameRx, FrameTx};
    use anyhow::Context as _;
    use axum::extract::ws::{Message, WebSocket};
    use futures_util::{SinkExt, StreamExt};
    use tokio::sync::Mutex;

    /// Builds a hub-facing connection from an upgraded WebSocket: one binary
    /// message per frame; ping/pong and text are not part of the protocol and
    /// are skipped (or rejected) on read.
    pub fn peer_conn(socket: WebSocket, peer_ip: std::net::IpAddr) -> super::PeerConn {
        let (sink, stream) = socket.split();
        super::PeerConn::new(Box::new(WsTx(Mutex::new(sink))), Box::new(WsRx(stream)))
            .with_peer_ip(peer_ip)
    }

    /// axum's sink is `SinkExt`-based rather than `&mut`-friendly under our
    /// lock model, so the tx half keeps its own mutex.
    struct WsTx(Mutex<futures_util::stream::SplitSink<WebSocket, Message>>);

    #[async_trait::async_trait]
    impl FrameTx for WsTx {
        async fn send_frame(&mut self, frame: &[u8]) -> anyhow::Result<()> {
            let mut sink = self.0.lock().await;
            sink.send(Message::Binary(frame.to_vec().into()))
                .await
                .context("ws send")
        }

        async fn close(&mut self) {
            let mut sink = self.0.lock().await;
            let _ = sink.close().await;
        }
    }

    struct WsRx(futures_util::stream::SplitStream<WebSocket>);

    #[async_trait::async_trait]
    impl FrameRx for WsRx {
        async fn read_frame(&mut self) -> anyhow::Result<Vec<u8>> {
            loop {
                let Some(msg) = self.0.next().await else {
                    anyhow::bail!("ws closed");
                };
                match msg.context("ws recv")? {
                    Message::Binary(b) => return Ok(b.to_vec()),
                    Message::Text(t) => anyhow::bail!("unexpected text frame: {t}"),
                    Message::Ping(_) | Message::Pong(_) => continue, // ws keepalive
                    Message::Close(_) => anyhow::bail!("ws closed"),
                }
            }
        }
    }
}
