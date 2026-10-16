//! Client: one long-lived bidirectional stream to a hub, with identity.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context as _;
use tokio::sync::Mutex;
use wxlike_core::Envelope;

use crate::framing;
use crate::wire::WireFrame;

#[derive(Clone)]
pub struct Client {
    send: Arc<Mutex<quinn::SendStream>>,
    recv: Arc<Mutex<quinn::RecvStream>>,
    pub user_id: String,
}

impl Client {
    pub async fn connect(
        server_addr: SocketAddr,
        server_name: &str,
        user_id: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let conn = crate::transport::connect_insecure(server_addr, server_name).await?;
        let (mut send, recv) = conn.open_bi().await.context("open control stream")?;
        // QUIC streams are lazy: the server only sees the stream once data
        // arrives. The Hello frame materializes the stream and registers us.
        let user_id = user_id.into();
        framing::write_frame(&mut send, &WireFrame::Hello { user_id: user_id.clone() }.to_bytes()?)
            .await
            .context("send hello")?;
        Ok(Self {
            send: Arc::new(Mutex::new(send)),
            recv: Arc::new(Mutex::new(recv)),
            user_id,
        })
    }

    /// Back-compat constructor used by the broadcast-hub tests/examples.
    pub async fn connect_anonymous(server_addr: SocketAddr, server_name: &str) -> anyhow::Result<Self> {
        let conn = crate::transport::connect_insecure(server_addr, server_name).await?;
        let (mut send, recv) = conn.open_bi().await.context("open control stream")?;
        framing::write_frame(&mut send, &WireFrame::Ping.to_bytes()?)
            .await
            .context("materialize stream")?;
        Ok(Self {
            send: Arc::new(Mutex::new(send)),
            recv: Arc::new(Mutex::new(recv)),
            user_id: String::new(),
        })
    }

    pub async fn send(&self, env: &Envelope) -> anyhow::Result<()> {
        let frame = WireFrame::Msg(env.clone()).to_bytes()?;
        let mut s = self.send.lock().await;
        framing::write_frame(&mut *s, &frame).await?;
        Ok(())
    }

    /// Await the next envelope relayed by the hub.
    pub async fn next_envelope(&self) -> anyhow::Result<Envelope> {
        let mut r = self.recv.lock().await;
        loop {
            let frame = framing::read_frame(&mut *r).await?;
            match WireFrame::from_bytes(&frame)? {
                WireFrame::Msg(env) => return Ok(env),
                WireFrame::Ping => continue,
                WireFrame::Hello { .. } => continue,
            }
        }
    }
}
