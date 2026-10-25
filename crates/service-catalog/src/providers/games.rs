use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::prelude::*;

pub const LAUNCH_TTL_MS: i64 = 10 * 60 * 1000;
pub const OFFER_TTL_MS: i64 = 5 * 60 * 1000;
pub const MAX_SAVE_BYTES: usize = 1_000_000;
pub const MAX_SAVE_SLOTS: u8 = 8;
pub const MAX_URL_LEN: usize = 2_048;
pub const SHA256_HEX_LEN: usize = 64;
const MAX_ID_LEN: usize = 64;
const MAX_TITLE_LEN: usize = 64;
const MAX_VERSION_LEN: usize = 32;
const MAX_ACTOR_LEN: usize = 128;
const MAX_AUTHORITY_LEN: usize = 253;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GamePackage {
    pub package_id: String,
    pub title: String,
    pub version: String,
    pub price_minor: i64,
    pub currency: String,
    pub regions: Vec<String>,
    pub program_sha256: String,
    pub signature: String,
    pub publisher_key_id: String,
}

impl GamePackage {
    pub fn canonical(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}",
            self.package_id,
            self.title,
            self.version,
            self.price_minor,
            self.currency,
            self.program_sha256,
            self.regions.join(",")
        )
    }
}

/// G08 提供的签名验签端口；没有真实验签实现时不得发布任何包。
pub trait GameSignatureVerifier: Send + Sync {
    fn verify(&self, package: &GamePackage) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchTicket {
    pub ticket_id: String,
    pub package_id: String,
    pub actor: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub save_slots: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalLink {
    pub url: String,
    pub domain: String,
    pub data_scopes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entitlement {
    actor: String,
    package_id: String,
    active: bool,
    price_minor: i64,
}

pub struct GameService {
    verifier: Arc<dyn GameSignatureVerifier>,
    clock: Arc<dyn Clock>,
    evidence: Option<SandboxEvidence>,
    packages: Mutex<HashMap<String, GamePackage>>,
    entitlements: Mutex<HashMap<String, Entitlement>>,
    saves: Mutex<HashMap<String, Vec<u8>>>,
    tickets: Mutex<HashMap<String, LaunchTicket>>,
}

impl GameService {
    pub fn new(
        verifier: Arc<dyn GameSignatureVerifier>,
        clock: Arc<dyn Clock>,
        evidence: Option<SandboxEvidence>,
    ) -> Self {
        Self {
            verifier,
            clock,
            evidence,
            packages: Mutex::new(HashMap::new()),
            entitlements: Mutex::new(HashMap::new()),
            saves: Mutex::new(HashMap::new()),
            tickets: Mutex::new(HashMap::new()),
        }
    }

    pub fn publish(&self, package: GamePackage) -> Result<()> {
        validate_package(&package)?;
        if !self.verifier.verify(&package) {
            return Err(ServiceError::new(
                Code::Unauthorized,
                "game signature rejected",
            ));
        }
        self.packages
            .lock()
            .map_err(|_| internal())?
            .insert(package.package_id.clone(), package);
        Ok(())
    }

    pub fn list(&self, region: &str) -> Vec<GamePackage> {
        let Ok(packages) = self.packages.lock() else {
            return Vec::new();
        };
        let mut visible: Vec<GamePackage> = packages
            .values()
            .filter(|package| package.regions.iter().any(|area| area == region))
            .cloned()
            .collect();
        visible.sort_by(|a, b| a.package_id.cmp(&b.package_id));
        visible
    }

    pub fn launch(&self, actor: &str, package_id: &str, region: &str) -> Result<LaunchTicket> {
        validate_actor(actor)?;
        let package = self.package(package_id)?;
        if !package.regions.iter().any(|area| area == region) {
            return Err(ServiceError::new(
                Code::BlockedExternal,
                "region unavailable",
            ));
        }
        let now = self.clock.now_ms();
        let ticket = LaunchTicket {
            ticket_id: new_id("game"),
            package_id: package_id.to_owned(),
            actor: actor.to_owned(),
            issued_at_ms: now,
            expires_at_ms: now + LAUNCH_TTL_MS,
            save_slots: MAX_SAVE_SLOTS,
        };
        self.tickets
            .lock()
            .map_err(|_| internal())?
            .insert(ticket.ticket_id.clone(), ticket.clone());
        Ok(ticket)
    }

    /// 存档按 (用户, 游戏, 槽位) 隔离；不提供跨用户读取入口。
    pub fn save(&self, actor: &str, package_id: &str, slot: u8, payload: Vec<u8>) -> Result<()> {
        validate_actor(actor)?;
        self.package(package_id)?;
        validate_slot(slot)?;
        if payload.is_empty() || payload.len() > MAX_SAVE_BYTES {
            return Err(ServiceError::new(Code::InvalidArgument, "save size"));
        }
        self.saves
            .lock()
            .map_err(|_| internal())?
            .insert(save_key(actor, package_id, slot), payload);
        Ok(())
    }

    pub fn load(&self, actor: &str, package_id: &str, slot: u8) -> Result<Vec<u8>> {
        validate_actor(actor)?;
        self.package(package_id)?;
        validate_slot(slot)?;
        self.saves
            .lock()
            .map_err(|_| internal())?
            .get(&save_key(actor, package_id, slot))
            .cloned()
            .ok_or_else(|| ServiceError::new(Code::NotFound, "save not found"))
    }

    pub fn delete_save(&self, actor: &str, package_id: &str, slot: u8) -> bool {
        self.saves
            .lock()
            .map(|mut saves| saves.remove(&save_key(actor, package_id, slot)).is_some())
            .unwrap_or(false)
    }

    /// 第三方跳转只展示目的域与授权项，并剥掉 token 类查询参数。
    pub fn external_link(&self, url: &str, scopes: Vec<String>) -> Result<ExternalLink> {
        let (url, domain) = sanitize_url(url)?;
        Ok(ExternalLink {
            url,
            domain,
            data_scopes: scopes,
        })
    }

    fn package(&self, package_id: &str) -> Result<GamePackage> {
        self.packages
            .lock()
            .map_err(|_| internal())?
            .get(package_id)
            .cloned()
            .ok_or_else(|| ServiceError::new(Code::NotFound, "game not found"))
    }
}

fn validate_package(package: &GamePackage) -> Result<()> {
    if package.package_id.is_empty() || package.package_id.len() > MAX_ID_LEN {
        return Err(ServiceError::new(Code::InvalidArgument, "package id"));
    }
    if package.title.is_empty() || package.title.len() > MAX_TITLE_LEN {
        return Err(ServiceError::new(Code::InvalidArgument, "package title"));
    }
    if package.version.is_empty() || package.version.len() > MAX_VERSION_LEN {
        return Err(ServiceError::new(Code::InvalidArgument, "package version"));
    }
    if package.price_minor < 0 || package.currency.len() != 3 {
        return Err(ServiceError::new(Code::InvalidArgument, "package price"));
    }
    if package.regions.is_empty() {
        return Err(ServiceError::new(Code::InvalidArgument, "package regions"));
    }
    if package.program_sha256.len() != SHA256_HEX_LEN
        || !package
            .program_sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(ServiceError::new(Code::InvalidArgument, "package digest"));
    }
    if package.signature.is_empty() || package.publisher_key_id.is_empty() {
        return Err(ServiceError::new(
            Code::InvalidArgument,
            "package signature",
        ));
    }
    Ok(())
}

fn validate_actor(actor: &str) -> Result<()> {
    if actor.is_empty() || actor.len() > MAX_ACTOR_LEN {
        return Err(ServiceError::new(Code::Unauthorized, "invalid actor"));
    }
    Ok(())
}

fn validate_slot(slot: u8) -> Result<()> {
    if slot >= MAX_SAVE_SLOTS {
        return Err(ServiceError::new(Code::InvalidArgument, "save slot"));
    }
    Ok(())
}

fn save_key(actor: &str, package_id: &str, slot: u8) -> String {
    format!("{actor}\u{1f}{package_id}\u{1f}{slot}")
}

fn sanitize_url(raw: &str) -> Result<(String, String)> {
    if raw.is_empty() || raw.len() > MAX_URL_LEN || raw.chars().any(char::is_control) {
        return Err(ServiceError::new(Code::InvalidArgument, "url"));
    }
    let without_fragment = raw.split('#').next().unwrap_or("");
    let rest = without_fragment
        .strip_prefix("https://")
        .ok_or_else(|| ServiceError::new(Code::BlockedExternal, "https required"))?;
    let (authority, path_and_query) = rest.split_once('/').unwrap_or((rest, ""));
    if !valid_authority(authority) {
        return Err(ServiceError::new(Code::InvalidArgument, "url host"));
    }
    let (path, query) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));
    let kept: Vec<&str> = query
        .split('&')
        .filter(|part| !part.is_empty() && !sensitive_param(part))
        .collect();
    let url = if kept.is_empty() {
        format!("https://{authority}/{path}")
    } else {
        format!("https://{authority}/{path}?{}", kept.join("&"))
    };
    Ok((url, authority.to_ascii_lowercase()))
}

fn valid_authority(authority: &str) -> bool {
    !authority.is_empty()
        && authority.len() <= MAX_AUTHORITY_LEN
        && !authority.contains('@')
        && authority.contains('.')
        && authority
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
}

fn sensitive_param(pair: &str) -> bool {
    let key = pair.split('=').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        key.as_str(),
        "token"
            | "access_token"
            | "id_token"
            | "auth"
            | "code"
            | "session"
            | "sig"
            | "signature"
            | "key"
            | "secret"
            | "password"
    )
}

fn internal() -> ServiceError {
    ServiceError::new(Code::Internal, "game state")
}

#[derive(Deserialize)]
struct PurchaseParams {
    package_id: String,
    region: String,
}

fn to_provider_error(error: ServiceError) -> ProviderError {
    match error.code() {
        Code::NotFound => ProviderError::Rejected("not found"),
        Code::BlockedExternal => ProviderError::Rejected("region unavailable"),
        Code::Unauthorized => ProviderError::Rejected("unauthorized"),
        _ => ProviderError::Rejected("game rejected"),
    }
}

#[async_trait]
impl ServiceProvider for GameService {
    fn id(&self) -> &'static str {
        "games"
    }

    fn supports(&self, kind: ServiceType) -> bool {
        kind == ServiceType::Game
    }

    fn sandbox_evidence(&self) -> Option<SandboxEvidence> {
        self.evidence.clone()
    }

    fn support_hint(&self) -> Option<String> {
        Some("support://service?provider=games".to_owned())
    }

    async fn availability(&self, ctx: &QuoteContext<'_>) -> ProviderResult<Availability> {
        let parsed: PurchaseParams = serde_json::from_value(ctx.params.clone())
            .map_err(|_| ProviderError::Rejected("ctx.params"))?;
        let package = self
            .package(&parsed.package_id)
            .map_err(to_provider_error)?;
        if parsed.region != ctx.card.region
            || !package.regions.iter().any(|area| area == &ctx.card.region)
        {
            return Err(ProviderError::Rejected("region unavailable"));
        }
        Ok(Availability {
            available: true,
            amount_minor: package.price_minor,
            currency: package.currency,
            terms_version: format!("game:{}:{}", package.package_id, package.version),
            valid_until_ms: self.clock.now_ms() + OFFER_TTL_MS,
            note: None,
        })
    }

    async fn place_order(&self, request: &ProviderOrderRequest) -> ProviderResult<ProviderOrder> {
        let parsed: PurchaseParams = serde_json::from_value(request.params.clone())
            .map_err(|_| ProviderError::Rejected("params"))?;
        if parsed.region != request.region {
            return Err(ProviderError::Rejected("region mismatch"));
        }
        let package = self
            .package(&parsed.package_id)
            .map_err(to_provider_error)?;
        if package.price_minor != request.amount_minor || package.currency != request.currency {
            return Err(ProviderError::Rejected("price changed"));
        }
        let ticket = self
            .launch(&request.actor, &parsed.package_id, &parsed.region)
            .map_err(to_provider_error)?;
        let entitlement = Entitlement {
            actor: request.actor.clone(),
            package_id: parsed.package_id,
            active: true,
            price_minor: package.price_minor,
        };
        self.entitlements
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .insert(ticket.ticket_id.clone(), entitlement);
        Ok(ProviderOrder {
            provider_order_id: ticket.ticket_id.clone(),
            state: ProviderOrderState::Fulfilled,
            receipt: Some(ProviderReceipt {
                business_no: ticket.ticket_id,
                status: "entitled".to_owned(),
                message: "launch ticket issued".to_owned(),
                issued_at_ms: self.clock.now_ms(),
            }),
        })
    }

    async fn fulfill(&self, order_ref: &str, _key: &str) -> ProviderResult<ProviderReceipt> {
        let entitlements = self
            .entitlements
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        let entitlement = entitlements
            .get(order_ref)
            .filter(|entry| entry.active)
            .ok_or(ProviderError::Rejected("entitlement inactive"))?;
        Ok(ProviderReceipt {
            business_no: order_ref.to_owned(),
            status: "entitled".to_owned(),
            message: entitlement.package_id.clone(),
            issued_at_ms: self.clock.now_ms(),
        })
    }

    async fn cancel(&self, order_ref: &str, _key: &str) -> ProviderResult<ProviderCancellation> {
        let mut entitlements = self
            .entitlements
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        let entitlement = entitlements
            .get_mut(order_ref)
            .ok_or(ProviderError::Rejected("unknown entitlement"))?;
        if !entitlement.active {
            return Ok(ProviderCancellation {
                accepted: true,
                refund_due_minor: 0,
                message: "already revoked".to_owned(),
            });
        }
        entitlement.active = false;
        Ok(ProviderCancellation {
            accepted: true,
            refund_due_minor: entitlement.price_minor,
            message: "revoked".to_owned(),
        })
    }

    async fn query(&self, order_ref: &str) -> ProviderResult<ProviderStatus> {
        let entitlements = self
            .entitlements
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        let entitlement = entitlements
            .get(order_ref)
            .ok_or(ProviderError::Rejected("unknown entitlement"))?;
        let state = if entitlement.active {
            ProviderOrderState::Fulfilled
        } else {
            ProviderOrderState::Cancelled
        };
        Ok(ProviderStatus {
            state,
            receipt: Some(ProviderReceipt {
                business_no: order_ref.to_owned(),
                status: if entitlement.active {
                    "entitled"
                } else {
                    "revoked"
                }
                .to_owned(),
                message: entitlement.package_id.clone(),
                issued_at_ms: self.clock.now_ms(),
            }),
        })
    }
}
