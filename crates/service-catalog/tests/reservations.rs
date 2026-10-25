mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use wxlike_service_catalog::auth::BearerAuthenticator;
use wxlike_service_catalog::providers::reservations::{
    Booking, BookingStatus, Hold, Offer, ReservationKind, ReservationQuery, ReservationService,
    ReservationVendor, SensitiveProfile, SensitiveStore,
};
use wxlike_service_catalog::{
    Catalog, Clock, CreateOrderRequest, OrderService, OrderState, ProviderCancellation,
    ProviderError, ServiceErrorCode as Code, ServiceType,
};

use common::{card, evidence, LedgerPayment, TestClock};

const PROFILE: &str =
    r#"{"passenger_name":"张三","id_number":"110101199001011234","health_note":"膝伤"}"#;

#[derive(Default)]
struct VendorState {
    offers: Vec<Offer>,
    hold_ttl_ms: i64,
    hold_error: Option<ProviderError>,
    confirm_error: Option<ProviderError>,
    confirm_status: Option<BookingStatus>,
    cancel_accept: bool,
    refund_accept: bool,
    refund_due_minor: i64,
    release_calls: usize,
    confirm_calls: usize,
    cancel_calls: usize,
    refund_calls: usize,
    bookings: HashMap<String, Booking>,
}

#[derive(Clone)]
struct FakeVendor {
    state: Arc<Mutex<VendorState>>,
    clock: Arc<TestClock>,
}

impl FakeVendor {
    fn new(clock: Arc<TestClock>) -> Self {
        Self {
            state: Arc::new(Mutex::new(VendorState {
                hold_ttl_ms: 5 * 60 * 1000,
                cancel_accept: true,
                refund_accept: true,
                refund_due_minor: 2_000,
                ..VendorState::default()
            })),
            clock,
        }
    }

    fn with<F: FnOnce(&mut VendorState)>(&self, change: F) {
        change(&mut self.state.lock().unwrap());
    }
}

fn offer(offer_id: &str, kind: ReservationKind, region: &str, authorized: bool) -> Offer {
    Offer {
        offer_id: offer_id.to_owned(),
        kind,
        title: format!("{offer_id} title"),
        price_minor: if kind == ReservationKind::Medical {
            800
        } else {
            2_000
        },
        currency: "CNY".to_owned(),
        starts_at_ms: 1_000_000,
        region: region.to_owned(),
        terms_version: "terms-v1".to_owned(),
        authorized,
    }
}

#[async_trait]
impl ReservationVendor for FakeVendor {
    async fn search(&self, _query: &ReservationQuery) -> Result<Vec<Offer>, ProviderError> {
        Ok(self.state.lock().unwrap().offers.clone())
    }

    async fn hold(&self, offer_id: &str, _idempotency_key: &str) -> Result<Hold, ProviderError> {
        let state = self.state.lock().unwrap();
        if let Some(err) = state.hold_error {
            return Err(err);
        }
        Ok(Hold {
            hold_id: format!("hold-{offer_id}"),
            offer_id: offer_id.to_owned(),
            expires_at_ms: self.clock.now_ms() + state.hold_ttl_ms,
        })
    }

    async fn release_hold(
        &self,
        _hold_id: &str,
        _idempotency_key: &str,
    ) -> Result<(), ProviderError> {
        self.state.lock().unwrap().release_calls += 1;
        Ok(())
    }

    async fn confirm(
        &self,
        _offer: &Offer,
        hold_id: &str,
        _profile: &SensitiveProfile,
        _idempotency_key: &str,
    ) -> Result<Booking, ProviderError> {
        let mut state = self.state.lock().unwrap();
        state.confirm_calls += 1;
        if let Some(err) = state.confirm_error {
            return Err(err);
        }
        let booking = Booking {
            booking_id: format!("booking-{hold_id}"),
            hold_id: hold_id.to_owned(),
            status: state.confirm_status.unwrap_or(BookingStatus::Confirmed),
            message: "confirmed".to_owned(),
        };
        state
            .bookings
            .insert(booking.booking_id.clone(), booking.clone());
        Ok(booking)
    }

    async fn cancel(
        &self,
        booking_id: &str,
        _idempotency_key: &str,
    ) -> Result<ProviderCancellation, ProviderError> {
        let mut state = self.state.lock().unwrap();
        state.cancel_calls += 1;
        if state.cancel_accept {
            if let Some(booking) = state.bookings.get_mut(booking_id) {
                booking.status = BookingStatus::Cancelled;
            }
        }
        Ok(ProviderCancellation {
            accepted: state.cancel_accept,
            refund_due_minor: state.refund_due_minor,
            message: "cancelled".to_owned(),
        })
    }

    async fn refund(
        &self,
        booking_id: &str,
        _idempotency_key: &str,
    ) -> Result<ProviderCancellation, ProviderError> {
        let mut state = self.state.lock().unwrap();
        state.refund_calls += 1;
        if state.refund_accept {
            if let Some(booking) = state.bookings.get_mut(booking_id) {
                booking.status = BookingStatus::Refunded;
            }
        }
        Ok(ProviderCancellation {
            accepted: state.refund_accept,
            refund_due_minor: state.refund_due_minor,
            message: "refunded".to_owned(),
        })
    }

    async fn query(&self, booking_id: &str) -> Result<Booking, ProviderError> {
        let state = self.state.lock().unwrap();
        state
            .bookings
            .get(booking_id)
            .cloned()
            .ok_or(ProviderError::Rejected("unknown booking"))
    }
}

struct Harness {
    service: OrderService,
    provider: Arc<ReservationService<FakeVendor>>,
    vendor: FakeVendor,
    payment: Arc<LedgerPayment>,
    clock: Arc<TestClock>,
}

fn harness() -> Harness {
    let clock = Arc::new(TestClock::new(1_000));
    let vendor = FakeVendor::new(clock.clone());
    vendor.with(|state| {
        state.offers = vec![
            offer("off-travel", ReservationKind::Travel, "CN", true),
            offer("off-travel-us", ReservationKind::Travel, "US", true),
            offer("off-hotel", ReservationKind::Hotel, "CN", true),
            offer("off-medical", ReservationKind::Medical, "CN", true),
            offer(
                "off-medical-unauthorized",
                ReservationKind::Medical,
                "CN",
                false,
            ),
        ];
    });
    let provider = Arc::new(ReservationService::new(
        vendor.clone(),
        evidence(),
        clock.clone(),
        [7u8; 32],
    ));
    let catalog = Arc::new(Catalog::new());
    for kind in [
        ServiceType::Travel,
        ServiceType::Hotel,
        ServiceType::Movie,
        ServiceType::Medical,
    ] {
        let id = format!("{}-cn", kind.as_str());
        catalog
            .upsert(card(&id, kind, "reservations", "CN"))
            .unwrap();
        catalog.set_region_enabled(kind, "CN", true).unwrap();
    }
    let payment = Arc::new(LedgerPayment::default());
    let auth = Arc::new(BearerAuthenticator::new(|token: &str| match token {
        "s1" => Some("u1".to_owned()),
        "s2" => Some("u2".to_owned()),
        _ => None,
    }));
    let service = OrderService::new(
        catalog,
        vec![provider.clone()],
        payment.clone(),
        auth,
        clock.clone(),
    );
    Harness {
        service,
        provider,
        vendor,
        payment,
        clock,
    }
}

fn query(kind: ReservationKind) -> ReservationQuery {
    ReservationQuery {
        kind,
        region: "CN".to_owned(),
        city: "SH".to_owned(),
        party_size: 2,
    }
}

fn profile() -> SensitiveProfile {
    serde_json::from_str(PROFILE).unwrap()
}

fn travel_params() -> Value {
    json!({"offer_id": "off-travel", "profile": serde_json::from_str::<Value>(PROFILE).unwrap()})
}

#[tokio::test]
async fn search_filters_region_kind_and_unauthorized_medical() {
    let h = harness();
    let travel = h
        .provider
        .search(&query(ReservationKind::Travel))
        .await
        .unwrap();
    assert_eq!(travel.len(), 1);
    assert_eq!(travel[0].offer_id, "off-travel");

    let medical = h
        .provider
        .search(&query(ReservationKind::Medical))
        .await
        .unwrap();
    assert_eq!(medical.len(), 1);
    assert_eq!(medical[0].offer_id, "off-medical");

    let hotel = h
        .provider
        .search(&query(ReservationKind::Hotel))
        .await
        .unwrap();
    assert_eq!(hotel[0].offer_id, "off-hotel");

    let mut invalid = query(ReservationKind::Travel);
    invalid.party_size = 0;
    assert_eq!(
        h.provider.search(&invalid).await.unwrap_err().code(),
        Code::InvalidArgument
    );
}

#[tokio::test]
async fn hold_ttl_is_bounded_and_expired_hold_cannot_confirm() {
    let h = harness();
    h.provider
        .search(&query(ReservationKind::Travel))
        .await
        .unwrap();

    h.vendor.with(|state| state.hold_ttl_ms = 20 * 60 * 1000);
    assert_eq!(
        h.provider
            .reserve("u1", "off-travel", None)
            .await
            .unwrap_err()
            .code(),
        Code::DependencyUnavailable
    );

    h.vendor.with(|state| state.hold_ttl_ms = 5 * 60 * 1000);
    let hold = h.provider.reserve("u1", "off-travel", None).await.unwrap();
    h.clock.advance(6 * 60 * 1000);
    assert_eq!(
        h.provider
            .confirm("u1", &hold.hold_id, &profile())
            .await
            .unwrap_err()
            .code(),
        Code::Expired
    );
}

#[tokio::test]
async fn confirm_failure_releases_the_hold() {
    let h = harness();
    h.provider
        .search(&query(ReservationKind::Travel))
        .await
        .unwrap();
    let hold = h.provider.reserve("u1", "off-travel", None).await.unwrap();
    h.vendor
        .with(|state| state.confirm_error = Some(ProviderError::Unavailable));

    let err = h
        .provider
        .confirm("u1", &hold.hold_id, &profile())
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::DependencyUnavailable);
    assert_eq!(h.vendor.state.lock().unwrap().release_calls, 1);
    assert_eq!(h.vendor.state.lock().unwrap().confirm_calls, 1);
}

#[tokio::test]
async fn medical_reservation_requires_authorized_offer_and_referral() {
    let h = harness();
    h.provider
        .search(&query(ReservationKind::Medical))
        .await
        .unwrap();

    assert_eq!(
        h.provider
            .reserve("u1", "off-medical", None)
            .await
            .unwrap_err()
            .code(),
        Code::Unauthorized
    );
    assert_eq!(
        h.provider
            .reserve("u1", "off-medical", Some(""))
            .await
            .unwrap_err()
            .code(),
        Code::Unauthorized
    );
    assert_eq!(
        h.provider
            .reserve("u1", "off-medical-unauthorized", Some("ref-1"))
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );

    let hold = h
        .provider
        .reserve("u1", "off-medical", Some("ref-1"))
        .await
        .unwrap();
    let view = h
        .provider
        .confirm("u1", &hold.hold_id, &profile())
        .await
        .unwrap();
    assert_eq!(view.booking.status, BookingStatus::Confirmed);
}

#[tokio::test]
async fn sensitive_profile_is_encrypted_owner_scoped_audited_and_deletable() {
    let store = SensitiveStore::new([9u8; 32]);
    let profile = profile();
    store.seal("u1", "booking-1", &profile).unwrap();

    let encrypted = store.stored("u1", "booking-1").unwrap();
    assert!(!encrypted.ciphertext.contains("110101199001011234"));
    assert_eq!(encrypted.nonce.len(), 24);
    assert!(store.stored("u2", "booking-1").is_none());
    assert_eq!(store.audit().len(), 0);

    let opened = store
        .open("u1", "booking-1", "travel_checkin", 1_000)
        .unwrap();
    assert_eq!(opened, profile);
    let audit = store.audit();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, "travel_checkin");
    assert_eq!(audit[0].booking_id, "booking-1");
    assert!(!format!("{audit:?}").contains("110101199001011234"));

    assert!(!store.delete("u2", "booking-1"));
    assert!(store.delete("u1", "booking-1"));
    assert!(store.stored("u1", "booking-1").is_none());
    assert_eq!(
        store
            .open("u1", "booking-1", "travel_checkin", 2_000)
            .unwrap_err()
            .code(),
        Code::NotFound
    );
}

#[tokio::test]
async fn booking_is_owner_scoped_and_cancel_then_refund_is_idempotent() {
    let h = harness();
    h.provider
        .search(&query(ReservationKind::Travel))
        .await
        .unwrap();
    let hold = h.provider.reserve("u1", "off-travel", None).await.unwrap();
    let view = h
        .provider
        .confirm("u1", &hold.hold_id, &profile())
        .await
        .unwrap();
    let booking_id = view.booking.booking_id.clone();
    assert_eq!(
        h.provider.view("u2", &booking_id).unwrap_err().code(),
        Code::NotFound
    );
    assert_eq!(
        h.provider
            .profile("u2", &booking_id, "peek")
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    assert!(!h.provider.delete_profile("u2", &booking_id));

    let opened = h.provider.profile("u1", &booking_id, "checkin").unwrap();
    assert_eq!(opened, profile());
    assert_eq!(h.provider.audit().len(), 1);

    let cancelled = h.provider.cancel("u1", &booking_id).await.unwrap();
    assert_eq!(cancelled.status, BookingStatus::Cancelled);
    let cancelled_again = h.provider.cancel("u1", &booking_id).await.unwrap();
    assert_eq!(cancelled_again.status, BookingStatus::Cancelled);
    assert_eq!(h.vendor.state.lock().unwrap().cancel_calls, 1);

    let refunded = h.provider.refund("u1", &booking_id).await.unwrap();
    assert_eq!(refunded.status, BookingStatus::Refunded);
    let refunded_again = h.provider.refund("u1", &booking_id).await.unwrap();
    assert_eq!(refunded_again.status, BookingStatus::Refunded);
    assert_eq!(h.vendor.state.lock().unwrap().refund_calls, 1);
    assert!(h.provider.delete_profile("u1", &booking_id));
}

#[tokio::test]
async fn reservation_order_flow_quotes_cached_offer_then_fulfills() {
    let h = harness();
    h.provider
        .search(&query(ReservationKind::Travel))
        .await
        .unwrap();

    let unknown = h
        .service
        .quote(
            "s1",
            "travel-cn",
            1,
            json!({"offer_id": "off-404", "profile": profile()}),
        )
        .await
        .unwrap_err();
    assert_eq!(unknown.code(), Code::Conflict);

    let quote = h
        .service
        .quote("s1", "travel-cn", 1, travel_params())
        .await
        .unwrap();
    assert_eq!(quote.amount_minor, 2_000);

    let order = h
        .service
        .create(
            "s1",
            CreateOrderRequest {
                quote_id: quote.quote_id.clone(),
                idempotency_key: "res-1".to_owned(),
                confirmed_amount_minor: 2_000,
                params: travel_params(),
            },
        )
        .await
        .unwrap();
    assert_eq!(order.state, OrderState::Paid);
    assert_eq!(h.vendor.state.lock().unwrap().confirm_calls, 1);
    assert_eq!(h.payment.intent_count(), 1);

    let booking_id = order.provider_order_id.clone();
    let view = h.provider.view("u1", &booking_id).unwrap();
    assert_eq!(view.booking.status, BookingStatus::Confirmed);
    assert_eq!(view.stages.len(), 2);
    assert!(view.stages.iter().all(|stage| stage.done));

    let fulfilled = h.service.fulfill("s1", &order.order_id).await.unwrap();
    assert_eq!(fulfilled.state, OrderState::Fulfilled);
    assert_eq!(fulfilled.receipt.as_ref().unwrap().status, "confirmed");
}
