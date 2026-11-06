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

/// LiveKit 只接受 HS256；其它 alg（含 `none`）一律拒绝，防算法混淆。
pub const LIVEKIT_ALG: &str = "HS256";
/// sidecar 共享密钥最短长度（与 LiveKit 自身的 apiKeySecret 校验一致）。
pub const MIN_API_SECRET_LEN: usize = 32;
/// 访问令牌默认寿命：60 秒，够完成一次入房，不够攒着一路用。
pub const DEFAULT_ACCESS_TTL_MS: u64 = 60_000;
/// 寿命上限：过期后必须回信令面重新入房（再过一次成员状态机 + 审计）。
pub const MAX_ACCESS_TTL_MS: u64 = 15 * 60_000;

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

/// `video` grant：sidecar 看得懂的那部分能力声明。字段名用 LiveKit 的
/// camelCase 线格式（`roomJoin` / `canPublish` / `canPublishData` …）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoGrant {
    /// 只能进这一个房间；LiveKit 自己在入房时校验。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub room_join: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_publish: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_subscribe: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub can_publish_data: bool,
    /// 服务端 Egress 录制器的隐藏身份 —— ADR-25 禁止我们签发。
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    /// 同上：`recorder` 令牌是 LiveKit 侧自动录制的入口。
    #[serde(default, skip_serializing_if = "is_false")]
    pub recorder: bool,
    /// 房间管理权（改他人状态）。必须只由 Rust 信令面行使。
    #[serde(default, skip_serializing_if = "is_false")]
    pub room_admin: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub ingress_admin: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
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

/// 解码后的 LiveKit 访问令牌声明。时间字段是 **Unix 秒**（JWT 规范，
/// 与 LiveKit 一致），不要和内部媒体令牌的毫秒混用。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessClaims {
    pub iss: String,
    #[serde(default)]
    pub sub: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jti: Option<String>,
    #[serde(default)]
    pub iat: u64,
    #[serde(default)]
    pub nbf: u64,
    pub exp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default)]
    pub video: VideoGrant,
    #[serde(default)]
    pub wxlike: WxlikeClaims,
}

/// 一次入房凭证的请求。`identity` 是 LiveKit 侧的用户身份（= user id），
/// 设备靠 `device` 区分：同一用户的两台设备是两个 LiveKit participant。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessRequest {
    pub room: String,
    pub identity: String,
    pub device: String,
    pub call: String,
    pub scope: MediaScope,
    pub generation: u64,
    pub engine: String,
    /// 媒体密钥指纹（`MediaKeyRing::fingerprint`），sidecar 只拿摘要校验，
    /// 拿不到密钥本身。
    pub key_fingerprint: Option<String>,
    /// 自定义寿命；`None` 用 [`DEFAULT_ACCESS_TTL_MS`]。
    pub ttl_ms: Option<u64>,
    /// 服务端静音（合同 §3.7）：为真时上行能力被降级为无——客户端继续发包
    /// 也不会被 sidecar 转发。只能**降**不能**升**（见 [`Self::verify_for`]）。
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
/// `canPublishData` 一律不开：通话/直播的信令只走 wxlike 自己的 QUIC 面，
/// 给客户端留 LiveKit 数据通道等于再开一条没审计的旁路。
pub fn grant_for(scope: MediaScope) -> VideoGrant {
    let mut grant = VideoGrant {
        room_join: true,
        can_subscribe: true,
        ..Default::default()
    };
    match scope {
        // 通话：双向，语音+视频轨由房间状态裁决。
        MediaScope::Call => grant.can_publish = true,
        // 推流：只有主播。
        MediaScope::LivePublish => {
            grant.can_publish = true;
            grant.can_subscribe = false;
        }
        // 观众：只下行，麦克风与数据通道永远关。
        MediaScope::LiveWatch => {}
        // 连麦：主播审批后才签发，可上行。
        MediaScope::LiveCohost => grant.can_publish = true,
    }
    grant
}

/// ADR-25 硬闸：sidecar 令牌不得带服务端录制能力，也不得带房间管理权。
///
/// 返回 `Err` 时调用方**不能**再签这枚令牌——LiveKit 的 `recorder`/`hidden`
/// 参与者是 Egress 自动录制的唯一入口，`roomAdmin` 则会让 sidecar 里长出
/// 第二套房间事实（静音/踢人/录制状态对不齐）。
pub fn assert_sidecar_grant_allowed(grant: &VideoGrant) -> Result<(), TokenError> {
    if grant.recorder || grant.hidden {
        return Err(TokenError::ServerEgressGrantForbidden);
    }
    if grant.room_admin || grant.ingress_admin {
        return Err(TokenError::SidecarAdminForbidden);
    }
    Ok(())
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
        if req.room.is_empty() || req.identity.is_empty() || req.engine.is_empty() {
            return Err(TokenError::Malformed);
        }
        let ttl_ms = match req.ttl_ms {
            Some(0) => return Err(TokenError::InvalidLifetime),
            Some(v) => v,
            None => self.ttl_ms,
        };
        if ttl_ms > MAX_ACCESS_TTL_MS {
            return Err(TokenError::InvalidLifetime);
        }
        let mut grant = grant_for(req.scope);
        grant.room = Some(req.room.clone());
        if req.muted {
            // 服务端静音落到能力面：客户端继续发也没人转发。
            grant.can_publish = false;
        }
        // 结构性保证：即使 grant_for 以后被改错，这里也不会放过 admin/recorder。
        assert_sidecar_grant_allowed(&grant)?;

        let issued_at_secs = now_ms / 1_000;
        let expires_at_secs = issued_at_secs.saturating_add(ttl_ms / 1_000);
        let claims = AccessClaims {
            iss: self.pair.api_key().to_string(),
            sub: req.identity.clone(),
            jti: Some(random_jti()),
            iat: issued_at_secs,
            nbf: issued_at_secs,
            exp: expires_at_secs,
            sha256: req.key_fingerprint.clone(),
            video: grant,
            wxlike: WxlikeClaims {
                call: req.call.clone(),
                device: req.device.clone(),
                scope: req.scope,
                generation: req.generation,
                engine: req.engine.clone(),
            },
        };
        Ok(IssuedAccess {
            token: self.encode(&claims)?,
            room: req.room.clone(),
            identity: req.identity.clone(),
            device: req.device.clone(),
            expires_at_ms: expires_at_secs.saturating_mul(1_000),
        })
    }

    fn encode(&self, claims: &AccessClaims) -> Result<String, TokenError> {
        let header = r#"{"alg":"HS256","typ":"JWT"}"#;
        let body = serde_json::to_vec(claims).map_err(|_| TokenError::Malformed)?;
        let signing_input = format!("{header}.{}", b64url_encode(&body));
        let sig = self.pair.sign(signing_input.as_bytes())?;
        Ok(format!("{signing_input}.{}", b64url_encode(&sig)))
    }

    /// 验签 + 验时效 + 验 apiKey。**不做**房间/代次判断——那是 [`Self::verify_for`]。
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
        if claims.video.room.as_deref() != Some(room) {
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
        // grant 只能等于或**低于** scope 的能力面（静音是降级），任何升级都拒。
        // 数据通道一律不开：开了就等于给客户端留一条不经审计的旁路。
        if claims.video.can_publish && !scope.may_publish() {
            return Err(TokenError::GrantMismatch);
        }
        if claims.video.can_publish_data {
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

pub(crate) fn b64url_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(URL_ALPHABET[(n >> 18) as usize & 0x3F] as char);
        out.push(URL_ALPHABET[(n >> 12) as usize & 0x3F] as char);
        if chunk.len() > 1 {
            out.push(URL_ALPHABET[(n >> 6) as usize & 0x3F] as char);
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
        b'a'..=b'z' => Ok(u32::from(c - b'a') + 26),
        b'0'..=b'9' => Ok(u32::from(c - b'0') + 52),
        b'-' => Ok(62),
        b'_' => Ok(63),
        _ => Err(TokenError::Malformed),
    }
}

/// 解码 base64url；容忍 padding（有些 SDK 会带上 `=`）。
pub(crate) fn b64url_decode(text: &str) -> Result<Vec<u8>, TokenError> {
    let raw = text.trim_end_matches('=').as_bytes();
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if raw.len() % 4 == 1 {
        return Err(TokenError::Malformed);
    }
    let mut out = Vec::with_capacity(raw.len() / 4 * 3 + 3);
    for chunk in raw.chunks(4) {
        let mut acc: u32 = 0;
        for (i, byte) in chunk.iter().enumerate() {
            let v = url_index(*byte)?;
            acc |= v << (18 - 6 * i);
        }
        match chunk.len() {
            2 => out.push((acc >> 12) as u8),
            3 => {
                out.push((acc >> 12) as u8);
                out.push((acc >> 6) as u8);
            }
            4 => {
                out.push((acc >> 18) as u8);
                out.push((acc >> 12) as u8);
                out.push((acc >> 6) as u8);
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
            key_fingerprint: None,
            ttl_ms: None,
            muted: false,
        }
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
        assert_eq!(claims.video.room.as_deref(), Some("wxlike-call-c1"));
        assert!(claims.video.room_join && claims.video.can_publish);
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
                    g.room = Some("wxlike-call-c1".into());
                    g
                },
                "{scope:?} grant 必须由映射表推导"
            );
            assert!(!claims.video.recorder, "{scope:?} 不得带服务端录制身份");
            assert!(!claims.video.hidden, "{scope:?} 不得是隐藏参与者");
            assert!(!claims.video.room_admin, "{scope:?} 不得带房间管理权");
            assert!(!claims.video.ingress_admin);
            assert_eq!(claims.video.can_publish, scope.may_publish());
        }
        // 观众永远不能推流。
        assert!(!grant_for(MediaScope::LiveWatch).can_publish);
        assert!(!grant_for(MediaScope::LiveWatch).can_publish_data);
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
        assert!(!claims.video.can_publish, "静音成员拿到的令牌不能再推流");
        assert!(claims.video.can_subscribe, "静音不影响下行");
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
        assert!(!claims.video.can_publish);
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
        claims.video.can_publish = true;
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
        with_data.room = Some("wxlike-call-c1".into());
        with_data.can_publish_data = true;
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
        ok.room = Some("r".into());
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

        // 不是三段 / 非法 base64
        assert_eq!(issuer.verify("a.b", 1_000), Err(TokenError::Malformed));
        assert_eq!(
            issuer.verify(&format!("{}.{}.{}", parts[0], "!!!!", parts[2]), 1_000),
            Err(TokenError::Malformed)
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
            Err(TokenError::Malformed)
        );
        assert!(format!("{:?}", pair()).contains("redacted"));
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
    }
}
