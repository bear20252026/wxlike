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
    /// Production constructor (WP-12): connects with trust-on-first-use and
    /// pins the server certificate fingerprint in `pins_path`.
    pub async fn connect_verified(
        server_addr: SocketAddr,
        server_name: &str,
        user_id: impl Into<String>,
        pins_path: impl AsRef<std::path::Path>,
    ) -> anyhow::Result<Self> {
        let conn = crate::transport::connect_tofu(server_addr, server_name, pins_path).await?;
        Self::hello(conn, user_id.into()).await
    }

    /// Lab constructor: skips certificate verification. Gate behind an
    /// explicit `--insecure-lab` switch; use [`Client::connect_verified`]
    /// everywhere else.
    pub async fn connect(
        server_addr: SocketAddr,
        server_name: &str,
        user_id: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let conn = crate::transport::connect_insecure(server_addr, server_name).await?;
        Self::hello(conn, user_id.into()).await
    }

    /// **生产重连**：带退避重试，且每次都走 TOFU + 证书固定。
    ///
    /// 之前的生产重连助手 `connect_retry` 内部调的是跳过校验的 `connect`——
    /// 重连路径比首次连接更容易被中间人利用（攻击者只要让首次连接失败、
    /// 在重连时出现即可）。所以这里单独开一个"verified + 重试"的入口，
    /// 而不是给旧函数加参数。
    pub async fn connect_retry_verified(
        server_addr: SocketAddr,
        server_name: &str,
        user_id: &str,
        pins_path: impl AsRef<std::path::Path>,
        deadline: std::time::Duration,
    ) -> anyhow::Result<Self> {
        Self::retry_with(
            deadline,
            |addr, name: String| {
                let pins = pins_path.as_ref().to_path_buf();
                let user = user_id.to_string();
                async move { Self::connect_verified(addr, &name, user, pins).await }
            },
            server_addr,
            server_name,
        )
        .await
    }

    /// 实验室用的重连（跳过校验）。**不要在生产调用**——用
    /// [`Client::connect_retry_verified`]。
    #[doc(hidden)]
    pub async fn connect_retry_insecure(
        server_addr: SocketAddr,
        server_name: &str,
        user_id: &str,
        deadline: std::time::Duration,
    ) -> anyhow::Result<Self> {
        Self::retry_with(
            deadline,
            |addr, name: String| {
                let user = user_id.to_string();
                async move { Self::connect(addr, &name, user).await }
            },
            server_addr,
            server_name,
        )
        .await
    }

    async fn retry_with<F, Fut>(
        deadline: std::time::Duration,
        attempt_fn: F,
        server_addr: SocketAddr,
        server_name: &str,
    ) -> anyhow::Result<Self>
    where
        F: Fn(SocketAddr, String) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<Self>>,
    {
        let started = std::time::Instant::now();
        let mut backoff = std::time::Duration::from_millis(200);
        loop {
            let attempt = tokio::time::timeout(
                backoff * 4,
                attempt_fn(server_addr, server_name.to_string()),
            )
            .await;
            match attempt {
                Ok(Ok(client)) => return Ok(client),
                Ok(Err(err)) => {
                    tracing::debug!("reconnect attempt failed: {err:#}");
                }
                Err(_) => tracing::debug!("reconnect attempt timed out"),
            }
            if started.elapsed() >= deadline {
                anyhow::bail!("could not reconnect to {server_addr} within {:?}", deadline);
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(std::time::Duration::from_secs(2));
        }
    }

    async fn hello(conn: quinn::Connection, user_id: String) -> anyhow::Result<Self> {
        let (mut send, recv) = conn.open_bi().await.context("open control stream")?;
        // QUIC streams are lazy: the server only sees the stream once data
        // arrives. The Hello frame materializes the stream and registers us.
        framing::write_frame(
            &mut send,
            &WireFrame::Hello {
                user_id: user_id.clone(),
            }
            .to_bytes()?,
        )
        .await
        .context("send hello")?;
        Ok(Self {
            send: Arc::new(Mutex::new(send)),
            recv: Arc::new(Mutex::new(recv)),
            user_id,
        })
    }

    /// Back-compat constructor used by the broadcast-hub tests/examples.
    pub async fn connect_anonymous(
        server_addr: SocketAddr,
        server_name: &str,
    ) -> anyhow::Result<Self> {
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
