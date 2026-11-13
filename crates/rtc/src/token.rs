//! 两件事（ADR-25），拆成两个子模块：
//!
//! 1. **内部媒体令牌**（`media`）：`v1.<hex(claims)>.<hex(HMAC-SHA256)>`，只在
//!    wxlike 自己的 QUIC 信令面里流转，回答"这次连接是否属于这个 call、这个用户、
//!    这个设备、这个 scope，且签发代次仍然有效"。签名自包含，服务端不需要为每个
//!    token 建表；撤销靠 `generation` 自增（退群/封禁/停播即整代失效）。
//! 2. **LiveKit sidecar 访问令牌**（`livekit`）：媒体面已经交给 LiveKit sidecar，
//!    sidecar 只认 `apiKey`/`apiSecret` 签名的 HS256 JWT。因此**签发权必须留在
//!    Rust**：由服务端裁决谁能进哪个房间、能发还是只能收、能待多久，再把它翻译成
//!    LiveKit 的 `video` grant。
//!
//! ADR-25 的两条硬闸写在代码里，不写在文档里：
//!
//! * `assert_sidecar_grant_allowed`：客户端令牌永远不含 `recorder`/`hidden`
//!   （服务端 Egress 自动录制关闭）与 `roomAdmin`/`ingressAdmin`（避免 sidecar
//!   里长出第二套房间事实）。
//! * 引擎归属由房间固定（`crate::room::MediaEngine`），令牌里的 `wxlike.engine`
//!   必须与房间一致，冷备引擎的信令不能混进 LiveKit 房间。
//!
//! claims 是公开数据，安全边界完全来自 HMAC；密钥只以 `Redacted` 形式出现在
//! Debug 输出里。
//!
//! 拆分前是"两件事，一个文件"（2,167 行）。依赖是单向的：`livekit` 用 `media` 的
//! `MediaScope` / `TokenError`，`media` 不引用任何 LiveKit 类型。子模块保持私有，
//! **公开 API 与拆分前逐字相同**——全部经下面的 `pub use` 回到本模块。

mod livekit;
mod media;

pub use livekit::{
    assert_participant_kind_allowed, assert_sidecar_grant_allowed, bearer_token,
    ensure_admin_permission, ensure_create_permission, ensure_dest_room_permission,
    ensure_ingress_admin_permission, ensure_join_permission, ensure_list_permission,
    ensure_record_permission, ensure_sip_admin_permission, ensure_sip_call_permission, grant_for,
    AccessClaims, AccessRequest, ApiKeyPair, IssuedAccess, LiveKitIssuer, ParticipantKind,
    SipGrant, VideoGrant, WxlikeClaims, DEFAULT_ACCESS_TTL_MS, LIVEKIT_ALG,
    LIVEKIT_DEFAULT_TTL_SECS, MAX_ACCESS_TTL_MS, MIN_API_SECRET_LEN,
};
pub use media::{
    MediaClaims, MediaScope, TokenError, TokenSigner, CLOCK_SKEW_MS, MIN_SECRET_LEN, TOKEN_VERSION,
};
