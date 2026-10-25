//! Control-plane frames sent on the client's control stream before envelopes.

use serde::{Deserialize, Serialize};
use wxlike_core::Envelope;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum WireFrame {
    /// 首帧：**只带凭据，不带 `user_id`**。
    ///
    /// 以前这里是 `Hello { user_id }`，客户端自报身份，hub 收到就注册。
    /// 知道 16 位 user_id（它出现在每条消息的 `sender` 里）就能冒充收实时消息、
    /// 取走并销毁离线队列——issue #3（P0）。现在身份只由服务端从 `token` 解析。
    Hello {
        /// 用户凭据（HTTP 登录拿到的 Bearer token）。空串一律拒绝。
        token: String,
    },
    /// 服务端对 [`WireFrame::Hello`] 的应答：**告诉客户端它是谁**。
    ///
    /// 客户端从此不需要（也不应该）自己知道 user_id——发消息时用它填
    /// `Envelope::sender`。认证失败时服务端直接关连接，不会发这一帧。
    HelloAck {
        user_id: String,
        /// 这次连接绑定的会话，便于客户端在"设备列表"里认领自己。
        session_id: String,
    },
    /// 服务端拒绝握手：**先给原因，再关连接**。
    ///
    /// 没有这一帧，客户端只能看到 `connection lost: closed by peer: 0`，
    /// 分不清"凭据过期"和"网络断了"——排障会往完全错误的方向走。
    /// 原因只写"哪一类"（无效凭据 / 服务端未配置认证），不回显 token。
    HelloReject {
        reason: String,
    },
    Msg(Envelope),
    /// Keepalive; also materializes the QUIC stream (lazy-open semantics).
    Ping,
    // ---------------------------------------------------------------------
    // G03-04 / protocol v3. These are **appended**: bincode encodes an enum by
    // variant index, so every v2 frame keeps its exact bytes and an old client
    // keeps working. The hub only sends a v3 frame to a peer that declared v3
    // in [`WireFrame::ClientHello`] (see `crate::proto::Negotiated`).
    // ---------------------------------------------------------------------
    /// First frame of a v3 handshake. **The device identifies itself, the user
    /// never does** — `user_id` still comes only from the credential in
    /// [`WireFrame::Hello`]/[`WireFrame::Auth`].
    ClientHello {
        device_id: String,
        /// Highest protocol the peer implements (2..=3).
        protocol: u8,
        /// `proto::caps_bits` of the peer's capabilities.
        caps: u32,
    },
    /// Server-side authentication result for a v3 connection: this is where the
    /// client learns who it is and which route generation it now holds.
    ///
    /// `generation` is the lease token for the route row: an old connection can
    /// only clear the route if it still owns the generation (G03-04 要求 1).
    Auth {
        user_id: String,
        device_id: String,
        session_id: String,
        version: u8,
    },
    /// Narrow the streams this connection wants. Scopes are opaque to the hub
    /// (`conv:<id>`), and the server authorises each one — an unauthorised
    /// scope is a close, not a silent drop.
    Subscribe {
        scopes: Vec<String>,
    },
    /// Server → client range hint: messages `from_seq..=to_seq` are available
    /// for `conv_id`, and `gap` says "you are further behind than a page".
    Event {
        conv_id: String,
        from_seq: u64,
        to_seq: u64,
        gap: bool,
    },
    /// Client → server: "I have persisted through `stored_cursor`". This is the
    /// only acknowledgement that retires a parked frame (G03-02).
    Ack {
        conv_id: String,
        stored_cursor: u64,
    },
    /// Client → server after a reconnect: send what is missing after `cursor`.
    Resume {
        conv_id: String,
        cursor: u64,
    },
}

impl WireFrame {
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(bincode::serialize(self)?)
    }

    pub fn from_bytes(b: &[u8]) -> anyhow::Result<Self> {
        Ok(bincode::deserialize(b)?)
    }
}
