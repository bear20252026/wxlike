use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use image::{Rgb, RgbaImage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::clock::Clock;
use crate::crypto::Signer;
use crate::error::{DiscoveryError, Result};
use crate::id::{new_id, random_hex};
use crate::qr::render::{render_png_with_logo, DEFAULT_QR_SIZE};

const CARD_SCHEME: &str = "wxlike://card/1";
const CARD_MAX_BYTES: usize = 1024;
const MIN_TTL_MS: i64 = 60 * 1000;
const MAX_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const CAPABILITY_BYTES: usize = 16;
const MIN_CONTRAST: f64 = 4.5;
const CHANNEL_MAX: f64 = 255.0;
const SRGB_THRESHOLD: f64 = 0.039_28;
const SRGB_DIVISOR: f64 = 12.92;
const GAMMA_OFFSET: f64 = 0.055;
const GAMMA_SCALE: f64 = 1.055;
const GAMMA_EXPONENT: f64 = 2.4;
const LUMA_RED: f64 = 0.2126;
const LUMA_GREEN: f64 = 0.7152;
const LUMA_BLUE: f64 = 0.0722;
const CONTRAST_OFFSET: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardStyle {
    SpringGreen,
    InkGreen,
    WarmSun,
    MistBlue,
}

impl CardStyle {
    pub const ALL: [Self; 4] = [
        Self::SpringGreen,
        Self::InkGreen,
        Self::WarmSun,
        Self::MistBlue,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::SpringGreen => "春日绿",
            Self::InkGreen => "墨绿",
            Self::WarmSun => "暖阳",
            Self::MistBlue => "雾蓝",
        }
    }

    pub fn colors(self) -> (Rgb<u8>, Rgb<u8>) {
        const SPRING_GREEN: (Rgb<u8>, Rgb<u8>) = (Rgb([27, 107, 69]), Rgb([243, 251, 246]));
        const INK_GREEN: (Rgb<u8>, Rgb<u8>) = (Rgb([20, 69, 47]), Rgb([234, 245, 238]));
        const WARM_SUN: (Rgb<u8>, Rgb<u8>) = (Rgb([138, 75, 18]), Rgb([255, 248, 236]));
        const MIST_BLUE: (Rgb<u8>, Rgb<u8>) = (Rgb([39, 75, 109]), Rgb([242, 247, 252]));
        match self {
            Self::SpringGreen => SPRING_GREEN,
            Self::InkGreen => INK_GREEN,
            Self::WarmSun => WARM_SUN,
            Self::MistBlue => MIST_BLUE,
        }
    }
}

pub fn contrast_ratio(dark: Rgb<u8>, light: Rgb<u8>) -> f64 {
    let l1 = relative_luminance(dark);
    let l2 = relative_luminance(light);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + CONTRAST_OFFSET) / (lo + CONTRAST_OFFSET)
}

fn relative_luminance(color: Rgb<u8>) -> f64 {
    let channel = |value: u8| {
        let v = f64::from(value) / CHANNEL_MAX;
        if v <= SRGB_THRESHOLD {
            v / SRGB_DIVISOR
        } else {
            ((v + GAMMA_OFFSET) / GAMMA_SCALE).powf(GAMMA_EXPONENT)
        }
    };
    LUMA_RED * channel(color[0]) + LUMA_GREEN * channel(color[1]) + LUMA_BLUE * channel(color[2])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactCard {
    pub card_id: String,
    pub wire: String,
    pub expires_at_ms: i64,
    record_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContactProfileRef {
    pub user_id: String,
    pub card_id: String,
}

#[async_trait]
pub trait FriendRequestPort: Send + Sync {
    /// G02 负责权限、限流与好友申请状态；本域只把"扫码后明确发起的申请"交给它。
    async fn request_friend(
        &self,
        actor: &str,
        target_user_id: &str,
        card_id: &str,
    ) -> Result<String>;
}

#[async_trait]
pub trait MediaPort: Send + Sync {
    async fn save_image(&self, bytes: &[u8], mime: &str) -> Result<String>;
    async fn share_image(&self, bytes: &[u8], mime: &str) -> Result<()>;
}

#[derive(Serialize, Deserialize)]
struct CardEnvelope {
    c: String,
    k: String,
    e: i64,
    n: String,
}

struct CardRecord {
    user_id: String,
    expires_at_ms: i64,
    revoked: bool,
}

pub struct ContactQrService<C: Clock> {
    clock: C,
    signer: Signer,
    cards: Mutex<HashMap<String, CardRecord>>,
}

impl<C: Clock> ContactQrService<C> {
    pub fn new(clock: C, signing_key: impl AsRef<[u8]>) -> Self {
        Self {
            clock,
            signer: Signer::new(signing_key),
            cards: Mutex::new(HashMap::new()),
        }
    }

    pub fn create(&self, actor: &str, ttl_ms: i64) -> Result<ContactCard> {
        if actor.is_empty() {
            return Err(DiscoveryError::Unauthorized);
        }
        let ttl_ms = ttl_ms.clamp(MIN_TTL_MS, MAX_TTL_MS);
        let now_ms = self.clock.now_ms();
        let card_id = new_id("card");
        let capability = random_hex(CAPABILITY_BYTES);
        let record = CardRecord {
            user_id: actor.to_owned(),
            expires_at_ms: now_ms + ttl_ms,
            revoked: false,
        };
        let record_key = hash_capability(&capability);
        self.cards
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .insert(record_key.clone(), record);
        let wire = self.encode(&card_id, &capability, now_ms + ttl_ms)?;
        Ok(ContactCard {
            card_id,
            wire,
            expires_at_ms: now_ms + ttl_ms,
            record_key,
        })
    }

    fn encode(&self, card_id: &str, capability: &str, expires_at_ms: i64) -> Result<String> {
        let nonce = new_id("n");
        let envelope = CardEnvelope {
            c: card_id.to_owned(),
            k: capability.to_owned(),
            e: expires_at_ms,
            n: nonce,
        };
        let json = serde_json::to_vec(&envelope)
            .map_err(|_| DiscoveryError::InvalidArgument("card encode"))?;
        let payload = URL_SAFE_NO_PAD.encode(json);
        let canonical = format!("wxlike:card:1:{payload}");
        let signature = self.signer.sign(&canonical);
        let wire = format!("{CARD_SCHEME}?p={payload}&sig={signature}");
        if wire.len() > CARD_MAX_BYTES {
            return Err(DiscoveryError::InvalidArgument("card payload size"));
        }
        Ok(wire)
    }

    pub fn render(
        &self,
        card: &ContactCard,
        style: CardStyle,
        logo: Option<&RgbaImage>,
    ) -> Result<Vec<u8>> {
        self.active_record(&card.record_key)?;
        let (dark, light) = style.colors();
        if contrast_ratio(dark, light) < MIN_CONTRAST {
            return Err(DiscoveryError::InvalidArgument("qr contrast"));
        }
        render_png_with_logo(&card.wire, dark, light, DEFAULT_QR_SIZE, logo)
    }

    pub fn resolve(&self, scanned: &str) -> Result<ContactProfileRef> {
        let envelope = self.decode_wire(scanned)?;
        let now_ms = self.clock.now_ms();
        if envelope.e <= now_ms {
            return Err(DiscoveryError::Expired);
        }
        let cards = self
            .cards
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let record = cards
            .get(&hash_capability(&envelope.k))
            .ok_or(DiscoveryError::NotFound)?;
        if record.revoked || record.expires_at_ms <= now_ms || record.user_id.is_empty() {
            return Err(DiscoveryError::Expired);
        }
        Ok(ContactProfileRef {
            user_id: record.user_id.clone(),
            card_id: envelope.c,
        })
    }

    /// 轮换后旧码立即失效；返回新卡。
    pub fn rotate(&self, actor: &str, ttl_ms: i64) -> Result<ContactCard> {
        self.revoke_all(actor)?;
        self.create(actor, ttl_ms)
    }

    pub fn revoke_all(&self, actor: &str) -> Result<()> {
        let mut cards = self
            .cards
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let mut found = false;
        for record in cards.values_mut() {
            if record.user_id == actor && !record.revoked {
                record.revoked = true;
                found = true;
            }
        }
        if found {
            Ok(())
        } else {
            Err(DiscoveryError::NotFound)
        }
    }

    pub async fn request_friend<P: FriendRequestPort>(
        &self,
        actor: &str,
        port: &P,
        profile: &ContactProfileRef,
    ) -> Result<String> {
        if actor == profile.user_id {
            return Err(DiscoveryError::InvalidArgument("self friend request"));
        }
        port.request_friend(actor, &profile.user_id, &profile.card_id)
            .await
    }

    pub async fn save_to_album<P: MediaPort>(&self, port: &P, png: &[u8]) -> Result<String> {
        port.save_image(png, "image/png").await
    }

    pub async fn share<P: MediaPort>(&self, port: &P, png: &[u8]) -> Result<()> {
        port.share_image(png, "image/png").await
    }

    fn active_record(&self, record_key: &str) -> Result<()> {
        let now_ms = self.clock.now_ms();
        let cards = self
            .cards
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        match cards.get(record_key) {
            Some(record) if !record.revoked && record.expires_at_ms > now_ms => Ok(()),
            _ => Err(DiscoveryError::NotFound),
        }
    }

    fn decode_wire(&self, scanned: &str) -> Result<CardEnvelope> {
        if scanned.len() > CARD_MAX_BYTES {
            return Err(DiscoveryError::InvalidArgument("card payload size"));
        }
        let rest = scanned
            .strip_prefix(&format!("{CARD_SCHEME}?"))
            .ok_or(DiscoveryError::InvalidArgument("card scheme"))?;
        let mut payload = None;
        let mut signature = None;
        for pair in rest.split('&') {
            let (key, value) = pair
                .split_once('=')
                .ok_or(DiscoveryError::InvalidArgument("card query"))?;
            match key {
                "p" if payload.is_none() => payload = Some(value),
                "sig" if signature.is_none() => signature = Some(value),
                _ => return Err(DiscoveryError::InvalidArgument("card query")),
            }
        }
        let (payload, signature) = match (payload, signature) {
            (Some(p), Some(s)) => (p, s),
            _ => return Err(DiscoveryError::InvalidArgument("card query")),
        };
        let canonical = format!("wxlike:card:1:{payload}");
        if !self.signer.verify(&canonical, signature) {
            return Err(DiscoveryError::InvalidArgument("card signature"));
        }
        let json = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| DiscoveryError::InvalidArgument("card encoding"))?;
        let envelope: CardEnvelope = serde_json::from_slice(&json)
            .map_err(|_| DiscoveryError::InvalidArgument("card schema"))?;
        Ok(envelope)
    }
}

fn hash_capability(capability: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(capability.as_bytes());
    hex::encode(hasher.finalize())
}

/// 用于测试和平台层检查：能力值熵足够，不承载用户资料。
pub fn capability_entropy_bytes() -> usize {
    CAPABILITY_BYTES
}
