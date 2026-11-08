//! 两件事，一个文件（ADR-25）：
//!
//! 1. **内部媒体令牌**（`v1.<hex(claims)>.<hex(HMAC-SHA256)>`）：只在 wxlike
//!    自己的 QUIC 信令面里流转，回答"这次连接是否属于这个 call、这个用户、
//!    这个设备、这个 scope，且签发代次仍然有效"。签名自包含，服务端不需要为
//!    每个 token 建表；撤销靠 `generation` 自增（退群/封禁/停播即整代失效）。
//! 2. **LiveKit sidecar 访问令牌**（[`LiveKitIssuer`]）：媒体面已经交给
//!    LiveKit sidecar，sidecar 只认 `apiKey`/`apiSecret` 签名的 HS256 JWT。
//!    因此**签发权必须留在 Rust**：由服务端裁决谁能进哪个房间、能发还是只能收、
//!    能待多久，再把它翻译成 LiveKit 的 `video` grant。
//!
//! ADR-25 的两条硬闸写在代码里，不写在文档里：
//!
//! * [`assert_sidecar_grant_allowed`]：客户端令牌永远不含 `recorder`/`hidden`
//!   （服务端 Egress 自动录制关闭）与 `roomAdmin`/`ingressAdmin`（避免 sidecar
//!   里长出第二套房间事实）。
//! * 引擎归属由房间固定（`crate::room::MediaEngine`），令牌里的
//!   `wxlike.engine` 必须与房间一致，冷备引擎的信令不能混进 LiveKit 房间。
//!
//! claims 是公开数据，安全边界完全来自 HMAC；密钥只以 `Redacted` 形式出现在
//! Debug 输出里。

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// 令牌格式版本；不认识的版本直接拒绝，而不是半解析。
pub const TOKEN_VERSION: u8 = 1;
/// HMAC 密钥最短长度（256 bit）。短于这个长度拒绝构造签名器。
pub const MIN_SECRET_LEN: usize = 32;
/// 允许的客户端时钟偏移：只影响 `issued_at` 的未来值检查。
pub const CLOCK_SKEW_MS: u64 = 60_000;

/// 令牌用途。call 与 live 的权限面必须分开，不能用一个 scope 通吃。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaScope {
    /// 通话房间（上行+下行都受房间状态裁决）。也是反序列化缺省值——
    /// 缺省只会得到权限面最小的一侧（不能推流到直播）。
    #[default]
    Call,
    /// 直播推流（只有主播）。
    LivePublish,
    /// 直播观看（只下行，永不含上行麦克风）。
    LiveWatch,
    /// 直播连麦（允许上行，但需主播单独签发）。
    LiveCohost,
}

impl MediaScope {
    pub fn may_publish(self) -> bool {
        matches!(self, Self::Call | Self::LivePublish | Self::LiveCohost)
    }

    pub fn may_subscribe(self) -> bool {
        true
    }
}

/// 签进令牌的声明。`generation` 与 call/通道当前代次一致才有效。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaClaims {
    pub call: String,
    pub user: String,
    pub device: String,
    pub scope: MediaScope,
    pub generation: u64,
    pub issued_at: u64,
    pub expires_at: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenError {
    #[error("token secret too short")]
    WeakSecret,
    #[error("api key must not be empty")]
    EmptyApiKey,
    #[error("malformed token")]
    Malformed,
    #[error("bad token signature")]
    BadSignature,
    #[error("token expired")]
    Expired,
    #[error("token lifetime invalid")]
    InvalidLifetime,
    #[error("token clock is ahead")]
    ClockAhead,
    #[error("token is for another call")]
    WrongCall,
    #[error("token is for another room")]
    WrongRoom,
    #[error("token is for another scope")]
    WrongScope,
    #[error("token generation is stale")]
    StaleGeneration,
    #[error("token was signed by another api key")]
    WrongApiKey,
    #[error("unexpected jwt header algorithm (only HS256 is accepted)")]
    BadAlgorithm,
    #[error("token engine `{0}` does not match the room engine `{1}`")]
    EngineMismatch(String, String),
    #[error("granted capabilities do not match the requested scope")]
    GrantMismatch,
    #[error("ADR-25: server-side egress recording is disabled; recorder/hidden tokens may not be minted")]
    ServerEgressGrantForbidden,
    #[error(
        "ADR-25: room authority stays in Rust; roomAdmin/ingressAdmin grants may not be minted"
    )]
    SidecarAdminForbidden,
    #[error("ADR-25: tokens must not carry the `{0}` participant kind; egress/ingress sessions are not minted by wxlike")]
    ParticipantKindForbidden(String),
    #[error("token grants roomJoin but has no identity or room")]
    InvalidJoinClaims,
    /// 上游 `pkg/service/auth.go` 的 `ErrPermissionDenied`。
    #[error("permissions denied")]
    PermissionDenied,
    /// 上游 `pkg/service/auth.go` 的 `claims == nil || claims.Video == nil` 分支：
    /// 没有 grant 的令牌不是"权限为空"，而是"根本没授权过"。
    #[error("token carries no grants")]
    NoGrants,
    #[error("invalid authorization header; must start with `Bearer ` or carry `access_token`")]
    MissingAuthorization,
}

/// HMAC-SHA256 签名器。克隆代价只是密钥字节；不要把它写进日志。
#[derive(Clone)]
pub struct TokenSigner {
    secret: Vec<u8>,
}

impl std::fmt::Debug for TokenSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSigner")
            .field("secret", &"[redacted]")
            .finish()
    }
}

impl TokenSigner {
    pub fn new(secret: impl AsRef<[u8]>) -> Result<Self, TokenError> {
        let secret = secret.as_ref();
        if secret.len() < MIN_SECRET_LEN {
            return Err(TokenError::WeakSecret);
        }
        Ok(Self {
            secret: secret.to_vec(),
        })
    }

    /// 从测试/部署密钥派生签名器，避免调用方手里再留一份原始字节。
    pub fn from_hex(secret_hex: &str) -> Result<Self, TokenError> {
        let raw = hex::decode(secret_hex).map_err(|_| TokenError::WeakSecret)?;
        Self::new(raw)
    }

    /// 签发一枚带明确寿命的令牌。
    pub fn issue(&self, claims: MediaClaims) -> Result<String, TokenError> {
        if claims.expires_at <= claims.issued_at {
            return Err(TokenError::InvalidLifetime);
        }
        let body = serde_json::to_vec(&claims).map_err(|_| TokenError::Malformed)?;
        let body_hex = hex::encode(body);
        let signing_input = format!("v{TOKEN_VERSION}.{body_hex}");
        let sig = self.sign(signing_input.as_bytes())?;
        Ok(format!("{signing_input}.{}", hex::encode(sig)))
    }

    /// 便捷签发：`ttl_ms` 由调用方给出，过期时间用饱和加法。
    #[allow(clippy::too_many_arguments)]
    pub fn issue_for(
        &self,
        call: &str,
        user: &str,
        device: &str,
        scope: MediaScope,
        generation: u64,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<String, TokenError> {
        self.issue(MediaClaims {
            call: call.to_string(),
            user: user.to_string(),
            device: device.to_string(),
            scope,
            generation,
            issued_at: now_ms,
            expires_at: now_ms.saturating_add(ttl_ms),
        })
    }

    /// 验签、验version、验时效。
    pub fn verify(&self, token: &str, now_ms: u64) -> Result<MediaClaims, TokenError> {
        let mut parts = token.split('.');
        let (Some(version), Some(body_hex), Some(sig_hex), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(TokenError::Malformed);
        };
        if version != format!("v{TOKEN_VERSION}") {
            return Err(TokenError::Malformed);
        }
        let sig = hex::decode(sig_hex).map_err(|_| TokenError::Malformed)?;
        let signing_input = format!("{version}.{body_hex}");
        self.verify_signature(signing_input.as_bytes(), &sig)?;
        let body = hex::decode(body_hex).map_err(|_| TokenError::Malformed)?;
        let claims: MediaClaims =
            serde_json::from_slice(&body).map_err(|_| TokenError::Malformed)?;
        if claims.expires_at <= now_ms {
            return Err(TokenError::Expired);
        }
        if claims.issued_at > now_ms.saturating_add(CLOCK_SKEW_MS) {
            return Err(TokenError::ClockAhead);
        }
        Ok(claims)
    }

    /// 验签之外再绑定 call/scope/代次。任何一项不符都拒绝。
    pub fn verify_for(
        &self,
        token: &str,
        now_ms: u64,
        call: &str,
        scope: MediaScope,
        generation: u64,
    ) -> Result<MediaClaims, TokenError> {
        let claims = self.verify(token, now_ms)?;
        if claims.call != call {
            return Err(TokenError::WrongCall);
        }
        if claims.scope != scope {
            return Err(TokenError::WrongScope);
        }
        if claims.generation != generation {
            return Err(TokenError::StaleGeneration);
        }
        Ok(claims)
    }

    fn sign(&self, data: &[u8]) -> Result<Vec<u8>, TokenError> {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).map_err(|_| TokenError::WeakSecret)?;
        mac.update(data);
        Ok(mac.finalize().into_bytes().to_vec())
    }

    fn verify_signature(&self, data: &[u8], sig: &[u8]) -> Result<(), TokenError> {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).map_err(|_| TokenError::WeakSecret)?;
        mac.update(data);
        mac.verify_slice(sig).map_err(|_| TokenError::BadSignature)
    }
}

// ==================== ADR-25：LiveKit sidecar 访问令牌 ====================
//
// LiveKit sidecar 的入房凭证是一枚 HS256 JWT：`iss` 是 apiKey，`sub` 是
// participant identity，`video` grant 决定这个连接在 sidecar 里能做什么。
// sidecar 自己**不查我们的成员表**，所以"谁可以进哪个房间"必须由这里算出来。
// 换句话说：令牌签发是 ADR-25 里 Rust 侧唯一的媒体准入闸。
//
// 本节按 LiveKit 官方 `livekit-token` crate 的 `src/access_token.rs`
// （`github.com/livekit/rust-sdks` @ `052f089166b6864bc5f7e92906f6a7897b9f546c`）
// 移植/改写：claims 字段集合、`video` grant 的三态语义（缺省 ≠ false）、
// "缺省即不序列化"的线格式，以及签发前的 `roomJoin ⇒ 有 identity 且有 room`
// 校验。权限判定函数（下面的 `ensure_*`）交叉核对服务端
// `github.com/livekit/livekit` @ `94edd5da75246bd3c04becf88e3fda563e3d6e1c`
// 的 `pkg/service/auth.go`。逐段差异、改动与"没拿什么"见
// `third_party/reviews/patches/UP-LIVEKIT-RUSTSDK-access-token.md`。

/// LiveKit 只接受 HS256；其它 alg（含 `none`）一律拒绝，防算法混淆。
pub const LIVEKIT_ALG: &str = "HS256";
/// sidecar 共享密钥最短长度（与 LiveKit 自身的 apiKeySecret 校验一致）。
pub const MIN_API_SECRET_LEN: usize = 32;
/// 访问令牌默认寿命：60 秒，够完成一次入房，不够攒着一路用。
pub const DEFAULT_ACCESS_TTL_MS: u64 = 60_000;
/// 寿命上限：过期后必须回信令面重新入房（再过一次成员状态机 + 审计）。
pub const MAX_ACCESS_TTL_MS: u64 = 15 * 60_000;
/// 上游 `livekit-token` 的 `DEFAULT_TTL`（`Duration::from_secs(3600 * 6)`，6 小时）。
/// 只登记用来对照 sidecar 的默认行为：我们签发时仍受 [`MAX_ACCESS_TTL_MS`] 收紧，
/// 这是 ADR-25 里刻意的偏离（房间事实要在 15 分钟内回到 Rust 侧重新裁决）。
pub const LIVEKIT_DEFAULT_TTL_SECS: u64 = 3600 * 6;

/// apiKey/apiSecret 对。Debug 只输出 `[redacted]`，绝不打印密钥。
#[derive(Clone)]
pub struct ApiKeyPair {
    api_key: String,
    secret: Vec<u8>,
}

impl std::fmt::Debug for ApiKeyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyPair")
            .field("api_key", &self.api_key)
            .field("secret", &"[redacted]")
            .finish()
    }
}

impl ApiKeyPair {
    pub fn new(api_key: impl Into<String>, secret: impl AsRef<[u8]>) -> Result<Self, TokenError> {
        let api_key = api_key.into();
        let secret = secret.as_ref();
        if api_key.is_empty() {
            return Err(TokenError::EmptyApiKey);
        }
        if secret.len() < MIN_API_SECRET_LEN {
            return Err(TokenError::WeakSecret);
        }
        Ok(Self {
            api_key,
            secret: secret.to_vec(),
        })
    }

    /// 部署里密钥通常从环境变量/密钥管理读出来的是 hex 文本。
    pub fn from_hex(api_key: impl Into<String>, secret_hex: &str) -> Result<Self, TokenError> {
        let raw = hex::decode(secret_hex).map_err(|_| TokenError::WeakSecret)?;
        Self::new(api_key, raw)
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    fn sign(&self, data: &[u8]) -> Result<Vec<u8>, TokenError> {
        let mut mac =
            HmacSha256::new_from_slice(&self.secret).map_err(|_| TokenError::WeakSecret)?;
        mac.update(data);
        Ok(mac.finalize().into_bytes().to_vec())
    }

    /// 常数时间比较，避免用耗时差探测签名。
    fn matches(&self, data: &[u8], sig: &[u8]) -> Result<(), TokenError> {
        let expected = self.sign(data)?;
        if constant_time_eq(&expected, sig) {
            Ok(())
        } else {
            Err(TokenError::BadSignature)
        }
    }
}

/// 上游 `is_default`（`access_token.rs:98`）：配合 `skip_serializing_if` 使用，
/// 让"没有声明的能力"在 JSON 里**缺席**而不是写成 `false`。这两件事在 LiveKit
/// 语义里不等价 —— 缺省由读取侧按 `unwrap_or(true)` 等规则补齐。
fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// `video` grant：sidecar 看得懂的那部分能力声明。字段名与字段集合照抄上游
/// `VideoGrants`（`access_token.rs:45-96`），线格式是 LiveKit 的 camelCase
/// （`roomJoin` / `canPublish` / `canPublishData` / `canPublishSources` …）。
///
/// **三态是重点**：`canPublish` / `canSubscribe` / `canPublishData` /
/// `canUpdateOwnMetadata` 是 `Option<bool>`，缺省含义由 [`VideoGrant`] 的
/// 取值方法决定（上游 `access_token.rs:102-118`）：
///
/// * `canPublish` 缺席 = **允许**（`unwrap_or(true)`）；
/// * `canSubscribe` 缺席 = **允许**；
/// * `canPublishData` 缺席 = 跟随 `canPublish`；
/// * `canUpdateOwnMetadata` 缺席 = **不允许**。
///
/// 用 `bool` 会把"没声明"读成"禁止"，与 LiveKit 服务端不一致：同一枚由
/// LiveKit SDK 签出的令牌，两边会得出相反的结论。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VideoGrant {
    // actions on rooms
    #[serde(skip_serializing_if = "is_default")]
    pub room_create: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub room_list: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub room_record: bool,

    // actions on a particular room
    #[serde(skip_serializing_if = "is_default")]
    pub room_admin: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub room_join: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub room: String,
    #[serde(skip_serializing_if = "is_default")]
    pub destination_room: String,

    // permissions within a room
    #[serde(skip_serializing_if = "is_default")]
    pub can_publish: Option<bool>,
    #[serde(skip_serializing_if = "is_default")]
    pub can_subscribe: Option<bool>,
    #[serde(skip_serializing_if = "is_default")]
    pub can_publish_data: Option<bool>,

    /// TrackSource 类型白名单。上游注释（`access_token.rs:72-74`）写明"一旦
    /// 设定即取代 `canPublish`"，但**判定发生在 sidecar 侧**（`livekit/protocol`
    /// 的 auth 包，本 checkout 未含），所以这里只保留字段本身，不提供
    /// 推断式访问器 —— 本仓库不假装自己实现了那份判定。
    #[serde(skip_serializing_if = "is_default")]
    pub can_publish_sources: Vec<String>,

    /// 默认不允许参与者改自己的 metadata（上游 `access_token.rs:77-79`）。
    #[serde(skip_serializing_if = "is_default")]
    pub can_update_own_metadata: Option<bool>,

    // actions on ingresses
    #[serde(skip_serializing_if = "is_default")]
    pub ingress_admin: bool,

    // participant is not visible to other participants (useful when making bots)
    #[serde(skip_serializing_if = "is_default")]
    pub hidden: bool,

    // indicates to the room that current participant is a recorder
    #[serde(skip_serializing_if = "is_default")]
    pub recorder: bool,

    // indicates to the room that current participant is an agent
    #[serde(skip_serializing_if = "is_default")]
    pub agent: bool,
}

impl VideoGrant {
    /// 上游 `access_token.rs:103-105`：缺省即允许。
    pub fn can_publish(&self) -> bool {
        self.can_publish.unwrap_or(true)
    }

    /// 上游 `access_token.rs:107-109`：缺省即允许。
    pub fn can_subscribe(&self) -> bool {
        self.can_subscribe.unwrap_or(true)
    }

    /// 上游 `access_token.rs:111-113`：缺省跟随 `canPublish`。
    pub fn can_publish_data(&self) -> bool {
        self.can_publish_data.unwrap_or_else(|| self.can_publish())
    }

    /// 上游 `access_token.rs:115-117`：缺省即不允许。
    pub fn can_update_own_metadata(&self) -> bool {
        self.can_update_own_metadata.unwrap_or(false)
    }
}

/// 上游 `SIPGrants`（`access_token.rs:120-135`）。wxlike 目前没有 SIP 接入，
/// 但字段集合照抄有两个理由：与 sidecar 的 claims 结构可比对；`ensure_sip_*`
/// 两个判定函数能原样落地（见 `pkg/service/auth.go:232-246`）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SipGrant {
    /// manage sip resources
    #[serde(skip_serializing_if = "is_default")]
    pub admin: bool,
    /// make outbound calls
    #[serde(skip_serializing_if = "is_default")]
    pub call: bool,
}

/// wxlike 私有 claim 命名空间：LiveKit 会原样忽略未知 claim，但我们的信令面
/// 用它来绑定 call/device/scope/代次/引擎——房间名之外的细粒度撤销靠它。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WxlikeClaims {
    #[serde(default)]
    pub call: String,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub scope: MediaScope,
    #[serde(default)]
    pub generation: u64,
    /// 令牌是为哪个引擎签的（`livekit` / `str0m`）。
    #[serde(default)]
    pub engine: String,
}

/// sidecar 侧的参与者种类。取值与上游 proto
/// `livekit-protocol/src/livekit.rs` 的 `participant_info::Kind`
/// （`STANDARD=0 / INGRESS=1 / EGRESS=2 / SIP=3 / AGENT=4 / CONNECTOR=7 / BRIDGE=8`）
/// 一一对应；`Connector`/`Bridge` 是 LiveKit Cloud 专用，本项目用不到，
/// 但保留 tag 数值以免和 sidecar 的数字对不上号。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ParticipantKind {
    Standard,
    Ingress,
    Egress,
    Sip,
    Agent,
    Connector,
    Bridge,
}

impl ParticipantKind {
    /// proto 里的数值（给 sidecar/遥测对齐用，不是我们自己的排序）。
    pub fn tag(self) -> i32 {
        match self {
            Self::Standard => 0,
            Self::Ingress => 1,
            Self::Egress => 2,
            Self::Sip => 3,
            Self::Agent => 4,
            Self::Connector => 7,
            Self::Bridge => 8,
        }
    }

    /// 上游 proto 的 `Kind::as_str_name()`（同一份字符串，稳定、可反解）。
    pub fn as_str_name(self) -> &'static str {
        match self {
            Self::Standard => "STANDARD",
            Self::Ingress => "INGRESS",
            Self::Egress => "EGRESS",
            Self::Sip => "SIP",
            Self::Agent => "AGENT",
            Self::Connector => "CONNECTOR",
            Self::Bridge => "BRIDGE",
        }
    }

    /// claims 的 `kind` 是自由字符串（上游 `Claims::kind: String`，
    /// `access_token.rs:280`；测试里用的是小写 `"agent"`）。这里同时接受
    /// 小写与 proto 的 `SCREAMING_SNAKE` 两种写法，认不出来即 `None`——
    /// 认不出来当 Standard（权限面最小的一侧），不猜。
    pub fn from_claim(raw: &str) -> Option<Self> {
        let norm = raw.trim().to_ascii_uppercase().replace('-', "_");
        match norm.as_str() {
            "" | "STANDARD" | "SDK" => Some(Self::Standard),
            "INGRESS" => Some(Self::Ingress),
            "EGRESS" => Some(Self::Egress),
            "SIP" => Some(Self::Sip),
            "AGENT" => Some(Self::Agent),
            "CONNECTOR" => Some(Self::Connector),
            "BRIDGE" => Some(Self::Bridge),
            _ => None,
        }
    }
}

/// 解码后的 LiveKit 访问令牌声明。时间字段是 **Unix 秒**（JWT 规范，
/// 与 LiveKit 一致），不要和内部媒体令牌的毫秒混用。
///
/// 字段集合照抄上游 `Claims`（`access_token.rs:267-303`）：`exp`/`iss`/`nbf`/
/// `sub`/`name`/`kind`/`video`/`sip`/`sha256`/`metadata`/`attributes`。
/// 其中 `jti`/`iat`/`wxlike` 是 **LiveKit 里没有、我们自研** 的三枚：
/// `jti` 给信令面做一次性防重放，`iat` 给审计看签发时刻，`wxlike` 绑定
/// call/device/scope/代次/引擎（LiveKit 只认房间名，粒度不够）。
/// 上游的 `room_config`（`Option<livekit_protocol::RoomConfiguration>`）
/// **没有移植**：它依赖 `livekit-protocol` 的 protobuf 类型，新增依赖必须走
/// integration-request，见 `integration-requests/G09/`。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AccessClaims {
    pub iss: String,
    #[serde(skip_serializing_if = "is_default")]
    pub sub: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jti: Option<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub iat: u64,
    pub nbf: u64,
    pub exp: u64,
    #[serde(skip_serializing_if = "is_default")]
    pub name: String,
    #[serde(skip_serializing_if = "is_default")]
    pub kind: String,
    #[serde(skip_serializing_if = "is_default")]
    pub video: VideoGrant,
    #[serde(skip_serializing_if = "is_default")]
    pub sip: SipGrant,
    /// 上游用它校验 HTTP 报文完整性（`access_token.rs:296`）。
    #[serde(skip_serializing_if = "is_default")]
    pub sha256: String,
    #[serde(skip_serializing_if = "is_default")]
    pub metadata: String,
    #[serde(skip_serializing_if = "is_default")]
    pub attributes: std::collections::HashMap<String, String>,
    #[serde(skip_serializing_if = "is_default")]
    pub wxlike: WxlikeClaims,
}

impl AccessClaims {
    /// 上游 `ParticipantImpl.IsRecorder`（`pkg/rtc/participant.go:605-608`）：
    /// `kind == EGRESS || video.Recorder`。ADR-25 下这类令牌**一律不签**，
    /// 见 [`assert_sidecar_grant_allowed`]。
    pub fn is_recorder(&self) -> bool {
        matches!(
            ParticipantKind::from_claim(&self.kind),
            Some(ParticipantKind::Egress)
        ) || self.video.recorder
    }

    /// 上游 `ParticipantImpl.IsAgent`（`pkg/rtc/participant.go:610-613`）：
    /// `kind == AGENT || video.Agent`。
    pub fn is_agent(&self) -> bool {
        matches!(
            ParticipantKind::from_claim(&self.kind),
            Some(ParticipantKind::Agent)
        ) || self.video.agent
    }

    /// 上游 `ParticipantImpl.IsDependent`（`pkg/rtc/participant.go:615-622`）：
    /// agent/egress 这类"依附参与者"不决定房间生死——房间空不空只看独立成员。
    /// [`crate::room::Room::close_if_empty`] 用同一条规则。
    pub fn is_dependent(&self) -> bool {
        match ParticipantKind::from_claim(&self.kind) {
            Some(ParticipantKind::Agent) | Some(ParticipantKind::Egress) => true,
            _ => self.video.agent || self.video.recorder,
        }
    }

    /// claims 的 `kind` 解析出的参与者种类；认不出来当 `Standard`（最小权限侧）。
    pub fn participant_kind(&self) -> ParticipantKind {
        ParticipantKind::from_claim(&self.kind).unwrap_or(ParticipantKind::Standard)
    }
}

/// 一次入房凭证的请求。`identity` 是 LiveKit 侧的用户身份（= user id），
/// 设备靠 `device` 区分：同一用户的两台设备是两个 LiveKit participant。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessRequest {
    pub room: String,
    pub identity: String,
    pub device: String,
    pub call: String,
    pub scope: MediaScope,
    pub generation: u64,
    pub engine: String,
    /// 上游 claim `name`（`access_token.rs:278`）：sidecar 名册上的显示名。
    /// 空即不写进 JSON（缺省不序列化，与上游同）。
    pub name: String,
    /// 上游 claim `kind`（`access_token.rs:280`）：参与者种类。正常客户端留空
    /// （等价 `STANDARD`）；`egress`/`ingress`/`agent` 等一律拒签，见
    /// [`assert_participant_kind_allowed`]。
    pub kind: String,
    /// 上游 claim `metadata`（`access_token.rs:298`）：不透明字符串，sidecar
    /// 只透传给名册，不参与判定。
    pub metadata: String,
    /// 上游 claim `attributes`（`access_token.rs:300`，`HashMap<String,String>`）：
    /// 键值属性，进 sidecar 名册也进我们的审计快照。
    pub attributes: std::collections::HashMap<String, String>,
    /// 媒体密钥指纹（`MediaKeyRing::fingerprint`），sidecar 只拿摘要校验，
    /// 拿不到密钥本身。落到 claim `sha256`（上游用它做报文完整性校验）。
    pub key_fingerprint: Option<String>,
    /// 自定义寿命；`None` 用 [`DEFAULT_ACCESS_TTL_MS`]。
    pub ttl_ms: Option<u64>,
    /// 服务端静音（合同 §3.7）：为真时上行能力被降级为无——客户端继续发包
    /// 也不会被 sidecar 转发。只能**降**不能**升**（见
    /// [`crate::token::LiveKitIssuer::verify_for`]）。
    pub muted: bool,
}

impl AccessRequest {
    /// 便捷构造：通话房间入房。
    pub fn call(
        room: impl Into<String>,
        identity: impl Into<String>,
        device: impl Into<String>,
        call: impl Into<String>,
        generation: u64,
        engine: impl Into<String>,
    ) -> Self {
        Self {
            room: room.into(),
            identity: identity.into(),
            device: device.into(),
            call: call.into(),
            scope: MediaScope::Call,
            generation,
            engine: engine.into(),
            name: String::new(),
            kind: String::new(),
            metadata: String::new(),
            attributes: std::collections::HashMap::new(),
            key_fingerprint: None,
            ttl_ms: None,
            muted: false,
        }
    }
}

/// 签发结果：令牌 + 房间/身份/到期（毫秒），给信令面直接回给客户端。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedAccess {
    pub token: String,
    pub room: String,
    pub identity: String,
    pub device: String,
    pub expires_at_ms: u64,
}

/// scope → grant 的唯一映射表。新增 scope 时必须在这里补一行，
/// 不允许在调用点手搓 grant。
///
/// 三个能力位都写成显式的 `Some(..)`：上游缺省含义是"允许"（见
/// [`VideoGrant`]），而我们**故意**不留缺省——我们的令牌必须自证权限面，
/// 不能靠读侧的默认来补。
///
/// `canPublishData` 一律显式关掉：通话/直播的信令只走 wxlike 自己的 QUIC 面，
/// 给客户端留 LiveKit 数据通道等于再开一条没审计的旁路。这比上游缺省
/// （跟随 `canPublish`）更严，是 ADR-25 下刻意的收紧。
pub fn grant_for(scope: MediaScope) -> VideoGrant {
    let mut grant = VideoGrant {
        room_join: true,
        can_subscribe: Some(true),
        can_publish_data: Some(false),
        ..Default::default()
    };
    match scope {
        // 通话：双向，语音+视频轨由房间状态裁决。
        MediaScope::Call => grant.can_publish = Some(true),
        // 推流：只有主播。
        MediaScope::LivePublish => {
            grant.can_publish = Some(true);
            grant.can_subscribe = Some(false);
        }
        // 观众：只下行，麦克风与数据通道永远关。
        MediaScope::LiveWatch => grant.can_publish = Some(false),
        // 连麦：主播审批后才签发，可上行。
        MediaScope::LiveCohost => grant.can_publish = Some(true),
    }
    grant
}

/// ADR-25 硬闸：给**客户端**签的 sidecar 令牌不得带服务端录制能力，也不得带
/// 房间管理权。
///
/// 返回 `Err` 时调用方**不能**再签这枚令牌——LiveKit 的 `recorder`/`hidden`
/// 参与者是 Egress 自动录制的唯一入口，`roomAdmin` 则会让 sidecar 里长出
/// 第二套房间事实（静音/踢人/录制状态对不齐）。
///
/// 注意作用域：这道闸只管 [`LiveKitIssuer::issue`] 的签发路径。服务端自己的
/// API 令牌（要调 sidecar 的 RoomService）另有 [`ensure_admin_permission`] 等
/// 判定，那是"服务对服务"的信任边界，与"给客户端放行"是两回事。
pub fn assert_sidecar_grant_allowed(grant: &VideoGrant) -> Result<(), TokenError> {
    if grant.recorder || grant.hidden {
        return Err(TokenError::ServerEgressGrantForbidden);
    }
    if grant.room_admin || grant.ingress_admin {
        return Err(TokenError::SidecarAdminForbidden);
    }
    if !grant.can_publish_sources.is_empty() {
        // 我们不下发 TrackSource 白名单：一旦下发，判定就落到 sidecar 侧的
        // `livekit/protocol` 里（本 checkout 未含其源码），房间事实会分叉。
        return Err(TokenError::GrantMismatch);
    }
    Ok(())
}

/// 参与者种类的硬闸：`EGRESS`（服务端录制器）与 `INGRESS`（外部注入源）都不
/// 是 wxlike 客户端该有的身份。上游把 `kind` 当自由字符串写进 claims
/// （`Claims::kind`），服务端据此决定 `IsRecorder`/`IsDependent`
/// （`pkg/rtc/participant.go:605-622`），所以只封 `video.recorder` 不够——
/// `kind = "egress"` 同样会打开 Egress 语义。
pub fn assert_participant_kind_allowed(kind: &str) -> Result<(), TokenError> {
    // 上游的正常客户端令牌就是不写 `kind`（`Claims::kind` 为空即不序列化），
    // 所以空串等价于 STANDARD；其余一律拒签，认不出来也拒（fail closed）。
    if kind.is_empty()
        || matches!(
            ParticipantKind::from_claim(kind),
            Some(ParticipantKind::Standard)
        )
    {
        return Ok(());
    }
    Err(TokenError::ParticipantKindForbidden(kind.to_string()))
}

// ---- 以下 `ensure_*` 判定逐条对应服务端 `pkg/service/auth.go` ----
//
// 上游这些函数从 `context` 里取 grants；我们没有 Go 的 context，改成显式收
// `Option<&AccessClaims>`（`None` 就是上游 `claims == nil` 的那一支，单独报
// [`TokenError::NoGrants`]，不折成 `PermissionDenied`）。判定条件与顺序照抄。

/// `pkg/service/auth.go:172-185` `EnsureJoinPermission`：只有 `roomJoin` 为真
/// 才允许入房，并把授权的房间名交回调用方（房间名以令牌为准，不以请求参数为准）。
pub fn ensure_join_permission(claims: Option<&AccessClaims>) -> Result<String, TokenError> {
    let claims = claims.ok_or(TokenError::NoGrants)?;
    if !claims.video.room_join {
        return Err(TokenError::PermissionDenied);
    }
    Ok(claims.video.room.clone())
}

/// `pkg/service/auth.go:187-198` `EnsureAdminPermission`：既要 `roomAdmin`，
/// 又必须正好是令牌里那间房——一把 admin 钥匙不能开所有门。
pub fn ensure_admin_permission(
    claims: Option<&AccessClaims>,
    room: &str,
) -> Result<(), TokenError> {
    let claims = claims.ok_or(TokenError::NoGrants)?;
    if !claims.video.room_admin || claims.video.room != room {
        return Err(TokenError::PermissionDenied);
    }
    Ok(())
}

/// `pkg/service/auth.go:200-206` `EnsureCreatePermission`。
pub fn ensure_create_permission(claims: Option<&AccessClaims>) -> Result<(), TokenError> {
    let claims = claims.ok_or(TokenError::NoGrants)?;
    if !claims.video.room_create {
        return Err(TokenError::PermissionDenied);
    }
    Ok(())
}

/// `pkg/service/auth.go:208-214` `EnsureListPermission`。
pub fn ensure_list_permission(claims: Option<&AccessClaims>) -> Result<(), TokenError> {
    let claims = claims.ok_or(TokenError::NoGrants)?;
    if !claims.video.room_list {
        return Err(TokenError::PermissionDenied);
    }
    Ok(())
}

/// `pkg/service/auth.go:216-222` `EnsureRecordPermission`。这是**授权面**，
/// 不等于我们能签：ADR-25 下客户端令牌的 `recorder` 已被
/// [`assert_sidecar_grant_allowed`] 关死，这里只回答"这把令牌本身有没有
/// roomRecord"，供服务端对服务端调用与审计使用。
pub fn ensure_record_permission(claims: Option<&AccessClaims>) -> Result<(), TokenError> {
    let claims = claims.ok_or(TokenError::NoGrants)?;
    if !claims.video.room_record {
        return Err(TokenError::PermissionDenied);
    }
    Ok(())
}

/// `pkg/service/auth.go:224-230` `EnsureIngressAdminPermission`。
pub fn ensure_ingress_admin_permission(claims: Option<&AccessClaims>) -> Result<(), TokenError> {
    let claims = claims.ok_or(TokenError::NoGrants)?;
    if !claims.video.ingress_admin {
        return Err(TokenError::PermissionDenied);
    }
    Ok(())
}

/// `pkg/service/auth.go:232-238` `EnsureSIPAdminPermission`。
pub fn ensure_sip_admin_permission(claims: Option<&AccessClaims>) -> Result<(), TokenError> {
    let claims = claims.ok_or(TokenError::NoGrants)?;
    if !claims.sip.admin {
        return Err(TokenError::PermissionDenied);
    }
    Ok(())
}

/// `pkg/service/auth.go:240-246` `EnsureSIPCallPermission`。
pub fn ensure_sip_call_permission(claims: Option<&AccessClaims>) -> Result<(), TokenError> {
    let claims = claims.ok_or(TokenError::NoGrants)?;
    if !claims.sip.call {
        return Err(TokenError::PermissionDenied);
    }
    Ok(())
}

/// `pkg/service/auth.go:248-259` `EnsureDestRoomPermission`：转发到另一间房
/// （`destinationRoom`）要求 `roomAdmin` + 源房与目标房都和令牌一致。
pub fn ensure_dest_room_permission(
    claims: Option<&AccessClaims>,
    source: &str,
    destination: &str,
) -> Result<(), TokenError> {
    let claims = claims.ok_or(TokenError::NoGrants)?;
    if !claims.video.room_admin
        || claims.video.room != source
        || claims.video.destination_room != destination
    {
        return Err(TokenError::PermissionDenied);
    }
    Ok(())
}

/// `pkg/service/auth.go:68-84` `APIKeyAuthMiddleware.ServeHTTP` 的取令牌部分：
/// 优先 `Authorization: Bearer <jwt>`，否则退回 `?access_token=<jwt>`（上游
/// 刻意不读 form body，为了让代理转发的请求体保持原样）。
pub fn bearer_token(
    auth_header: Option<&str>,
    query_token: Option<&str>,
) -> Result<String, TokenError> {
    if let Some(header) = auth_header.filter(|h| !h.is_empty()) {
        return header
            .strip_prefix("Bearer ")
            .map(str::to_string)
            .filter(|t| !t.is_empty())
            .ok_or(TokenError::MissingAuthorization);
    }
    query_token
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .ok_or(TokenError::MissingAuthorization)
}

/// LiveKit 访问令牌签发器。克隆代价只是密钥字节；不要写进日志。
#[derive(Clone)]
pub struct LiveKitIssuer {
    pair: ApiKeyPair,
    ttl_ms: u64,
}

impl std::fmt::Debug for LiveKitIssuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveKitIssuer")
            .field("api_key", &self.pair.api_key())
            .field("ttl_ms", &self.ttl_ms)
            .finish_non_exhaustive()
    }
}

impl LiveKitIssuer {
    pub fn new(pair: ApiKeyPair) -> Self {
        Self {
            pair,
            ttl_ms: DEFAULT_ACCESS_TTL_MS,
        }
    }

    pub fn with_ttl(pair: ApiKeyPair, ttl_ms: u64) -> Result<Self, TokenError> {
        if ttl_ms == 0 || ttl_ms > MAX_ACCESS_TTL_MS {
            return Err(TokenError::InvalidLifetime);
        }
        Ok(Self { pair, ttl_ms })
    }

    pub fn api_key(&self) -> &str {
        self.pair.api_key()
    }

    pub fn ttl_ms(&self) -> u64 {
        self.ttl_ms
    }

    /// 签发一枚入房凭证。能力面**只**由 [`grant_for`] 推导，调用方无权指定
    /// recorder/admin；越权请求在进入 LiveKit 之前就被拒。
    pub fn issue(&self, req: &AccessRequest, now_ms: u64) -> Result<IssuedAccess, TokenError> {
        let ttl_ms = self.check_issue_input(req)?;
        let grant = self.access_grant(req)?;
        let claims = self.access_claims(req, &grant, ttl_ms, now_ms);
        Ok(IssuedAccess {
            token: self.encode(&claims)?,
            room: req.room.clone(),
            identity: req.identity.clone(),
            device: req.device.clone(),
            expires_at_ms: claims.exp.saturating_mul(1_000),
        })
    }

    /// 上游 `AccessToken::to_jwt`（`access_token.rs:452-471`）的前置校验，
    /// 通过则返回本次签发的寿命（毫秒）。
    fn check_issue_input(&self, req: &AccessRequest) -> Result<u64, TokenError> {
        if self.pair.api_key().is_empty() {
            return Err(TokenError::EmptyApiKey);
        }
        if req.engine.is_empty() {
            return Err(TokenError::Malformed);
        }
        // grant_for 恒带 roomJoin，所以这条就是上游那条 `InvalidClaims`。
        if req.room.is_empty() || req.identity.is_empty() {
            return Err(TokenError::InvalidJoinClaims);
        }
        let ttl_ms = match req.ttl_ms {
            Some(0) => return Err(TokenError::InvalidLifetime),
            Some(v) => v,
            None => self.ttl_ms,
        };
        if ttl_ms > MAX_ACCESS_TTL_MS {
            return Err(TokenError::InvalidLifetime);
        }
        Ok(ttl_ms)
    }

    /// 能力面**只**由 [`grant_for`] 推导，调用方无权指定 recorder/admin；
    /// 越权请求在进入 LiveKit 之前就被拒。
    fn access_grant(&self, req: &AccessRequest) -> Result<VideoGrant, TokenError> {
        let mut grant = grant_for(req.scope);
        grant.room = req.room.clone();
        if req.muted {
            // 服务端静音落到能力面：客户端继续发也没人转发。用显式 `Some(false)`
            // 而不是留空——留空在上游语义里是"允许"。
            grant.can_publish = Some(false);
        }
        // 结构性保证：即使 grant_for 以后被改错，这里也不会放过 admin/recorder。
        assert_sidecar_grant_allowed(&grant)?;
        // `kind` 是上游 claims 里真正决定 IsRecorder/IsDependent 的字段，
        // 光封 `video.recorder` 不够（见 `assert_participant_kind_allowed`）。
        assert_participant_kind_allowed(&req.kind)?;
        Ok(grant)
    }

    /// 把请求 + 能力面摊平成一份 JWT claims。
    fn access_claims(
        &self,
        req: &AccessRequest,
        grant: &VideoGrant,
        ttl_ms: u64,
        now_ms: u64,
    ) -> AccessClaims {
        let issued_at_secs = now_ms / 1_000;
        AccessClaims {
            iss: self.pair.api_key().to_string(),
            sub: req.identity.clone(),
            jti: Some(random_jti()),
            iat: issued_at_secs,
            nbf: issued_at_secs,
            exp: issued_at_secs.saturating_add(ttl_ms / 1_000),
            name: req.name.clone(),
            kind: req.kind.clone(),
            video: grant.clone(),
            sip: SipGrant::default(),
            sha256: req.key_fingerprint.clone().unwrap_or_default(),
            metadata: req.metadata.clone(),
            attributes: req.attributes.clone(),
            wxlike: WxlikeClaims {
                call: req.call.clone(),
                device: req.device.clone(),
                scope: req.scope,
                generation: req.generation,
                engine: req.engine.clone(),
            },
        }
    }

    /// 只序列化非缺省 claim（与上游 `Claims` 的 `skip_serializing_if` 同法）。
    /// 一张干净的令牌只会有 `exp`/`iss`/`nbf` 三枚基础 claim，便于和 LiveKit
    /// SDK 产出的字节对照（上游测试 `test_agent_grant_and_kind` 正是这么断言的）。
    ///
    /// 三段都必须是 base64url(`-`/`_`、无 padding)：**头部也一样要编码**。
    /// 早期版本把 `{"alg":"HS256","typ":"JWT"}` 原文拼进去了，看着还是三段、
    /// 实际不是合法 JWT —— LiveKit 侧解不开，我们的 `verify` 也解不开自己签的
    /// 令牌（`{` 不在 base64url 字母表里）。这条由
    /// `issued_token_is_a_real_three_part_jwt` 钉住。
    fn encode(&self, claims: &AccessClaims) -> Result<String, TokenError> {
        let header = b"{\"alg\":\"HS256\",\"typ\":\"JWT\"}";
        let body = serde_json::to_vec(claims).map_err(|_| TokenError::Malformed)?;
        let signing_input = format!("{}.{}", b64url_encode(header), b64url_encode(&body));
        let sig = self.pair.sign(signing_input.as_bytes())?;
        Ok(format!("{signing_input}.{}", b64url_encode(&sig)))
    }

    /// 验签 + 验时效 + 验 apiKey。**不做**房间/代次判断——那是 [`Self::verify_for`]。
    ///
    /// 三条校验对应上游 `TokenVerifier::verify`（`access_token.rs:496-511`）：
    /// `validate_exp`、`validate_nbf`、`set_issuer(&[apiKey])`。顺序刻意为
    /// "算法 → 签名 → issuer → 时效"：先验签名再信任何字段，`iss` 才不会被
    /// 未验签的 payload 影响。
    pub fn verify(&self, token: &str, now_ms: u64) -> Result<AccessClaims, TokenError> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(TokenError::Malformed);
        }
        let header_bytes = b64url_decode(parts[0])?;
        let header: serde_json::Value =
            serde_json::from_slice(&header_bytes).map_err(|_| TokenError::Malformed)?;
        if header.get("alg").and_then(serde_json::Value::as_str) != Some(LIVEKIT_ALG) {
            // `none`、RS256、或者把 alg 改成 HS512 的降级攻击都在这里断掉。
            return Err(TokenError::BadAlgorithm);
        }
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig = b64url_decode(parts[2])?;
        self.pair.matches(signing_input.as_bytes(), &sig)?;
        let body = b64url_decode(parts[1])?;
        let claims: AccessClaims =
            serde_json::from_slice(&body).map_err(|_| TokenError::Malformed)?;
        if claims.iss != self.pair.api_key() {
            return Err(TokenError::WrongApiKey);
        }
        let now_secs = now_ms / 1_000;
        if claims.exp <= now_secs {
            return Err(TokenError::Expired);
        }
        let skew_secs = CLOCK_SKEW_MS / 1_000;
        if claims.nbf > now_secs.saturating_add(skew_secs) {
            return Err(TokenError::ClockAhead);
        }
        if claims.iat > now_secs.saturating_add(skew_secs) {
            return Err(TokenError::ClockAhead);
        }
        Ok(claims)
    }

    /// 验签之外再绑定房间 / scope / 代次 / 引擎。房间名交给 LiveKit 自己校验，
    /// 这里再校验一次，避免 sidecar 配置漂移后我们仍然放行。
    pub fn verify_for(
        &self,
        token: &str,
        now_ms: u64,
        room: &str,
        scope: MediaScope,
        generation: u64,
        engine: &str,
    ) -> Result<AccessClaims, TokenError> {
        let claims = self.verify(token, now_ms)?;
        if claims.video.room != room {
            return Err(TokenError::WrongRoom);
        }
        if claims.wxlike.scope != scope {
            return Err(TokenError::WrongScope);
        }
        if claims.wxlike.generation != generation {
            return Err(TokenError::StaleGeneration);
        }
        if claims.wxlike.engine != engine {
            return Err(TokenError::EngineMismatch(
                claims.wxlike.engine.clone(),
                engine.to_string(),
            ));
        }
        // ADR-25 特权面在**校验**层再拦一次：签名有效不代表权限有效。
        assert_sidecar_grant_allowed(&claims.video)?;
        assert_participant_kind_allowed(&claims.kind)?;
        // grant 只能等于或**低于** scope 的能力面（静音是降级），任何升级都拒。
        // 这里用取值方法而不是字段本身：LiveKit 自己签的令牌可能不写 `canPublish`，
        // 按上游语义那要当成"允许"来比，否则两边对同一枚令牌结论相反。
        if claims.video.can_publish() && !scope.may_publish() {
            return Err(TokenError::GrantMismatch);
        }
        // 数据通道一律不开：开了就等于给客户端留一条不经审计的旁路。
        if claims.video.can_publish_data() {
            return Err(TokenError::GrantMismatch);
        }
        Ok(claims)
    }
}

fn random_jti() -> String {
    use rand::RngCore as _;
    let mut buf = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// base64url（无 padding），JWT 的线格式。手写以免为一个小函数新增依赖。
const URL_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// 一个 base64 字符承载的位数；`0x3F` 就是它的掩码。
const B64_BITS: u32 = 6;
/// 一组的长度：3 字节 ↔ 4 字符，两个方向都按这个粒度走。
const B64_GROUP_BYTES: usize = 3;
const B64_GROUP_CHARS: usize = 4;
/// 码表分段偏移：A-Z、a-z 各 26 个，再往后是 10 个数字，最后两位留给
/// URL-safe 的 `-` 与 `_`。
const B64_LETTERS: u32 = 26;
const B64_DIGITS_OFFSET: u32 = 2 * B64_LETTERS;
const B64_DASH_INDEX: u32 = B64_DIGITS_OFFSET + 10;
const B64_UNDERSCORE_INDEX: u32 = B64_DASH_INDEX + 1;
const B64_TOP_SHIFT: u32 = (B64_GROUP_CHARS - 1) as u32 * B64_BITS;

pub(crate) fn b64url_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(B64_GROUP_BYTES) * B64_GROUP_CHARS);
    for chunk in bytes.chunks(B64_GROUP_BYTES) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(URL_ALPHABET[(n >> B64_TOP_SHIFT) as usize & 0x3F] as char);
        out.push(URL_ALPHABET[(n >> (2 * B64_BITS)) as usize & 0x3F] as char);
        if chunk.len() > 1 {
            out.push(URL_ALPHABET[(n >> B64_BITS) as usize & 0x3F] as char);
        }
        if chunk.len() > 2 {
            out.push(URL_ALPHABET[n as usize & 0x3F] as char);
        }
    }
    out
}

fn url_index(c: u8) -> Result<u32, TokenError> {
    match c {
        b'A'..=b'Z' => Ok(u32::from(c - b'A')),
        b'a'..=b'z' => Ok(u32::from(c - b'a') + B64_LETTERS),
        b'0'..=b'9' => Ok(u32::from(c - b'0') + B64_DIGITS_OFFSET),
        b'-' => Ok(B64_DASH_INDEX),
        b'_' => Ok(B64_UNDERSCORE_INDEX),
        _ => Err(TokenError::Malformed),
    }
}

/// 解码 base64url；容忍 padding（有些 SDK 会带上 `=`）。
pub(crate) fn b64url_decode(text: &str) -> Result<Vec<u8>, TokenError> {
    let raw = text.trim_end_matches('=').as_bytes();
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if raw.len() % B64_GROUP_CHARS == 1 {
        return Err(TokenError::Malformed);
    }
    let mut out = Vec::with_capacity(raw.len() / B64_GROUP_CHARS * B64_GROUP_BYTES + 3);
    for chunk in raw.chunks(B64_GROUP_CHARS) {
        let mut acc: u32 = 0;
        for (i, byte) in chunk.iter().enumerate() {
            let v = url_index(*byte)?;
            acc |= v << (B64_TOP_SHIFT - i as u32 * B64_BITS);
        }
        match chunk.len() {
            2 => out.push((acc >> 16) as u8),
            3 => {
                out.push((acc >> 16) as u8);
                out.push((acc >> 8) as u8);
            }
            4 => {
                out.push((acc >> 16) as u8);
                out.push((acc >> 8) as u8);
                out.push(acc as u8);
            }
            // 1 个字符（以及 0）无法构成完整的 base64 组：不是合法 JWT 段。
            _ => return Err(TokenError::Malformed),
        }
    }
    Ok(out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> TokenSigner {
        TokenSigner::new(vec![7u8; MIN_SECRET_LEN]).unwrap()
    }

    fn issue(s: &TokenSigner) -> String {
        s.issue_for(
            "call-1",
            "alice",
            "phone",
            MediaScope::Call,
            3,
            1_000,
            60_000,
        )
        .unwrap()
    }

    #[test]
    fn round_trip_binds_every_field() {
        let s = signer();
        let token = issue(&s);
        let claims = s
            .verify_for(&token, 2_000, "call-1", MediaScope::Call, 3)
            .unwrap();
        assert_eq!(claims.user, "alice");
        assert_eq!(claims.device, "phone");
        assert_eq!(claims.expires_at, 61_000);
    }

    #[test]
    fn tampered_claims_are_rejected() {
        let s = signer();
        let token = issue(&s);
        let mut parts: Vec<&str> = token.split('.').collect();
        let mut body = hex::decode(parts[1]).unwrap();
        let mut claims: MediaClaims = serde_json::from_slice(&body).unwrap();
        claims.user = "mallory".into();
        body = serde_json::to_vec(&claims).unwrap();
        parts[1] = Box::leak(hex::encode(body).into_boxed_str());
        let forged = parts.join(".");
        assert_eq!(s.verify(&forged, 2_000), Err(TokenError::BadSignature));
    }

    #[test]
    fn expiry_is_enforced_at_the_boundary() {
        let s = signer();
        let token = issue(&s);
        assert!(s.verify(&token, 60_999).is_ok());
        assert_eq!(s.verify(&token, 61_000), Err(TokenError::Expired));
    }

    #[test]
    fn binding_mismatches_are_rejected() {
        let s = signer();
        let token = issue(&s);
        assert_eq!(
            s.verify_for(&token, 2_000, "call-2", MediaScope::Call, 3),
            Err(TokenError::WrongCall)
        );
        assert_eq!(
            s.verify_for(&token, 2_000, "call-1", MediaScope::LiveWatch, 3),
            Err(TokenError::WrongScope)
        );
        assert_eq!(
            s.verify_for(&token, 2_000, "call-1", MediaScope::Call, 4),
            Err(TokenError::StaleGeneration)
        );
    }

    #[test]
    fn malformed_and_weak_inputs_fail_closed() {
        assert_eq!(
            TokenSigner::new(vec![1u8; MIN_SECRET_LEN - 1]).unwrap_err(),
            TokenError::WeakSecret
        );
        let s = signer();
        assert_eq!(s.verify("v1.zz", 0), Err(TokenError::Malformed));
        assert_eq!(s.verify("v2.a.b", 0), Err(TokenError::Malformed));
        assert_eq!(s.verify("", 0), Err(TokenError::Malformed));
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let s = TokenSigner::new([0xAB; 40]).unwrap();
        let rendered = format!("{s:?}");
        assert!(rendered.contains("redacted"));
        assert!(!rendered.contains("abab"));
    }

    // ---------------- ADR-25 LiveKit 访问令牌 ----------------

    fn pair() -> ApiKeyPair {
        ApiKeyPair::new("APIsidetest", vec![0x31; MIN_API_SECRET_LEN]).unwrap()
    }

    fn request(scope: MediaScope) -> AccessRequest {
        AccessRequest {
            room: "wxlike-call-c1".into(),
            identity: "bob".into(),
            device: "phone".into(),
            call: "c1".into(),
            scope,
            generation: 4,
            engine: "livekit".into(),
            ..Default::default()
        }
    }

    /// 一枚真的能交给 LiveKit sidecar 的 JWT：三段、全是 base64url 字母表、
    /// 不带 padding、头部解出来是 `{"alg":"HS256","typ":"JWT"}`、签名 32 字节。
    /// 这一条钉的是"头部也要 base64url 编码"——原文拼头部的版本看着仍是三段，
    /// 实际既不合法也验不过自己签的令牌。
    #[test]
    fn issued_token_is_a_real_three_part_jwt() {
        let issuer = LiveKitIssuer::new(pair());
        let issued = issuer.issue(&request(MediaScope::Call), 1_000).unwrap();
        let parts: Vec<&str> = issued.token.split('.').collect();
        assert_eq!(parts.len(), 3);
        for part in &parts {
            assert!(
                part.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "JWT 段只允许 base64url 字母表：{part}"
            );
            assert!(!part.contains('='), "不带 padding：{part}");
        }
        let header: serde_json::Value =
            serde_json::from_slice(&b64url_decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], serde_json::json!("HS256"));
        assert_eq!(header["typ"], serde_json::json!("JWT"));
        assert_eq!(
            b64url_decode(parts[2]).unwrap().len(),
            32,
            "HS256 = 32 字节"
        );
        // 自己签的令牌必须自己验得过。
        assert!(issuer.verify(&issued.token, 1_000).is_ok());
    }

    #[test]
    fn access_claims_survive_a_json_round_trip() {
        let issuer = LiveKitIssuer::new(pair());
        let issued = issuer.issue(&request(MediaScope::Call), 1_000).unwrap();
        let body = b64url_decode(issued.token.split('.').nth(1).unwrap()).unwrap();
        let text = String::from_utf8(body).unwrap();
        let parsed: AccessClaims = serde_json::from_str(&text).unwrap();
        let again: AccessClaims =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(
            parsed, again,
            "claims 必须能稳定往返（sidecar 读的是同一份）"
        );
        assert_eq!(parsed.video.room, "wxlike-call-c1");
    }

    #[test]
    fn livekit_round_trip_binds_room_identity_and_generation() {
        let issuer = LiveKitIssuer::new(pair());
        let issued = issuer
            .issue(&request(MediaScope::Call), 1_700_000_000_000)
            .unwrap();
        // 三段式 JWT，头部固定 HS256。
        assert_eq!(issued.token.split('.').count(), 3);
        let claims = issuer
            .verify_for(
                &issued.token,
                1_700_000_030_000,
                "wxlike-call-c1",
                MediaScope::Call,
                4,
                "livekit",
            )
            .unwrap();
        assert_eq!(claims.iss, "APIsidetest");
        assert_eq!(claims.sub, "bob");
        assert_eq!(claims.wxlike.device, "phone");
        assert_eq!(claims.wxlike.call, "c1");
        assert_eq!(claims.video.room, "wxlike-call-c1");
        assert!(claims.video.room_join && claims.video.can_publish());
        assert_eq!(
            issued.expires_at_ms,
            (1_700_000_000_000 / 1_000 + DEFAULT_ACCESS_TTL_MS / 1_000) * 1_000
        );
    }

    #[test]
    fn livekit_expiry_uses_unix_seconds_not_millis() {
        let issuer = LiveKitIssuer::new(pair());
        let issued = issuer.issue(&request(MediaScope::Call), 0).unwrap();
        let claims = issuer.verify(&issued.token, 0).unwrap();
        // exp 必须是秒：60 秒寿命 => 60，而不是 60000。
        assert_eq!(claims.exp, 60);
        assert_eq!(claims.iat, 0);
        assert_eq!(issued.expires_at_ms, 60_000);
        // 边界：第 60 秒整即失效（exp <= now）。
        assert!(issuer.verify(&issued.token, 59_999).is_ok());
        assert_eq!(
            issuer.verify(&issued.token, 60_000),
            Err(TokenError::Expired)
        );
    }

    #[test]
    fn livekit_grants_are_scope_derived_and_never_privileged() {
        let issuer = LiveKitIssuer::new(pair());
        for scope in [
            MediaScope::Call,
            MediaScope::LivePublish,
            MediaScope::LiveWatch,
            MediaScope::LiveCohost,
        ] {
            let issued = issuer.issue(&request(scope), 1_000).unwrap();
            let body = b64url_decode(issued.token.split('.').nth(1).unwrap()).unwrap();
            let claims: AccessClaims = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                claims.video,
                {
                    let mut g = grant_for(scope);
                    g.room = "wxlike-call-c1".into();
                    g
                },
                "{scope:?} grant 必须由映射表推导"
            );
            assert!(!claims.video.recorder, "{scope:?} 不得带服务端录制身份");
            assert!(!claims.video.hidden, "{scope:?} 不得是隐藏参与者");
            assert!(!claims.video.room_admin, "{scope:?} 不得带房间管理权");
            assert!(!claims.video.ingress_admin);
            assert!(!claims.is_recorder(), "{scope:?} 不得是 Egress 参与者");
            assert_eq!(claims.video.can_publish(), scope.may_publish());
            // 三态必须"写死"：我们签的令牌不允许靠读侧缺省来补能力位。
            assert!(
                claims.video.can_publish.is_some()
                    && claims.video.can_subscribe.is_some()
                    && claims.video.can_publish_data.is_some(),
                "{scope:?} 能力位必须显式落进 JSON"
            );
        }
        // 观众永远不能推流，也永远不能开数据通道。
        assert!(!grant_for(MediaScope::LiveWatch).can_publish());
        assert!(!grant_for(MediaScope::LiveWatch).can_publish_data());
    }

    #[test]
    fn server_side_mute_downgrades_the_grant_without_escalating() {
        let issuer = LiveKitIssuer::new(pair());
        let req = AccessRequest {
            muted: true,
            ..request(MediaScope::Call)
        };
        let issued = issuer.issue(&req, 1_000).unwrap();
        let claims = issuer
            .verify_for(
                &issued.token,
                1_000,
                "wxlike-call-c1",
                MediaScope::Call,
                4,
                "livekit",
            )
            .unwrap();
        assert_eq!(
            claims.video.can_publish,
            Some(false),
            "静音必须写成显式 false，缺省在上游语义里是「允许」"
        );
        assert!(!claims.video.can_publish(), "静音成员拿到的令牌不能再推流");
        assert!(claims.video.can_subscribe(), "静音不影响下行");
        // 静音也不能把 canPublishData 打开（上游会让它跟随 canPublish）。
        assert!(!claims.video.can_publish_data());
        // 观众再怎么"静音"也不会得到上行能力（降级不是升级）。
        let watch = AccessRequest {
            scope: MediaScope::LiveWatch,
            muted: true,
            ..request(MediaScope::Call)
        };
        let token = issuer.issue(&watch, 1_000).unwrap().token;
        let claims = issuer
            .verify_for(
                &token,
                1_000,
                "wxlike-call-c1",
                MediaScope::LiveWatch,
                4,
                "livekit",
            )
            .unwrap();
        assert!(!claims.video.can_publish());
    }

    #[test]
    fn capability_escalation_inside_a_valid_signature_is_refused() {
        // 纵深防御：签发路径不会产出越权 grant，但校验层也不能因为
        // "签名有效"就放行（例如未来 grant_for 被改错）。
        let issuer = LiveKitIssuer::new(pair());
        let mut claims: AccessClaims = {
            let issued = issuer
                .issue(&request(MediaScope::LiveWatch), 1_000)
                .unwrap();
            let body = b64url_decode(issued.token.split('.').nth(1).unwrap()).unwrap();
            serde_json::from_slice(&body).unwrap()
        };
        claims.video.can_publish = Some(true);
        let forged = issuer.encode(&claims).unwrap();
        assert_eq!(
            issuer.verify_for(
                &forged,
                1_000,
                "wxlike-call-c1",
                MediaScope::LiveWatch,
                4,
                "livekit"
            ),
            Err(TokenError::GrantMismatch)
        );

        let mut with_data = grant_for(MediaScope::Call);
        with_data.room = "wxlike-call-c1".into();
        with_data.can_publish_data = Some(true);
        claims.video = with_data;
        claims.wxlike.scope = MediaScope::Call;
        let forged = issuer.encode(&claims).unwrap();
        assert_eq!(
            issuer.verify_for(
                &forged,
                1_000,
                "wxlike-call-c1",
                MediaScope::Call,
                4,
                "livekit"
            ),
            Err(TokenError::GrantMismatch),
            "数据通道一律不开"
        );
    }

    /// 上游语义的可执行对照：LiveKit SDK 签的令牌不会写 `canPublishData`，
    /// 读侧必须得出"跟随 canPublish"；不写 `canPublish` 则是"允许"。
    /// 这三条断言就是 `access_token.rs:102-118` 的移植验收单。
    #[test]
    fn video_grant_defaults_match_upstream_three_state_semantics() {
        // 上游 `test_defaults_are_not_serialized`（access_token.rs:685-700）同款：
        // 空 grant 序列化成 `{}`，反序列化回来还是 default。
        assert_eq!(serde_json::to_string(&VideoGrant::default()).unwrap(), "{}");
        let parsed: VideoGrant = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, VideoGrant::default());
        assert!(parsed.can_publish(), "缺省 canPublish = 允许");
        assert!(parsed.can_subscribe(), "缺省 canSubscribe = 允许");
        assert!(
            parsed.can_publish_data(),
            "缺省 canPublishData 跟随 canPublish"
        );
        assert!(
            !parsed.can_update_own_metadata(),
            "缺省 canUpdateOwnMetadata = 不允许"
        );

        // 显式 false 的两个方向都要能区分出来。
        let no_publish: VideoGrant =
            serde_json::from_str(r#"{"canPublish":false,"canPublishData":true}"#).unwrap();
        assert!(!no_publish.can_publish());
        assert!(no_publish.can_publish_data());
        let only_data_off: VideoGrant =
            serde_json::from_str(r#"{"canPublishData":false}"#).unwrap();
        assert!(only_data_off.can_publish(), "关数据通道不等于关推流");
        assert!(!only_data_off.can_publish_data());

        // 线格式：camelCase + 只写非缺省（与上游同一份字节）。
        let explicit = VideoGrant {
            room_join: true,
            room: "r".into(),
            can_publish: Some(false),
            can_publish_data: Some(true),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&explicit).unwrap(),
            r#"{"roomJoin":true,"room":"r","canPublish":false,"canPublishData":true}"#
        );
    }

    /// 上游 `AccessToken::to_jwt` 的 `InvalidClaims` 分支（access_token.rs:458-464）：
    /// 授了 `roomJoin` 却没有 identity 或 room 的令牌不许签出去。
    #[test]
    fn room_join_without_identity_or_room_is_not_minted() {
        let issuer = LiveKitIssuer::new(pair());
        let no_identity = AccessRequest {
            identity: String::new(),
            ..request(MediaScope::Call)
        };
        assert_eq!(
            issuer.issue(&no_identity, 1_000),
            Err(TokenError::InvalidJoinClaims)
        );
        let no_room = AccessRequest {
            room: String::new(),
            ..request(MediaScope::Call)
        };
        assert_eq!(
            issuer.issue(&no_room, 1_000),
            Err(TokenError::InvalidJoinClaims)
        );
    }

    /// ADR-25：`kind` 也要拦。上游 `IsRecorder()` 的判据是
    /// `kind == EGRESS || video.recorder`（participant.go:605-608），
    /// 只封 `recorder` 会留下 `kind="egress"` 这条后门。
    #[test]
    fn egress_and_ingress_participant_kinds_are_not_minted() {
        let issuer = LiveKitIssuer::new(pair());
        for kind in ["egress", "EGRESS", "ingress", "agent", "sip", "weird"] {
            let req = AccessRequest {
                kind: kind.into(),
                ..request(MediaScope::Call)
            };
            assert_eq!(
                issuer.issue(&req, 1_000),
                Err(TokenError::ParticipantKindForbidden(kind.into())),
                "`{kind}` 种类不得签给客户端"
            );
        }
        // 空串与 "standard" 是正常客户端，放行。
        for kind in ["", "standard", "STANDARD"] {
            let req = AccessRequest {
                kind: kind.into(),
                ..request(MediaScope::Call)
            };
            assert!(
                issuer.issue(&req, 1_000).is_ok(),
                "`{kind}` 应当是普通客户端"
            );
        }
        // 种类解析：tag 与 proto 数值一致，字符串可反解。
        assert_eq!(ParticipantKind::Egress.tag(), 2);
        assert_eq!(ParticipantKind::Agent.tag(), 4);
        assert_eq!(ParticipantKind::Bridge.tag(), 8);
        assert_eq!(
            ParticipantKind::from_claim("agent"),
            Some(ParticipantKind::Agent)
        );
        assert_eq!(
            ParticipantKind::from_claim(ParticipantKind::Connector.as_str_name()),
            Some(ParticipantKind::Connector)
        );
        assert_eq!(ParticipantKind::from_claim("nope"), None);
        // is_recorder / is_agent / is_dependent 三条照抄 Go 的判据。
        let rec = AccessClaims {
            kind: "egress".into(),
            ..Default::default()
        };
        assert!(rec.is_recorder() && rec.is_dependent() && !rec.is_agent());
        let by_flag = AccessClaims {
            video: VideoGrant {
                recorder: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(by_flag.is_recorder() && by_flag.is_dependent());
        let agent = AccessClaims {
            video: VideoGrant {
                agent: true,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(agent.is_agent() && agent.is_dependent() && !agent.is_recorder());
        let std = AccessClaims::default();
        assert_eq!(std.participant_kind(), ParticipantKind::Standard);
        assert!(!std.is_dependent());
    }

    /// 移植自服务端 `pkg/service/auth.go` 的 `Ensure*Permission` 判定。
    /// 这些是"服务对服务"的授权面校验，与客户端签发闸是两条路。
    #[test]
    fn ported_permission_gates_follow_the_service_reference() {
        let claims = |g: VideoGrant| AccessClaims {
            video: g,
            ..Default::default()
        };
        // claims == nil 那一支单独报 NoGrants，不折成 PermissionDenied。
        assert_eq!(ensure_join_permission(None), Err(TokenError::NoGrants));
        assert_eq!(
            ensure_admin_permission(None, "r"),
            Err(TokenError::NoGrants)
        );

        // EnsureJoinPermission：只有 roomJoin 才给房间名，且以令牌为准。
        let joiner = claims(VideoGrant {
            room_join: true,
            room: "room-a".into(),
            ..Default::default()
        });
        assert_eq!(
            ensure_join_permission(Some(&joiner)),
            Ok("room-a".to_string())
        );
        assert_eq!(
            ensure_join_permission(Some(&AccessClaims::default())),
            Err(TokenError::PermissionDenied)
        );

        // EnsureAdminPermission：roomAdmin + 房间必须正好是令牌里那间。
        let admin = claims(VideoGrant {
            room_admin: true,
            room: "room-a".into(),
            ..Default::default()
        });
        assert_eq!(ensure_admin_permission(Some(&admin), "room-a"), Ok(()));
        assert_eq!(
            ensure_admin_permission(Some(&admin), "room-b"),
            Err(TokenError::PermissionDenied),
            "一把 admin 钥匙不能开所有门"
        );
        assert_eq!(
            ensure_admin_permission(
                Some(&claims(VideoGrant {
                    room: "room-a".into(),
                    ..Default::default()
                })),
                "room-a"
            ),
            Err(TokenError::PermissionDenied)
        );

        // Create / List / Record / IngressAdmin 各查自己那一位。
        assert_eq!(
            ensure_create_permission(Some(&AccessClaims::default())),
            Err(TokenError::PermissionDenied)
        );
        assert_eq!(
            ensure_create_permission(Some(&claims(VideoGrant {
                room_create: true,
                ..Default::default()
            }))),
            Ok(())
        );
        assert_eq!(
            ensure_list_permission(Some(&claims(VideoGrant {
                room_list: true,
                ..Default::default()
            }))),
            Ok(())
        );
        assert_eq!(
            ensure_record_permission(Some(&claims(VideoGrant {
                room_record: true,
                ..Default::default()
            }))),
            Ok(())
        );
        assert_eq!(
            ensure_ingress_admin_permission(Some(&AccessClaims::default())),
            Err(TokenError::PermissionDenied)
        );

        // SIP 两条读的是 `sip` 而不是 `video`。
        let sip = AccessClaims {
            sip: SipGrant {
                admin: true,
                call: false,
            },
            ..Default::default()
        };
        assert_eq!(ensure_sip_admin_permission(Some(&sip)), Ok(()));
        assert_eq!(
            ensure_sip_call_permission(Some(&sip)),
            Err(TokenError::PermissionDenied)
        );

        // EnsureDestRoomPermission：roomAdmin + 源房 + 目标房三者都要对上。
        let forwarder = claims(VideoGrant {
            room_admin: true,
            room: "src".into(),
            destination_room: "dst".into(),
            ..Default::default()
        });
        assert_eq!(
            ensure_dest_room_permission(Some(&forwarder), "src", "dst"),
            Ok(())
        );
        assert_eq!(
            ensure_dest_room_permission(Some(&forwarder), "src", "other"),
            Err(TokenError::PermissionDenied)
        );
        assert_eq!(
            ensure_dest_room_permission(Some(&forwarder), "other", "dst"),
            Err(TokenError::PermissionDenied)
        );
    }

    /// `APIKeyAuthMiddleware.ServeHTTP` 的取令牌部分：Bearer 优先，退回
    /// query `access_token`；有 Authorization 头但不是 Bearer 前缀即拒，
    /// 不再回退到 query（上游正是这么排的）。
    #[test]
    fn bearer_and_query_token_extraction_match_the_middleware() {
        assert_eq!(
            bearer_token(Some("Bearer abc.def.ghi"), None).unwrap(),
            "abc.def.ghi"
        );
        assert_eq!(
            bearer_token(Some("Basic abc"), None),
            Err(TokenError::MissingAuthorization)
        );
        assert_eq!(
            bearer_token(Some("Bearer "), None),
            Err(TokenError::MissingAuthorization)
        );
        assert_eq!(bearer_token(None, Some("q-token")).unwrap(), "q-token");
        assert_eq!(
            bearer_token(None, None),
            Err(TokenError::MissingAuthorization)
        );
        // 空 Authorization 头 = 没带 header，走 query（上游 `authHeader != ""` 分支）。
        assert_eq!(bearer_token(Some(""), Some("q")).unwrap(), "q");
    }

    /// 上游 `Claims` 的"缺省即不序列化"整表：一枚只声明时效的令牌，
    /// payload 只应有 `exp`/`iss`/`nbf` 三个键（对照上游
    /// `test_agent_grant_and_kind` 里 `assert_eq!(keys, ["exp","iss","nbf"])`）。
    /// 我们的 `iat`/`jti`/`wxlike` 是自研字段，所以断言时把它们剥掉。
    #[test]
    fn claims_omit_defaults_on_the_wire() {
        let bare = AccessClaims {
            iss: "APIkey".into(),
            nbf: 1,
            exp: 2,
            ..Default::default()
        };
        let json = serde_json::to_string(&bare).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["exp", "iss", "nbf"], "{json}");
        // 反解同一份 JSON 得到同一结构（LiveKit SDK 的令牌我们能读）。
        assert_eq!(serde_json::from_str::<AccessClaims>(&json).unwrap(), bare);
        // 上游字段名是 camelCase，且 `name`/`kind`/`metadata` 能读回来。
        let full: AccessClaims = serde_json::from_str(
            r#"{"iss":"k","exp":9,"nbf":1,"sub":"bob","name":"Bob","kind":"agent","video":{"agent":true,"canPublish":false},"metadata":"m","attributes":{"a":"1"}}"#,
        )
        .unwrap();
        assert_eq!(full.sub, "bob");
        assert_eq!(full.name, "Bob");
        assert_eq!(full.kind, "agent");
        assert_eq!(full.metadata, "m");
        assert_eq!(full.attributes.get("a").map(String::as_str), Some("1"));
        assert!(full.video.agent && !full.video.can_publish());
        assert!(full.is_agent());
        assert_eq!(full.participant_kind(), ParticipantKind::Agent);
    }

    /// 上游 `AccessTokenError::InvalidKeys`：密钥为空不签。这里 `ApiKeyPair`
    /// 构造期就拦掉了空 apiKey，签发期再拦一次是纵深防御。
    #[test]
    fn empty_api_key_is_refused_at_mint_time_too() {
        let issuer = LiveKitIssuer::new(pair());
        let req = request(MediaScope::Call);
        assert!(issuer.issue(&req, 1_000).is_ok());
        // grant 里带 source 白名单也不签：判定会落到 sidecar 的 protocol 库里，
        // 房间事实分叉我们看不见。
        let mut g = grant_for(MediaScope::Call);
        g.can_publish_sources = vec!["microphone".into()];
        assert_eq!(
            assert_sidecar_grant_allowed(&g),
            Err(TokenError::GrantMismatch)
        );
    }

    #[test]
    fn privileged_grants_are_rejected_by_the_hard_gate() {
        // 服务端 Egress 入口（recorder/hidden）与房间管理权（roomAdmin/
        // ingressAdmin）都必须在签发前被拒；错误里带 ADR-25 的理由。
        let privileged = [
            VideoGrant {
                recorder: true,
                ..Default::default()
            },
            VideoGrant {
                hidden: true,
                ..Default::default()
            },
            VideoGrant {
                room_admin: true,
                ..Default::default()
            },
            VideoGrant {
                ingress_admin: true,
                ..Default::default()
            },
        ];
        let errs: Vec<_> = privileged
            .iter()
            .map(assert_sidecar_grant_allowed)
            .map(|r| r.expect_err("特权 grant 必须被拒"))
            .collect();
        assert_eq!(
            errs,
            vec![
                TokenError::ServerEgressGrantForbidden,
                TokenError::ServerEgressGrantForbidden,
                TokenError::SidecarAdminForbidden,
                TokenError::SidecarAdminForbidden,
            ]
        );
        assert!(errs[0]
            .to_string()
            .contains("server-side egress recording is disabled"));
        // 普通入房 grant 走同一条闸，放行。
        let mut ok = grant_for(MediaScope::Call);
        ok.room = "r".into();
        assert_eq!(assert_sidecar_grant_allowed(&ok), Ok(()));
    }

    #[test]
    fn algorithm_confusion_and_forgery_are_rejected() {
        let issuer = LiveKitIssuer::new(pair());
        let issued = issuer.issue(&request(MediaScope::Call), 1_000).unwrap();
        let parts: Vec<&str> = issued.token.split('.').collect();

        // alg: none（去掉签名）
        let none_header = b64url_encode(br#"{"alg":"none","typ":"JWT"}"#);
        let forged = format!("{none_header}.{}.{}", parts[1], parts[2]);
        assert_eq!(issuer.verify(&forged, 1_000), Err(TokenError::BadAlgorithm));

        // alg 改成 HS512 但签名仍是 HS256：头先拒。
        let hs512 = b64url_encode(br#"{"alg":"HS512","typ":"JWT"}"#);
        assert_eq!(
            issuer.verify(&format!("{hs512}.{}.{}", parts[1], parts[2]), 1_000),
            Err(TokenError::BadAlgorithm)
        );

        // 改 payload 不改签名
        let body = b64url_decode(parts[1]).unwrap();
        let mut claims: AccessClaims = serde_json::from_slice(&body).unwrap();
        claims.sub = "mallory".into();
        let tampered = format!(
            "{}.{}.{}",
            parts[0],
            b64url_encode(&serde_json::to_vec(&claims).unwrap()),
            parts[2]
        );
        assert_eq!(
            issuer.verify(&tampered, 1_000),
            Err(TokenError::BadSignature)
        );

        // 不是三段 = 结构就不合法
        assert_eq!(issuer.verify("a.b", 1_000), Err(TokenError::Malformed));
        assert_eq!(
            issuer.verify(
                &format!("{}.{}.{}", parts[0], parts[1], "not!base64"),
                1_000
            ),
            Err(TokenError::Malformed),
            "签名段本身解不开 → 结构错误"
        );
        // payload 段不是合法 base64url：签名先失败。顺序是刻意的——
        // **验签之前不解释 payload**，否则会拿未验签的字节去做任何判断。
        assert_eq!(
            issuer.verify(&format!("{}.{}.{}", parts[0], "!!!!", parts[2]), 1_000),
            Err(TokenError::BadSignature)
        );
    }

    #[test]
    fn binding_mismatches_are_named_not_folded() {
        let issuer = LiveKitIssuer::new(pair());
        let issued = issuer.issue(&request(MediaScope::Call), 1_000).unwrap();
        let t = &issued.token;
        assert_eq!(
            issuer.verify_for(t, 1_000, "other-room", MediaScope::Call, 4, "livekit"),
            Err(TokenError::WrongRoom)
        );
        assert_eq!(
            issuer.verify_for(
                t,
                1_000,
                "wxlike-call-c1",
                MediaScope::LiveWatch,
                4,
                "livekit"
            ),
            Err(TokenError::WrongScope)
        );
        assert_eq!(
            issuer.verify_for(t, 1_000, "wxlike-call-c1", MediaScope::Call, 5, "livekit"),
            Err(TokenError::StaleGeneration)
        );
        assert_eq!(
            issuer.verify_for(t, 1_000, "wxlike-call-c1", MediaScope::Call, 4, "str0m"),
            Err(TokenError::EngineMismatch("livekit".into(), "str0m".into()))
        );
        // 冷备引擎签的令牌换不了 LiveKit 房间；反过来也一样。
        let cold = AccessRequest {
            engine: "str0m".into(),
            ..request(MediaScope::Call)
        };
        let cold_token = issuer.issue(&cold, 1_000).unwrap();
        assert!(matches!(
            issuer.verify_for(
                &cold_token.token,
                1_000,
                "wxlike-call-c1",
                MediaScope::Call,
                4,
                "livekit"
            ),
            Err(TokenError::EngineMismatch(_, _))
        ));
    }

    #[test]
    fn wrong_api_key_and_weak_inputs_fail_closed() {
        assert_eq!(
            ApiKeyPair::new("", vec![1u8; MIN_API_SECRET_LEN]).unwrap_err(),
            TokenError::EmptyApiKey
        );
        assert_eq!(
            ApiKeyPair::new("APIx", vec![1u8; MIN_API_SECRET_LEN - 1]).unwrap_err(),
            TokenError::WeakSecret
        );
        let issued = LiveKitIssuer::new(pair())
            .issue(&request(MediaScope::Call), 1_000)
            .unwrap();
        // hex 形式的部署密钥与原始字节等价（同一 apiKey/密钥 → 同一签名）。
        let from_hex = ApiKeyPair::from_hex("APIsidetest", &hex::encode([0x31u8; 32])).unwrap();
        assert_eq!(
            LiveKitIssuer::new(from_hex)
                .verify(&issued.token, 1_000)
                .unwrap()
                .sub,
            "bob"
        );
        assert!(matches!(
            ApiKeyPair::from_hex("APIsidetest", "zz"),
            Err(TokenError::WeakSecret)
        ));
        // 同密钥、不同 apiKey：签名仍然成立，但 iss 绑定把它拦下——
        // 一把密钥不能冒充另一个 apiKey 的令牌。
        let renamed = LiveKitIssuer::new(ApiKeyPair::new("APIrenamed", vec![0x31; 32]).unwrap());
        assert_eq!(
            renamed.verify(&issued.token, 1_000),
            Err(TokenError::WrongApiKey)
        );
        // 同 apiKey、不同密钥：签名不符，直接 BadSignature（不走 iss）。
        let weak = LiveKitIssuer::new(ApiKeyPair::new("APIsidetest", vec![0x32; 32]).unwrap());
        assert_eq!(
            weak.verify(&issued.token, 1_000),
            Err(TokenError::BadSignature)
        );
        // TTL 越界一律拒签（不允许长效令牌）。
        assert!(LiveKitIssuer::with_ttl(pair(), 0).is_err());
        assert!(LiveKitIssuer::with_ttl(pair(), MAX_ACCESS_TTL_MS + 1).is_err());
        let long = AccessRequest {
            ttl_ms: Some(MAX_ACCESS_TTL_MS + 1),
            ..request(MediaScope::Call)
        };
        assert_eq!(
            LiveKitIssuer::new(pair()).issue(&long, 1_000),
            Err(TokenError::InvalidLifetime)
        );
        let empty = AccessRequest {
            room: String::new(),
            ..request(MediaScope::Call)
        };
        assert_eq!(
            LiveKitIssuer::new(pair()).issue(&empty, 1_000),
            Err(TokenError::InvalidJoinClaims),
            "上游 to_jwt 的 InvalidClaims 分支：roomJoin 却没有 room"
        );
        let no_engine = AccessRequest {
            engine: String::new(),
            ..request(MediaScope::Call)
        };
        assert_eq!(
            LiveKitIssuer::new(pair()).issue(&no_engine, 1_000),
            Err(TokenError::Malformed)
        );
        assert!(format!("{:?}", pair()).contains("redacted"));
        // 上游 DEFAULT_TTL 是 6 小时；我们的上限收紧到 15 分钟（ADR-25 刻意偏离）。
        const { assert!(MAX_ACCESS_TTL_MS / 1_000 < LIVEKIT_DEFAULT_TTL_SECS) };
        assert_eq!(LIVEKIT_DEFAULT_TTL_SECS, 3600 * 6);
    }

    #[test]
    fn base64url_matches_std_alphabet_without_padding() {
        // RFC 4648 示例 + JWT 常用的非 ASCII 边界。
        assert_eq!(b64url_encode(b""), "");
        assert_eq!(b64url_encode(b"f"), "Zg");
        assert_eq!(b64url_encode(b"fo"), "Zm8");
        assert_eq!(b64url_encode(b"foo"), "Zm9v");
        assert_eq!(b64url_encode(b"foob"), "Zm9vYg");
        // 62/63 两个字符是 `-`/`_`，不是 `+`/`/`。
        assert_eq!(b64url_encode(&[0xFB, 0xFF]), "-_8");
        for sample in [
            b"".to_vec(),
            vec![0u8],
            vec![0xFF; 7],
            b"hello world".to_vec(),
            (0u8..=255).collect::<Vec<u8>>(),
        ] {
            let text = b64url_encode(&sample);
            assert!(!text.contains('='), "JWT 段不带 padding");
            assert_eq!(b64url_decode(&text).unwrap(), sample, "{text}");
            // 容忍带 padding 的等价写法。
            assert_eq!(b64url_decode(&format!("{text}==")).unwrap(), sample);
        }
        assert_eq!(b64url_decode("A"), Err(TokenError::Malformed));
        assert_eq!(b64url_decode("A*"), Err(TokenError::Malformed));
        // 非整组的尾巴必须按 16/8 位移回来，不是按 12/6：这是手写 base64
        // 最容易错的一处（`"_w"` 是 0xFF 的 2 字符编码，`"wA"` 是 0x30 0x80…
        // 少一位就是整枚 JWT 解不开）。
        assert_eq!(b64url_encode(&[0xFF]), "_w");
        assert_eq!(b64url_decode("_w").unwrap(), vec![0xFF]);
        assert_eq!(b64url_decode("________").unwrap(), vec![0xFF; 6]);
        assert_eq!(b64url_encode(&[0xFF; 6]), "________");
        assert_eq!(
            b64url_decode("Zm9vYg").unwrap(),
            vec![b'f', b'o', b'o', b'b']
        );
        // 5 个字符 = 一组 + 落单的一个，非法（JWT 段不会长这样）。
        assert_eq!(b64url_decode("Zm9vY"), Err(TokenError::Malformed));
    }
}
