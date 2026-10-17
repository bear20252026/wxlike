//! WP-12 A2: trust-on-first-use for the QUIC transport.
//!
//! 首连记录指纹 → 二连伪造证书 → 拒绝。Each hub process gets a fresh
//! self-signed certificate, so a second hub under the same server name is
//! exactly the "forged certificate" scenario the task book asks for.

use wxlike_net::transport::{connect_tofu, make_server_endpoint, TofuPins};

fn pin_file(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("wxlike-tofu-{}-{}.json", std::process::id(), tag))
}

fn free_endpoint() -> (quinn::Endpoint, std::net::SocketAddr) {
    // port 0 = OS-assigned, so every hub here is a distinct listener
    let ep = make_server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = ep.local_addr().unwrap();
    // an endpoint only completes handshakes when someone accepts
    let accept_ep = ep.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Some(incoming) = accept_ep.accept().await {
            if let Ok(conn) = incoming.await {
                held.push(conn);
            }
        }
    });
    (ep, addr)
}

#[tokio::test]
async fn first_connection_pins_and_second_connection_matches() {
    let pins_path = pin_file("reconnect");
    let _ = std::fs::remove_file(&pins_path);
    let (_hub, addr) = free_endpoint();

    let conn = connect_tofu(addr, "localhost", &pins_path).await.unwrap();
    conn.close(0u32.into(), b"done");
    drop(conn);

    let store = TofuPins::load(&pins_path);
    assert_eq!(store.len(), 1, "exactly one pin was recorded");
    let text = std::fs::read_to_string(&pins_path).unwrap();
    assert!(
        text.contains("localhost"),
        "pin keyed by server name: {text}"
    );
    assert!(text.matches(char::is_numeric).count() > 10);

    // reconnect to the SAME hub: same certificate, must pass
    let again = connect_tofu(addr, "localhost", &pins_path).await;
    assert!(
        again.is_ok(),
        "same cert must still verify: {:?}",
        again.err()
    );
}

#[tokio::test]
async fn forged_certificate_on_second_connection_is_rejected() {
    let pins_path = pin_file("forged");
    let _ = std::fs::remove_file(&pins_path);

    // the real hub: first use pins its certificate
    let (_real_hub, real_addr) = free_endpoint();
    let conn = connect_tofu(real_addr, "localhost", &pins_path)
        .await
        .unwrap();
    conn.close(0u32.into(), b"done");
    drop(conn);
    assert_eq!(TofuPins::load(&pins_path).len(), 1);

    // the attacker: a different process with a different self-signed cert,
    // reached under the same server name
    let (_attacker_hub, attacker_addr) = free_endpoint();
    let err = connect_tofu(attacker_addr, "localhost", &pins_path)
        .await
        .expect_err("a different certificate under a pinned name must be refused");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("fingerprint changed") || msg.contains("handshake rejected"),
        "error should name the fingerprint mismatch: {msg}"
    );

    // and the pin store was not overwritten by the attack
    let store = TofuPins::load(&pins_path);
    assert_eq!(store.len(), 1);
}

#[tokio::test]
async fn different_names_pin_independently() {
    let pins_path = pin_file("names");
    let _ = std::fs::remove_file(&pins_path);
    let (_hub_a, addr_a) = free_endpoint();
    let (_hub_b, addr_b) = free_endpoint();

    let a = connect_tofu(addr_a, "hub-a.local", &pins_path)
        .await
        .unwrap();
    a.close(0u32.into(), b"done");
    let b = connect_tofu(addr_b, "hub-b.local", &pins_path)
        .await
        .unwrap();
    b.close(0u32.into(), b"done");

    // two names, two pins; swapping servers under the wrong name is refused
    assert_eq!(TofuPins::load(&pins_path).len(), 2);
    let err = connect_tofu(addr_b, "hub-a.local", &pins_path).await;
    assert!(err.is_err(), "hub-b's cert is not what hub-a.local pinned");
}

#[test]
fn pin_store_round_trips_and_survives_reload() {
    let pins_path = pin_file("store");
    let _ = std::fs::remove_file(&pins_path);

    let mut store = TofuPins::load(&pins_path);
    assert!(store.is_empty());
    assert!(
        store.check_or_pin("localhost", "aa11").unwrap(),
        "fresh pin"
    );
    assert!(
        !store.check_or_pin("localhost", "aa11").unwrap(),
        "same fingerprint matches"
    );
    assert!(store.check_or_pin("localhost", "bb22").is_err());

    // a new process reloads the same file and enforces the same pin
    let reloaded = TofuPins::load(&pins_path);
    let mut reloaded = reloaded;
    assert!(!reloaded.check_or_pin("localhost", "aa11").unwrap());
    assert!(reloaded.check_or_pin("localhost", "bb22").is_err());
}

/// The verified client constructor goes through TOFU end to end (Hello and
/// all), using the same pin store a desktop app would keep.
#[tokio::test]
async fn client_connect_verified_uses_tofu() {
    use wxlike_net::Client;

    let pins_path = pin_file("client");
    let _ = std::fs::remove_file(&pins_path);
    let (_hub, addr) = free_endpoint();

    let client = Client::connect_verified(addr, "localhost", "alice", &pins_path)
        .await
        .unwrap();
    assert_eq!(client.user_id, "alice");
    assert_eq!(TofuPins::load(&pins_path).len(), 1);

    // a different hub under the pinned name is refused at the transport layer
    let (_other, other_addr) = free_endpoint();
    assert!(
        Client::connect_verified(other_addr, "localhost", "alice", &pins_path)
            .await
            .is_err()
    );
}
