mod common;

use std::sync::Arc;

use serde_json::json;
use wxlike_service_catalog::auth::BearerAuthenticator;
use wxlike_service_catalog::providers::games::{
    ExternalLink, GamePackage, GameService, GameSignatureVerifier,
};
use wxlike_service_catalog::{
    Catalog, CreateOrderRequest, OrderService, OrderState, ProviderOrderState, SandboxEvidence,
    ServiceErrorCode as Code, ServiceProvider, ServiceType,
};

use common::{card, evidence, LedgerPayment, TestClock};

struct TestVerifier;

impl GameSignatureVerifier for TestVerifier {
    fn verify(&self, package: &GamePackage) -> bool {
        package.signature == "valid-signature" && package.publisher_key_id == "pub-1"
    }
}

fn package(package_id: &str, regions: &[&str]) -> GamePackage {
    GamePackage {
        package_id: package_id.to_owned(),
        title: "拼图大师".to_owned(),
        version: "1.2.0".to_owned(),
        price_minor: 900,
        currency: "CNY".to_owned(),
        regions: regions.iter().map(|region| (*region).to_owned()).collect(),
        program_sha256: "ab".repeat(32),
        signature: "valid-signature".to_owned(),
        publisher_key_id: "pub-1".to_owned(),
    }
}

fn service(evidence: Option<SandboxEvidence>) -> Arc<GameService> {
    Arc::new(GameService::new(
        Arc::new(TestVerifier),
        Arc::new(TestClock::new(1_000)),
        evidence,
    ))
}

#[tokio::test]
async fn publish_requires_trusted_signature_and_lists_by_region() {
    let games = service(evidence());
    games.publish(package("game-1", &["CN"])).unwrap();
    games.publish(package("game-2", &["US"])).unwrap();

    let cn = games.list("CN");
    assert_eq!(cn.len(), 1);
    assert_eq!(cn[0].package_id, "game-1");
    assert!(games.list("JP").is_empty());

    let mut unsigned = package("game-3", &["CN"]);
    unsigned.signature = "forged".to_owned();
    assert_eq!(
        games.publish(unsigned).unwrap_err().code(),
        Code::Unauthorized
    );

    let mut malformed = package("game-4", &["CN"]);
    malformed.title.clear();
    assert_eq!(
        games.publish(malformed).unwrap_err().code(),
        Code::InvalidArgument
    );
    assert!(games
        .list("CN")
        .iter()
        .all(|item| item.package_id != "game-3"));
}

#[tokio::test]
async fn launch_is_region_and_actor_gated() {
    let games = service(evidence());
    games.publish(package("game-1", &["CN"])).unwrap();

    let ticket = games.launch("u1", "game-1", "CN").unwrap();
    assert_eq!(ticket.package_id, "game-1");
    assert_eq!(ticket.save_slots, 8);
    assert_eq!(ticket.expires_at_ms - ticket.issued_at_ms, 10 * 60 * 1000);

    assert_eq!(
        games.launch("u1", "game-1", "US").unwrap_err().code(),
        Code::BlockedExternal
    );
    assert_eq!(
        games.launch("", "game-1", "CN").unwrap_err().code(),
        Code::Unauthorized
    );
    assert_eq!(
        games.launch("u1", "game-404", "CN").unwrap_err().code(),
        Code::NotFound
    );
}

#[tokio::test]
async fn saves_are_isolated_per_actor_package_and_slot() {
    let games = service(evidence());
    games.publish(package("game-1", &["CN"])).unwrap();
    games.publish(package("game-2", &["CN"])).unwrap();

    games
        .save("u1", "game-1", 0, b"slot-zero".to_vec())
        .unwrap();
    games
        .save("u2", "game-1", 0, b"other-user".to_vec())
        .unwrap();
    assert_eq!(
        games.load("u1", "game-1", 0).unwrap(),
        b"slot-zero".to_vec()
    );
    assert_eq!(
        games.load("u2", "game-1", 0).unwrap(),
        b"other-user".to_vec()
    );
    assert_eq!(
        games.load("u1", "game-2", 0).unwrap_err().code(),
        Code::NotFound
    );
    assert_eq!(
        games
            .save("u1", "game-1", 8, b"x".to_vec())
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    assert_eq!(
        games
            .save("u1", "game-1", 1, Vec::new())
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    assert!(!games.delete_save("u2", "game-1", 1));
    assert!(games.delete_save("u1", "game-1", 0));
    assert_eq!(
        games.load("u1", "game-1", 0).unwrap_err().code(),
        Code::NotFound
    );
    assert_eq!(
        games
            .save("", "game-1", 0, b"x".to_vec())
            .unwrap_err()
            .code(),
        Code::Unauthorized
    );
}

#[tokio::test]
async fn external_links_are_https_and_strip_tokens() {
    let games = service(evidence());
    let link: ExternalLink = games
        .external_link(
            "https://shop.example.com/checkout?token=abc&item=7&access_token=xyz#frag",
            vec!["profile.nickname".to_owned()],
        )
        .unwrap();
    assert_eq!(link.url, "https://shop.example.com/checkout?item=7");
    assert_eq!(link.domain, "shop.example.com");
    assert_eq!(link.data_scopes, vec!["profile.nickname".to_owned()]);

    assert_eq!(
        games
            .external_link("http://shop.example.com/x", vec![])
            .unwrap_err()
            .code(),
        Code::BlockedExternal
    );
    assert_eq!(
        games
            .external_link("https://user:pass@shop.example.com/x", vec![])
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    assert_eq!(
        games
            .external_link("https://shop.example.com/a\u{0007}b", vec![])
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    assert_eq!(
        games
            .external_link("https://shop.example.com/x?session=s", vec![])
            .unwrap()
            .url,
        "https://shop.example.com/x"
    );
}

struct Harness {
    order: OrderService,
    games: Arc<GameService>,
    payment: Arc<LedgerPayment>,
}

fn harness() -> Harness {
    let games = service(evidence());
    games.publish(package("game-1", &["CN"])).unwrap();
    let clock = Arc::new(TestClock::new(1_000));
    let catalog = Arc::new(Catalog::new());
    catalog
        .upsert(card("game-cn", ServiceType::Game, "games", "CN"))
        .unwrap();
    catalog
        .set_region_enabled(ServiceType::Game, "CN", true)
        .unwrap();
    let payment = Arc::new(LedgerPayment::default());
    let auth = Arc::new(BearerAuthenticator::new(|token: &str| match token {
        "s1" => Some("u1".to_owned()),
        _ => None,
    }));
    let order = OrderService::new(catalog, vec![games.clone()], payment.clone(), auth, clock);
    Harness {
        order,
        games,
        payment,
    }
}

#[tokio::test]
async fn game_purchase_grants_entitlement_and_refund_revokes_it() {
    let h = harness();
    let params = json!({"package_id": "game-1", "region": "CN"});
    let quote = h
        .order
        .quote("s1", "game-cn", 1, params.clone())
        .await
        .unwrap();
    assert_eq!(quote.amount_minor, 900);

    let order = h
        .order
        .create(
            "s1",
            CreateOrderRequest {
                quote_id: quote.quote_id,
                idempotency_key: "game-1".to_owned(),
                confirmed_amount_minor: 900,
                params: params.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(order.state, OrderState::Fulfilled);
    assert_eq!(order.receipt.as_ref().unwrap().status, "entitled");
    assert_eq!(h.payment.intent_count(), 1);

    let entitlement = h.games.query(&order.provider_order_id).await.unwrap();
    assert_eq!(entitlement.state, ProviderOrderState::Fulfilled);

    let cancelled = h.order.cancel("s1", &order.order_id, "too late").await;
    assert_eq!(cancelled.unwrap_err().code(), Code::Conflict);

    let refunded = h
        .order
        .refund("s1", &order.order_id, "publisher withdrew")
        .await
        .unwrap();
    assert_eq!(refunded.state, OrderState::Refunded);
    assert_eq!(h.payment.refunded_minor(), 900);
    let revoked = h.games.query(&order.provider_order_id).await.unwrap();
    assert_eq!(revoked.state, ProviderOrderState::Cancelled);
}

#[tokio::test]
async fn game_quote_blocks_region_mismatch_and_missing_evidence() {
    let h = harness();
    let mismatch = h
        .order
        .quote(
            "s1",
            "game-cn",
            1,
            json!({"package_id": "game-1", "region": "US"}),
        )
        .await
        .unwrap_err();
    assert_eq!(mismatch.code(), Code::Conflict);

    let blocked = service(None);
    blocked.publish(package("game-1", &["CN"])).unwrap();
    let catalog = Arc::new(Catalog::new());
    catalog
        .upsert(card("game-cn", ServiceType::Game, "games", "CN"))
        .unwrap();
    catalog
        .set_region_enabled(ServiceType::Game, "CN", true)
        .unwrap();
    let order = OrderService::new(
        catalog,
        vec![blocked],
        Arc::new(LedgerPayment::default()),
        Arc::new(BearerAuthenticator::new(|token: &str| match token {
            "s1" => Some("u1".to_owned()),
            _ => None,
        })),
        Arc::new(TestClock::new(1_000)),
    );
    let err = order
        .quote(
            "s1",
            "game-cn",
            1,
            json!({"package_id": "game-1", "region": "CN"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::BlockedExternal);
}
