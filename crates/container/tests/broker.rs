//! G08-03：权限 broker 的调用边界。
//!
//! 这些用例只走真实 broker + 真实端口替身（不 mock 被测逻辑），覆盖
//! 伪造 origin、会话失效、授权重放、用户拒绝/取消、支付/网络/定位的
//! 最小数据面。

use serde_json::json;
use wxlike_container::broker::calls::{FilePurpose, HttpMethod, TypedCall};
use wxlike_container::broker::grants::{GuestIdentity, Perm};
use wxlike_container::broker::ports::{fixtures, BrokerPorts};
use wxlike_container::broker::{AuditOutcome, Broker, BrokerError};
use wxlike_container::host::budget::{BudgetLedger, BudgetLimits};
use wxlike_container::host::profile::{DeviceTarget, IsolationProfile};

const APP: &str = "com.demo.notes";
const SESSION: &str = "sess-0001";

fn profile() -> IsolationProfile {
    IsolationProfile::new(DeviceTarget::Desktop, APP).unwrap()
}

fn setup(ports: BrokerPorts) -> (Broker, GuestIdentity) {
    let mut broker = Broker::with_ports(profile(), ports);
    broker.open_session(SESSION).unwrap();
    let guest = GuestIdentity {
        app_id: APP.into(),
        origin: broker.profile().origin.clone(),
        session_id: SESSION.into(),
    };
    (broker, guest)
}

fn ledger() -> BudgetLedger {
    BudgetLedger::new(BudgetLimits::desktop())
}

fn set(key: &str, value: &str) -> TypedCall {
    TypedCall::StorageSet {
        key: key.into(),
        value: value.into(),
    }
}

fn get(key: &str) -> TypedCall {
    TypedCall::StorageGet { key: key.into() }
}

#[test]
fn storage_roundtrip_is_budgeted_and_refunded() {
    let (mut broker, guest) = setup(BrokerPorts::default());
    broker.grant(&[Perm::Storage]);
    let mut budget = ledger();

    assert_eq!(
        broker
            .request(&mut budget, &guest, "r1", set("draft", "secret"))
            .unwrap(),
        json!(true)
    );
    assert_eq!(
        broker
            .request(&mut budget, &guest, "r2", get("draft"))
            .unwrap(),
        json!("secret")
    );
    assert_eq!(budget.usage().storage_bytes, 6);

    assert_eq!(
        broker
            .request(
                &mut budget,
                &guest,
                "r3",
                TypedCall::StorageRemove {
                    key: "draft".into()
                },
            )
            .unwrap(),
        json!(true)
    );
    assert_eq!(budget.usage().storage_bytes, 0);
    assert_eq!(
        broker
            .request(&mut budget, &guest, "r4", get("draft"))
            .unwrap(),
        json!(null)
    );
}

#[test]
fn forged_origin_and_unknown_app_are_refused() {
    let (mut broker, guest) = setup(BrokerPorts::default());
    broker.grant(&[Perm::Storage]);
    let mut budget = ledger();

    let mut forged = guest.clone();
    forged.origin = "https://evil.example".into();
    assert_eq!(
        broker
            .request(&mut budget, &forged, "r1", get("k"))
            .unwrap_err(),
        BrokerError::OriginMismatch
    );

    let mut stranger = guest.clone();
    stranger.app_id = "com.evil.app".into();
    assert_eq!(
        broker
            .request(&mut budget, &stranger, "r2", get("k"))
            .unwrap_err(),
        BrokerError::UnknownApp("com.evil.app".into())
    );
}

#[test]
fn navigating_away_closes_the_session() {
    let (mut broker, guest) = setup(BrokerPorts::default());
    broker.grant(&[Perm::Storage]);
    let mut budget = ledger();

    assert_eq!(
        broker.navigate("https://evil.example/").unwrap_err(),
        BrokerError::OriginMismatch
    );
    assert_eq!(
        broker
            .request(&mut budget, &guest, "r1", get("k"))
            .unwrap_err(),
        BrokerError::SessionClosed
    );
}

#[test]
fn request_ids_cannot_be_replayed() {
    let (mut broker, guest) = setup(BrokerPorts::default());
    broker.grant(&[Perm::Storage]);
    let mut budget = ledger();

    broker
        .request(&mut budget, &guest, "same-id", set("k", "v"))
        .unwrap();
    assert_eq!(
        broker
            .request(&mut budget, &guest, "same-id", set("k", "v"))
            .unwrap_err(),
        BrokerError::Replayed("same-id".into())
    );
}

#[test]
fn scope_must_be_granted_and_revocation_is_immediate() {
    let (mut broker, guest) = setup(BrokerPorts::default());
    let mut budget = ledger();

    assert_eq!(
        broker
            .request(&mut budget, &guest, "r1", get("k"))
            .unwrap_err(),
        BrokerError::NotGranted(Perm::Storage)
    );

    broker.grant(&[Perm::Storage]);
    broker.request(&mut budget, &guest, "r2", get("k")).unwrap();

    broker.revoke_scope(Perm::Storage);
    assert_eq!(
        broker
            .request(&mut budget, &guest, "r3", get("k"))
            .unwrap_err(),
        BrokerError::NotGranted(Perm::Storage)
    );
}

#[test]
fn user_denial_and_cancel_are_distinct() {
    let (mut denying, guest) = setup(BrokerPorts::default());
    denying.grant(&[Perm::Camera]);
    let mut budget = ledger();
    assert_eq!(
        denying
            .request(&mut budget, &guest, "r1", TypedCall::CameraCapture {})
            .unwrap_err(),
        BrokerError::Denied(Perm::Camera)
    );

    let mut ports = BrokerPorts {
        confirmer: Box::new(fixtures::CancelAll),
        ..BrokerPorts::default()
    };
    ports.devices = Box::new(fixtures::RecordingDevices::new().0);
    let (mut cancelling, guest) = setup(ports);
    cancelling.grant(&[Perm::Camera]);
    assert_eq!(
        cancelling
            .request(&mut budget, &guest, "r2", TypedCall::CameraCapture {})
            .unwrap_err(),
        BrokerError::Cancelled(Perm::Camera)
    );
}

#[test]
fn payment_returns_a_receipt_and_nothing_else() {
    let (payments, log) = fixtures::RecordingPayments::new();
    let ports = BrokerPorts {
        confirmer: Box::new(fixtures::AllowAll),
        payments: Box::new(payments),
        ..BrokerPorts::default()
    };
    let (mut broker, guest) = setup(ports);
    broker.grant(&[Perm::Payment]);
    let mut budget = ledger();

    let out = broker
        .request(
            &mut budget,
            &guest,
            "pay-1",
            TypedCall::PayRequest {
                amount: "12.50".into(),
                currency: "CNY".into(),
                order_id: "order-1".into(),
            },
        )
        .unwrap();
    let object = out.as_object().unwrap();
    assert_eq!(
        object.len(),
        2,
        "receipt has exactly reference + status: {out}"
    );
    assert_eq!(out["status"], json!("settled"));
    assert!(out.get("token").is_none(), "no session/order token leaks");

    let calls = log.entries();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].app_id, APP);
    assert_eq!(calls[0].amount, "12.50");

    let rejected = broker.request(
        &mut budget,
        &guest,
        "pay-2",
        TypedCall::PayRequest {
            amount: "12.345".into(),
            currency: "CNY".into(),
            order_id: "order-2".into(),
        },
    );
    assert!(matches!(rejected, Err(BrokerError::InvalidCall(_))));
    assert_eq!(log.entries().len(), 1, "invalid amount never reaches G06");
}

#[test]
fn network_is_https_allowlisted_and_budgeted() {
    let ports = BrokerPorts {
        confirmer: Box::new(fixtures::AllowAll),
        network: Box::new(fixtures::EchoNetwork),
        ..BrokerPorts::default()
    };
    let (mut broker, guest) = setup(ports);
    broker.grant(&[Perm::Network]);
    let mut budget = ledger();

    let evil = broker.request(
        &mut budget,
        &guest,
        "n1",
        TypedCall::NetworkRequest {
            url: "https://evil.example/steal".into(),
            method: HttpMethod::Get,
            body: None,
        },
    );
    assert!(matches!(evil, Err(BrokerError::InvalidCall(_))));

    let out = broker
        .request(
            &mut budget,
            &guest,
            "n2",
            TypedCall::NetworkRequest {
                url: "https://api.wxlike.app/v1/quotes".into(),
                method: HttpMethod::Get,
                body: None,
            },
        )
        .unwrap();
    assert_eq!(out["status"], json!(200));
    assert_eq!(out["body"]["host"], json!("api.wxlike.app"));

    let mut tiny = BudgetLedger::new(BudgetLimits {
        network_bytes: 1,
        ..BudgetLimits::desktop()
    });
    assert_eq!(
        broker
            .request(
                &mut tiny,
                &guest,
                "n3",
                TypedCall::NetworkRequest {
                    url: "https://api.wxlike.app/v1/quotes".into(),
                    method: HttpMethod::Get,
                    body: None,
                },
            )
            .unwrap_err(),
        BrokerError::Budget("network")
    );
}

#[test]
fn location_is_coarsened_and_files_have_no_path() {
    let (devices, log) = fixtures::RecordingDevices::new();
    let ports = BrokerPorts {
        confirmer: Box::new(fixtures::AllowAll),
        devices: Box::new(devices),
        ..BrokerPorts::default()
    };
    let (mut broker, guest) = setup(ports);
    broker.grant(&[Perm::Location, Perm::File]);
    let mut budget = ledger();

    let location = broker
        .request(&mut budget, &guest, "l1", TypedCall::LocationGet {})
        .unwrap();
    assert_eq!(location["latitude"], json!(31.23));
    assert_eq!(location["longitude"], json!(121.47));

    let file = broker
        .request(
            &mut budget,
            &guest,
            "f1",
            TypedCall::FilePick {
                purpose: FilePurpose::Attachment,
            },
        )
        .unwrap();
    assert_eq!(file["name"], json!("lease.pdf"));
    assert!(
        file.get("path").is_none(),
        "guest never sees the picker path"
    );
    assert_eq!(log.prompts(), vec![FilePurpose::Attachment]);
}

#[test]
fn audit_records_methods_not_payloads() {
    let (mut broker, guest) = setup(BrokerPorts::default());
    broker.grant(&[Perm::Storage]);
    let mut budget = ledger();

    let secret = "super-secret-token-value";
    broker
        .request(&mut budget, &guest, "a1", set("token", secret))
        .unwrap();
    let _ = broker.request(&mut budget, &guest, "a2", TypedCall::CameraCapture {});

    let entries = broker.audit();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].method, "storage.set");
    assert_eq!(entries[0].scope, Perm::Storage);
    assert_eq!(entries[0].outcome, AuditOutcome::Ok);
    assert_eq!(entries[1].outcome, AuditOutcome::Denied);
    assert!(
        !format!("{entries:?}").contains(secret),
        "audit lines must never carry the value"
    );
}
