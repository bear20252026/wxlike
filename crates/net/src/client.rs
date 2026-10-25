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
    /// 保留连接句柄：读 RTT 与存活状态，喂给自适应心跳
    /// （`crate::heartbeat`）。流本身不足以判断"连接是否还活着"。
    conn: quinn::Connection,
    pub user_id: String,
}

impl Client {
    /// Production constructor (WP-12): connects with trust-on-first-use and
    /// pins the server certificate fingerprint in `pins_path`.
    ///
    /// 第二个参数是**凭据**（HTTP 登录拿到的 token），不是 `user_id`：
    /// 身份由服务端解析后通过 [`WireFrame::HelloAck`] 告知（issue #3）。
    pub async fn connect_verified(
        server_addr: SocketAddr,
        server_name: &str,
        token: impl Into<String>,
        pins_path: impl AsRef<std::path::Path>,
    ) -> anyhow::Result<Self> {
        let conn = crate::transport::connect_tofu(server_addr, server_name, pins_path).await?;
        Self::hello(conn, token.into()).await
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

    /// 握手：发凭据，等 `HelloAck` 告诉我们**自己是谁**。
    ///
    /// 顺序很重要——先收 ack 再返回 `Client`，否则调用方会拿到一个
    /// `user_id` 还是空的连接，第一条消息的 `sender` 就是空串。
    /// 认证失败时服务端直接关连接，这里表现为读帧出错。
    async fn hello(conn: quinn::Connection, token: String) -> anyhow::Result<Self> {
        let (mut send, mut recv) = conn.open_bi().await.context("open control stream")?;
        // QUIC streams are lazy: the server only sees the stream once data
        // arrives. The Hello frame materializes the stream and authenticates us.
        framing::write_frame(&mut send, &WireFrame::Hello { token }.to_bytes()?)
            .await
            .context("send hello")?;

        let frame = framing::read_frame(&mut recv)
            .await
            .context("await hello ack (认证失败时服务端会直接关连接)")?;
        let user_id = match WireFrame::from_bytes(&frame)? {
            WireFrame::HelloAck { user_id, .. } => user_id,
            // 服务端明确拒绝：把原因原样带出去，别让调用方去猜"是网断了还是凭据废了"
            WireFrame::HelloReject { reason } => anyhow::bail!("credential rejected: {reason}"),
            other => anyhow::bail!("expected HelloAck, got {other:?}"),
        };

        Ok(Self {
            send: Arc::new(Mutex::new(send)),
            recv: Arc::new(Mutex::new(recv)),
            conn,
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
            conn,
            user_id: String::new(),
        })
    }

    /// 喂一条心跳观测给自适应策略，返回**下一次**该采样的间隔。
    ///
    /// `crates/net` 不自己起定时器：要算的就是"多久采一次"，自己调度会形成
    /// 循环依赖。调用方（客户端的维护线程）按返回的间隔下次再来即可。
    pub fn sample_heartbeat(&self) -> std::time::Duration {
        let sample = match self.conn.close_reason() {
            None => crate::heartbeat::HeartSample::ok(self.conn.rtt().as_millis() as u64),
            // 连接已经关了：按超时失败处理，让策略回落到最短间隔
            Some(_) => crate::heartbeat::HeartSample::failed(true),
        };
        crate::heartbeat::Heartbeat::current().record(sample)
    }

    /// 发一次心跳。
    ///
    /// 服务端在收到 `Ping` 时会顺带复查凭据（`HubAuthenticator::is_revoked`），
    /// 所以这个函数也是"吊销能不能踢掉长连接"的触发点。客户端的维护线程
    /// 按 [`Client::sample_heartbeat`] 给出的间隔调它即可。
    pub async fn ping(&self) -> anyhow::Result<()> {
        let mut s = self.send.lock().await;
        framing::write_frame(&mut *s, &WireFrame::Ping.to_bytes()?).await?;
        Ok(())
    }

    /// 当前连接是否还活着。
    pub fn is_alive(&self) -> bool {
        self.conn.close_reason().is_none()
    }

    /// 平滑 RTT（毫秒）。没连上时是 0。
    pub fn rtt_ms(&self) -> u64 {
        self.conn.rtt().as_millis() as u64
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
                WireFrame::Hello { .. }
                | WireFrame::HelloAck { .. }
                | WireFrame::HelloReject { .. } => continue,
                // A v2 client must never be fed a v3 downlink frame: the hub only
                // sends one after a v3 ClientHello. If it happens anyway the
                // server broke the negotiation — skip the frame rather than
                // pretend it is an envelope.
                _ => continue,
            }
        }
    }
}
