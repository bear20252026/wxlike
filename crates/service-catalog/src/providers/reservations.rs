use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::prelude::*;

pub const MAX_HOLD_TTL_MS: i64 = 15 * 60 * 1000;
pub const OFFER_TTL_MS: i64 = 5 * 60 * 1000;
pub const MAX_PARTY_SIZE: u32 = 9;
pub const MAX_NAME_LEN: usize = 64;
pub const MAX_ID_LEN: usize = 32;
pub const MAX_NOTE_LEN: usize = 500;
pub const NONCE_LEN: usize = 12;
const PROFILE_KEY_LEN: usize = 32;
const MAX_REFERRAL_LEN: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationKind {
    Travel,
    Hotel,
    Movie,
    Medical,
}

impl ReservationKind {
    pub fn from_card(kind: ServiceType) -> Option<Self> {
        match kind {
            ServiceType::Travel => Some(Self::Travel),
            ServiceType::Hotel => Some(Self::Hotel),
            ServiceType::Movie => Some(Self::Movie),
            ServiceType::Medical => Some(Self::Medical),
            _ => None,
        }
    }

    pub fn requires_authorization(self) -> bool {
        matches!(self, Self::Medical)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReservationQuery {
    pub kind: ReservationKind,
    pub region: String,
    pub city: String,
    pub party_size: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub offer_id: String,
    pub kind: ReservationKind,
    pub title: String,
    pub price_minor: i64,
    pub currency: String,
    pub starts_at_ms: i64,
    pub region: String,
    pub terms_version: String,
    /// 医疗类必须由已授权机构提供号源，客户端不能自造。
    pub authorized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookingStatus {
    Held,
    Confirmed,
    Cancelled,
    Refunded,
    Failed,
    PendingReconciliation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub hold_id: String,
    pub offer_id: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Booking {
    pub booking_id: String,
    pub hold_id: String,
    pub status: BookingStatus,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitiveProfile {
    pub passenger_name: String,
    pub id_number: String,
    pub health_note: Option<String>,
}

impl SensitiveProfile {
    fn validate(&self) -> Result<()> {
        if self.passenger_name.is_empty() || self.passenger_name.len() > MAX_NAME_LEN {
            return Err(ServiceError::new(Code::InvalidArgument, "passenger name"));
        }
        if self.id_number.is_empty() || self.id_number.len() > MAX_ID_LEN {
            return Err(ServiceError::new(Code::InvalidArgument, "id number"));
        }
        if self
            .health_note
            .as_ref()
            .is_some_and(|note| note.len() > MAX_NOTE_LEN)
        {
            return Err(ServiceError::new(Code::InvalidArgument, "health note"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedProfile {
    pub ciphertext: String,
    pub nonce: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub actor: String,
    pub booking_id: String,
    pub action: String,
    pub at_ms: i64,
}

struct StoredProfile {
    actor: String,
    encrypted: EncryptedProfile,
}

/// 敏感资料最小化加密存储；读取必须同一用户并写审计，审计不含证件号。
pub struct SensitiveStore {
    key: [u8; PROFILE_KEY_LEN],
    records: Mutex<HashMap<String, StoredProfile>>,
    audit: Mutex<Vec<AuditEntry>>,
}

impl SensitiveStore {
    pub fn new(key: [u8; PROFILE_KEY_LEN]) -> Self {
        Self {
            key,
            records: Mutex::new(HashMap::new()),
            audit: Mutex::new(Vec::new()),
        }
    }

    pub fn seal(&self, actor: &str, booking_id: &str, profile: &SensitiveProfile) -> Result<()> {
        profile.validate()?;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key));
        let plaintext = serde_json::to_vec(profile)
            .map_err(|_| ServiceError::new(Code::Internal, "profile encode"))?;
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_slice())
            .map_err(|_| ServiceError::new(Code::Internal, "profile encrypt"))?;
        let stored = StoredProfile {
            actor: actor.to_owned(),
            encrypted: EncryptedProfile {
                ciphertext: hex::encode(ciphertext),
                nonce: hex::encode(nonce_bytes),
            },
        };
        self.records
            .lock()
            .map_err(|_| internal())?
            .insert(booking_id.to_owned(), stored);
        Ok(())
    }

    pub fn open(
        &self,
        actor: &str,
        booking_id: &str,
        purpose: &str,
        now_ms: i64,
    ) -> Result<SensitiveProfile> {
        let encrypted = {
            let records = self.records.lock().map_err(|_| internal())?;
            let record = records
                .get(booking_id)
                .ok_or_else(|| ServiceError::new(Code::NotFound, "profile not found"))?;
            if record.actor != actor {
                return Err(ServiceError::new(Code::NotFound, "profile not found"));
            }
            record.encrypted.clone()
        };
        let nonce = hex::decode(&encrypted.nonce)
            .map_err(|_| ServiceError::new(Code::Internal, "profile nonce"))?;
        let ciphertext = hex::decode(&encrypted.ciphertext)
            .map_err(|_| ServiceError::new(Code::Internal, "profile cipher"))?;
        if nonce.len() != NONCE_LEN {
            return Err(ServiceError::new(Code::Internal, "profile nonce"));
        }
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key));
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&nonce), ciphertext.as_slice())
            .map_err(|_| ServiceError::new(Code::Internal, "profile decrypt"))?;
        let profile: SensitiveProfile = serde_json::from_slice(&plaintext)
            .map_err(|_| ServiceError::new(Code::Internal, "profile decode"))?;
        self.audit.lock().map_err(|_| internal())?.push(AuditEntry {
            actor: actor.to_owned(),
            booking_id: booking_id.to_owned(),
            action: purpose.to_owned(),
            at_ms: now_ms,
        });
        Ok(profile)
    }

    pub fn delete(&self, actor: &str, booking_id: &str) -> bool {
        let Ok(mut records) = self.records.lock() else {
            return false;
        };
        match records.get(booking_id) {
            Some(record) if record.actor == actor => records.remove(booking_id).is_some(),
            _ => false,
        }
    }

    /// 仅返回密文与 nonce，供同步/迁移核对；明文只经 `open` 并留审计。
    pub fn stored(&self, actor: &str, booking_id: &str) -> Option<EncryptedProfile> {
        let records = self.records.lock().ok()?;
        records
            .get(booking_id)
            .filter(|record| record.actor == actor)
            .map(|record| record.encrypted.clone())
    }

    pub fn audit(&self) -> Vec<AuditEntry> {
        self.audit
            .lock()
            .map(|audit| audit.clone())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SagaStage {
    pub name: String,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookingView {
    pub booking: Booking,
    pub offer: Offer,
    pub stages: Vec<SagaStage>,
}

#[async_trait]
pub trait ReservationVendor: Send + Sync {
    async fn search(&self, query: &ReservationQuery) -> ProviderResult<Vec<Offer>>;

    async fn hold(&self, offer_id: &str, idempotency_key: &str) -> ProviderResult<Hold>;

    async fn release_hold(&self, hold_id: &str, idempotency_key: &str) -> ProviderResult<()>;

    async fn confirm(
        &self,
        offer: &Offer,
        hold_id: &str,
        profile: &SensitiveProfile,
        idempotency_key: &str,
    ) -> ProviderResult<Booking>;

    async fn cancel(
        &self,
        booking_id: &str,
        idempotency_key: &str,
    ) -> ProviderResult<ProviderCancellation>;

    async fn refund(
        &self,
        booking_id: &str,
        idempotency_key: &str,
    ) -> ProviderResult<ProviderCancellation>;

    async fn query(&self, booking_id: &str) -> ProviderResult<Booking>;
}

#[derive(Clone)]
struct HoldRecord {
    hold_id: String,
    actor: String,
    offer: Offer,
    expires_at_ms: i64,
    referral: Option<String>,
}

#[derive(Clone)]
struct BookingRecord {
    actor: String,
    offer: Offer,
    booking: Booking,
    stages: Vec<SagaStage>,
}

pub struct ReservationService<V: ReservationVendor> {
    vendor: V,
    evidence: Option<SandboxEvidence>,
    clock: Arc<dyn Clock>,
    sensitive: SensitiveStore,
    offers: Mutex<HashMap<String, Offer>>,
    holds: Mutex<HashMap<String, HoldRecord>>,
    bookings: Mutex<HashMap<String, BookingRecord>>,
}

impl<V: ReservationVendor> ReservationService<V> {
    pub fn new(
        vendor: V,
        evidence: Option<SandboxEvidence>,
        clock: Arc<dyn Clock>,
        profile_key: [u8; PROFILE_KEY_LEN],
    ) -> Self {
        Self {
            vendor,
            evidence,
            clock,
            sensitive: SensitiveStore::new(profile_key),
            offers: Mutex::new(HashMap::new()),
            holds: Mutex::new(HashMap::new()),
            bookings: Mutex::new(HashMap::new()),
        }
    }

    /// 搜索只返回供应商实时库存；医疗类还必须带机构授权标记。
    pub async fn search(&self, query: &ReservationQuery) -> Result<Vec<Offer>> {
        if query.party_size == 0 || query.party_size > MAX_PARTY_SIZE {
            return Err(ServiceError::new(Code::InvalidArgument, "party size"));
        }
        let offers = self
            .vendor
            .search(query)
            .await
            .map_err(|err| crate::provider::map_provider_error("reservations", err))?;
        let valid: Vec<Offer> = offers
            .into_iter()
            .filter(|offer| offer.region == query.region && offer.kind == query.kind)
            .filter(|offer| !query.kind.requires_authorization() || offer.authorized)
            .collect();
        if let Ok(mut cache) = self.offers.lock() {
            for offer in &valid {
                cache.insert(offer.offer_id.clone(), offer.clone());
            }
        }
        Ok(valid)
    }

    pub async fn reserve(
        &self,
        actor: &str,
        offer_id: &str,
        referral: Option<&str>,
    ) -> Result<Hold> {
        let offer = self
            .offers
            .lock()
            .map_err(|_| internal())?
            .get(offer_id)
            .cloned()
            .ok_or_else(|| ServiceError::new(Code::NotFound, "offer not found"))?;
        self.require_authorization(&offer, referral)?;
        let idem = format!("{actor}:{offer_id}:hold");
        let hold = self
            .vendor
            .hold(offer_id, &idem)
            .await
            .map_err(|err| crate::provider::map_provider_error("reservations", err))?;
        let now = self.clock.now_ms();
        if hold.offer_id != offer_id || hold.expires_at_ms <= now {
            return Err(ServiceError::new(
                Code::DependencyUnavailable,
                "hold expired",
            ));
        }
        if hold.expires_at_ms - now > MAX_HOLD_TTL_MS {
            return Err(ServiceError::new(Code::DependencyUnavailable, "hold ttl"));
        }
        self.holds.lock().map_err(|_| internal())?.insert(
            hold.hold_id.clone(),
            HoldRecord {
                hold_id: hold.hold_id.clone(),
                actor: actor.to_owned(),
                offer,
                expires_at_ms: hold.expires_at_ms,
                referral: referral.map(str::to_owned),
            },
        );
        Ok(hold)
    }

    pub async fn confirm(
        &self,
        actor: &str,
        hold_id: &str,
        profile: &SensitiveProfile,
    ) -> Result<BookingView> {
        let hold = self
            .holds
            .lock()
            .map_err(|_| internal())?
            .get(hold_id)
            .cloned()
            .ok_or_else(|| ServiceError::new(Code::NotFound, "hold not found"))?;
        if hold.actor != actor {
            return Err(ServiceError::new(Code::NotFound, "hold not found"));
        }
        let now = self.clock.now_ms();
        if hold.expires_at_ms <= now {
            return Err(ServiceError::new(Code::Expired, "hold expired"));
        }
        self.require_authorization(&hold.offer, hold.referral.as_deref())?;
        let mut stages = initial_stages();
        let idem = format!("{actor}:{hold_id}:confirm");
        let booking = match self
            .vendor
            .confirm(&hold.offer, hold_id, profile, &idem)
            .await
        {
            Ok(booking) => booking,
            Err(err) => {
                stages.push(self.compensate_hold(&hold, &idem).await);
                return Err(crate::provider::map_provider_error("reservations", err));
            }
        };
        stages[1].done = true;
        self.sensitive.seal(actor, &booking.booking_id, profile)?;
        let record = BookingRecord {
            actor: actor.to_owned(),
            offer: hold.offer.clone(),
            booking: booking.clone(),
            stages,
        };
        self.bookings
            .lock()
            .map_err(|_| internal())?
            .insert(booking.booking_id.clone(), record.clone());
        Ok(BookingView {
            booking,
            offer: record.offer,
            stages: record.stages,
        })
    }

    async fn compensate_hold(&self, hold: &HoldRecord, idem: &str) -> SagaStage {
        let key = format!("{idem}:release");
        let released = self.vendor.release_hold(&hold.hold_id, &key).await.is_ok();
        if released {
            if let Ok(mut holds) = self.holds.lock() {
                holds.remove(&hold.hold_id);
            }
        }
        SagaStage {
            name: "release_hold".to_owned(),
            done: released,
        }
    }

    pub async fn cancel(&self, actor: &str, booking_id: &str) -> Result<Booking> {
        let record = self.owned_booking(actor, booking_id)?;
        if matches!(
            record.booking.status,
            BookingStatus::Cancelled | BookingStatus::Refunded
        ) {
            return Ok(record.booking);
        }
        let idem = format!("{actor}:{booking_id}:cancel");
        let cancellation = self
            .vendor
            .cancel(booking_id, &idem)
            .await
            .map_err(|err| crate::provider::map_provider_error("reservations", err))?;
        if !cancellation.accepted {
            return Err(ServiceError::new(Code::Conflict, "cancellation rejected"));
        }
        self.set_booking_status(actor, booking_id, BookingStatus::Cancelled)
    }

    pub async fn refund(&self, actor: &str, booking_id: &str) -> Result<Booking> {
        let record = self.owned_booking(actor, booking_id)?;
        if record.booking.status == BookingStatus::Refunded {
            return Ok(record.booking);
        }
        let idem = format!("{actor}:{booking_id}:refund");
        let refund = self
            .vendor
            .refund(booking_id, &idem)
            .await
            .map_err(|err| crate::provider::map_provider_error("reservations", err))?;
        if !refund.accepted {
            return Err(ServiceError::new(Code::Conflict, "refund rejected"));
        }
        self.set_booking_status(actor, booking_id, BookingStatus::Refunded)
    }

    pub fn view(&self, actor: &str, booking_id: &str) -> Result<BookingView> {
        let record = self.owned_booking(actor, booking_id)?;
        Ok(BookingView {
            booking: record.booking,
            offer: record.offer,
            stages: record.stages,
        })
    }

    pub fn profile(
        &self,
        actor: &str,
        booking_id: &str,
        purpose: &str,
    ) -> Result<SensitiveProfile> {
        self.owned_booking(actor, booking_id)?;
        self.sensitive
            .open(actor, booking_id, purpose, self.clock.now_ms())
    }

    pub fn delete_profile(&self, actor: &str, booking_id: &str) -> bool {
        self.owned_booking(actor, booking_id).is_ok() && self.sensitive.delete(actor, booking_id)
    }

    pub fn audit(&self) -> Vec<AuditEntry> {
        self.sensitive.audit()
    }

    fn require_authorization(&self, offer: &Offer, referral: Option<&str>) -> Result<()> {
        if !offer.kind.requires_authorization() {
            return Ok(());
        }
        if !offer.authorized {
            return Err(ServiceError::new(
                Code::BlockedExternal,
                "medical source unauthorized",
            ));
        }
        match referral {
            Some(referral) if !referral.is_empty() && referral.len() <= MAX_REFERRAL_LEN => Ok(()),
            _ => Err(ServiceError::new(Code::Unauthorized, "referral required")),
        }
    }

    fn owned_booking(&self, actor: &str, booking_id: &str) -> Result<BookingRecord> {
        let bookings = self.bookings.lock().map_err(|_| internal())?;
        let record = bookings
            .get(booking_id)
            .ok_or_else(|| ServiceError::new(Code::NotFound, "booking not found"))?;
        if record.actor != actor {
            return Err(ServiceError::new(Code::NotFound, "booking not found"));
        }
        Ok(record.clone())
    }

    fn set_booking_status(
        &self,
        actor: &str,
        booking_id: &str,
        status: BookingStatus,
    ) -> Result<Booking> {
        let mut bookings = self.bookings.lock().map_err(|_| internal())?;
        let record = bookings
            .get_mut(booking_id)
            .ok_or_else(|| ServiceError::new(Code::NotFound, "booking not found"))?;
        if record.actor != actor {
            return Err(ServiceError::new(Code::NotFound, "booking not found"));
        }
        record.booking.status = status;
        Ok(record.booking.clone())
    }
}

fn status_of(status: BookingStatus) -> &'static str {
    match status {
        BookingStatus::Held => "held",
        BookingStatus::Confirmed => "confirmed",
        BookingStatus::Cancelled => "cancelled",
        BookingStatus::Refunded => "refunded",
        BookingStatus::Failed => "failed",
        BookingStatus::PendingReconciliation => "pending_reconciliation",
    }
}

fn provider_state(status: BookingStatus) -> ProviderOrderState {
    match status {
        BookingStatus::Held => ProviderOrderState::Accepted,
        BookingStatus::Confirmed => ProviderOrderState::Fulfilled,
        BookingStatus::Cancelled | BookingStatus::Refunded => ProviderOrderState::Cancelled,
        BookingStatus::Failed => ProviderOrderState::Failed,
        BookingStatus::PendingReconciliation => ProviderOrderState::Pending,
    }
}

#[derive(Deserialize)]
struct OfferParams {
    offer_id: String,
}

#[derive(Deserialize)]
struct ReservationParams {
    offer_id: String,
    #[serde(default)]
    referral_ref: Option<String>,
    profile: SensitiveProfile,
}

fn to_provider_error(error: ServiceError) -> ProviderError {
    match error.code() {
        Code::NotFound => ProviderError::Rejected("not found"),
        Code::Expired => ProviderError::Rejected("hold expired"),
        Code::Conflict => ProviderError::Rejected("conflict"),
        _ => ProviderError::Rejected("reservation rejected"),
    }
}

#[async_trait]
impl<V: ReservationVendor> ServiceProvider for ReservationService<V> {
    fn id(&self) -> &'static str {
        "reservations"
    }

    fn supports(&self, kind: ServiceType) -> bool {
        ReservationKind::from_card(kind).is_some()
    }

    fn sandbox_evidence(&self) -> Option<SandboxEvidence> {
        self.evidence.clone()
    }

    fn support_hint(&self) -> Option<String> {
        Some("support://service?provider=reservations".to_owned())
    }

    async fn availability(&self, ctx: &QuoteContext<'_>) -> ProviderResult<Availability> {
        let parsed: OfferParams = serde_json::from_value(ctx.params.clone())
            .map_err(|_| ProviderError::Rejected("ctx.params"))?;
        let expected =
            ReservationKind::from_card(ctx.card.kind).ok_or(ProviderError::Rejected("kind"))?;
        let offer = self
            .offers
            .lock()
            .ok()
            .and_then(|offers| offers.get(&parsed.offer_id).cloned())
            .ok_or(ProviderError::Rejected("offer not found"))?;
        if offer.kind != expected || offer.region != ctx.card.region {
            return Err(ProviderError::Rejected("offer mismatch"));
        }
        Ok(Availability {
            available: true,
            amount_minor: offer.price_minor,
            currency: offer.currency,
            terms_version: offer.terms_version,
            valid_until_ms: self.clock.now_ms() + OFFER_TTL_MS,
            note: None,
        })
    }

    async fn place_order(&self, request: &ProviderOrderRequest) -> ProviderResult<ProviderOrder> {
        let params: ReservationParams = serde_json::from_value(request.params.clone())
            .map_err(|_| ProviderError::Rejected("params"))?;
        let expected =
            ReservationKind::from_card(request.kind).ok_or(ProviderError::Rejected("kind"))?;
        let offer = self
            .offers
            .lock()
            .ok()
            .and_then(|offers| offers.get(&params.offer_id).cloned())
            .ok_or(ProviderError::Rejected("offer not found"))?;
        if offer.kind != expected
            || offer.region != request.region
            || offer.price_minor != request.amount_minor
            || offer.currency != request.currency
        {
            return Err(ProviderError::Rejected("offer changed"));
        }
        let hold = self
            .reserve(
                &request.actor,
                &params.offer_id,
                params.referral_ref.as_deref(),
            )
            .await
            .map_err(to_provider_error)?;
        let view = self
            .confirm(&request.actor, &hold.hold_id, &params.profile)
            .await
            .map_err(to_provider_error)?;
        Ok(ProviderOrder {
            provider_order_id: view.booking.booking_id.clone(),
            state: ProviderOrderState::Accepted,
            receipt: Some(ProviderReceipt {
                business_no: view.booking.booking_id,
                status: status_of(view.booking.status).to_owned(),
                message: view.booking.message,
                issued_at_ms: self.clock.now_ms(),
            }),
        })
    }

    async fn fulfill(&self, order_ref: &str, _key: &str) -> ProviderResult<ProviderReceipt> {
        let booking = self.vendor.query(order_ref).await?;
        if booking.status != BookingStatus::Confirmed {
            return Err(ProviderError::Rejected("not confirmed"));
        }
        Ok(ProviderReceipt {
            business_no: booking.booking_id,
            status: status_of(booking.status).to_owned(),
            message: booking.message,
            issued_at_ms: self.clock.now_ms(),
        })
    }

    async fn cancel(&self, order_ref: &str, _key: &str) -> ProviderResult<ProviderCancellation> {
        let actor = {
            let bookings = self
                .bookings
                .lock()
                .map_err(|_| ProviderError::Unavailable)?;
            bookings
                .get(order_ref)
                .map(|record| record.actor.clone())
                .ok_or(ProviderError::Rejected("unknown booking"))?
        };
        let record = self
            .owned_booking(&actor, order_ref)
            .map_err(|_| ProviderError::Rejected("unknown booking"))?;
        self.cancel(&actor, order_ref)
            .await
            .map_err(to_provider_error)?;
        Ok(ProviderCancellation {
            accepted: true,
            refund_due_minor: record.offer.price_minor,
            message: "cancelled".to_owned(),
        })
    }

    async fn query(&self, order_ref: &str) -> ProviderResult<ProviderStatus> {
        let booking = self.vendor.query(order_ref).await?;
        Ok(ProviderStatus {
            state: provider_state(booking.status),
            receipt: Some(ProviderReceipt {
                business_no: booking.booking_id,
                status: status_of(booking.status).to_owned(),
                message: booking.message,
                issued_at_ms: self.clock.now_ms(),
            }),
        })
    }
}

fn initial_stages() -> Vec<SagaStage> {
    vec![
        SagaStage {
            name: "hold".to_owned(),
            done: true,
        },
        SagaStage {
            name: "confirm".to_owned(),
            done: false,
        },
    ]
}

fn internal() -> ServiceError {
    ServiceError::new(Code::Internal, "reservation state")
}
