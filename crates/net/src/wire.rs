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
}

impl WireFrame {
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(bincode::serialize(self)?)
    }

    pub fn from_bytes(b: &[u8]) -> anyhow::Result<Self> {
        Ok(bincode::deserialize(b)?)
    }
}
