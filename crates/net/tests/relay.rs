//! Integration test (legacy broadcast hub): two clients exchange envelopes.

use wxlike_core::Envelope;
use wxlike_net::{run_hub, Client};

#[tokio::test]
async fn two_clients_relay_bidirectional() {
    let hub = run_hub("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = hub.local_addr().unwrap();

    let alice = Client::connect_anonymous(addr, "localhost").await.unwrap();
    let bob = Client::connect_anonymous(addr, "localhost").await.unwrap();
    // hub registers each peer's stream asynchronously after accept_bi; give it a beat
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    alice
        .send(&Envelope::text("conv1", "alice", 1, "hi bob"))
        .await
        .unwrap();
    bob.send(&Envelope::text("conv1", "bob", 1, "hi alice"))
        .await
        .unwrap();

    let to_bob = tokio::time::timeout(std::time::Duration::from_secs(5), bob.next_envelope())
        .await
        .expect("a->b within timeout")
        .unwrap();
    assert_eq!(to_bob.sender, "alice");
    assert!(matches!(to_bob.body, wxlike_core::MsgBody::Text(ref t) if t == "hi bob"));

    let to_alice = tokio::time::timeout(std::time::Duration::from_secs(5), alice.next_envelope())
        .await
        .expect("b->a within timeout")
        .unwrap();
    assert_eq!(to_alice.sender, "bob");
    assert!(matches!(to_alice.body, wxlike_core::MsgBody::Text(ref t) if t == "hi alice"));
}
