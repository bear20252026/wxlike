//! WP-06 A2 end to end: bob is completely offline, alice sends him a message,
//! the hub fires the offline hook, the dispatcher notifies every registered
//! device through the provider — and the notification payload contains who
//! sent it, never the message body.

use std::sync::{Arc, Mutex};

use wxlike_core::{Envelope, MsgBody, MsgKind, PROTOCOL_VERSION};
use wxlike_net::MqHubOptions;
use wxlike_push::{DeviceDirectory, DispatchHook, PushPayload, PushProvider};

/// Records every (token, payload) the provider is asked to send.
#[derive(Default)]
struct RecordingProvider {
    sent: Mutex<Vec<(String, PushPayload)>>,
}

#[async_trait::async_trait]
impl PushProvider for RecordingProvider {
    async fn send(&self, device_token: &str, payload: &PushPayload) -> anyhow::Result<()> {
        self.sent
            .lock()
            .unwrap()
            .push((device_token.to_string(), payload.clone()));
        Ok(())
    }
}

/// The platform's devices table behind the directory trait.
struct PlatformDirectory(Arc<wxlike_platform::Platform>);

#[async_trait::async_trait]
impl DeviceDirectory for PlatformDirectory {
    async fn devices_of(&self, user_id: &str) -> anyhow::Result<Vec<(String, String)>> {
        Ok(self
            .0
            .devices_of(user_id)?
            .into_iter()
            .map(|d| (d.platform, d.device_token))
            .collect())
    }
}

fn text(from: &str, to: &str, body: &str) -> Envelope {
    Envelope {
        version: PROTOCOL_VERSION,
        conv_id: "d-a~b".into(),
        sender: from.into(),
        to: vec![to.into()],
        seq: 1,
        kind: MsgKind::Text,
        body: MsgBody::Text(body.into()),
        sent_at_ms: 1,
        msg_id: [7; 16],
        quote_of: None,
        receipt: None,
    }
}

#[tokio::test]
async fn offline_recipient_gets_pushed_and_online_recipient_does_not() {
    let platform = wxlike_platform::Platform::in_memory().unwrap();
    platform.register("alice", "pw").unwrap();
    platform.register("bob", "pw").unwrap();
    let bob_id = platform.user_id_by_name("bob").unwrap().unwrap();

    // bob has a phone and a tablet registered; the phone re-registers with a
    // newer token first (latest per platform wins)
    platform
        .register_device(
            &bob_id,
            &wxlike_platform::DeviceReg {
                platform: "android".into(),
                device_token: "old-phone-token".into(),
            },
        )
        .unwrap();
    platform
        .register_device(
            &bob_id,
            &wxlike_platform::DeviceReg {
                platform: "android".into(),
                device_token: "new-phone-token".into(),
            },
        )
        .unwrap();
    platform
        .register_device(
            &bob_id,
            &wxlike_platform::DeviceReg {
                platform: "ios".into(),
                device_token: "tablet-token".into(),
            },
        )
        .unwrap();
    assert_eq!(platform.devices_of(&bob_id).unwrap().len(), 2);

    let provider = Arc::new(RecordingProvider::default());
    let hook = DispatchHook::new(
        Arc::new(PlatformDirectory(platform.shared())),
        provider.clone(),
    );

    let router = wxlike_mq::MqRouter::new(
        Arc::new(wxlike_mq::memory::MemoryMq::new()),
        wxlike_mq::memory::MemoryRoutes::new(),
    );
    let hub = wxlike_net::run_mq_hub_with(
        "push-hub",
        "127.0.0.1:0".parse().unwrap(),
        router,
        MqHubOptions {
            // 测试用 StaticAuth：token 原样当 user_id（生产走 session 表）。
            // 不注入的话 hub 会 fail closed 拒绝连接——这是 issue #3 的默认值。
            auth: Some(std::sync::Arc::new(wxlike_net::auth::StaticAuth)),
            push_hook: Some(hook),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // A2: bob completely offline -> alice sends -> provider fires once per
    // registered device, payload has no message body
    hub.handle()
        .publish(&text("alice", &bob_id, "meet me at the secret place"))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let sent = provider.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2, "one push per registered device: {sent:?}");
    let tokens: Vec<&str> = sent.iter().map(|(t, _)| t.as_str()).collect();
    assert!(tokens.contains(&"new-phone-token"));
    assert!(
        !tokens.contains(&"old-phone-token"),
        "the replaced token must not be notified"
    );
    assert!(tokens.contains(&"tablet-token"));
    for (token, payload) in &sent {
        assert_eq!(
            payload.from_user, "alice",
            "payload names the sender ({token})"
        );
        assert_eq!(payload.conv_id, "d-a~b");
        let json = serde_json::to_string(payload).unwrap();
        assert!(
            !json.contains("secret place"),
            "push payload must never carry the message body: {json}"
        );
    }

    // now bob is online: the same message must NOT trigger another push
    let bob_client =
        wxlike_net::Client::connect(hub.local_addr().unwrap(), "localhost", bob_id.clone())
            .await
            .unwrap();
    // the client resolves when Hello is *written*; wait until the hub has
    // actually bound bob before asserting anything about online behaviour
    for _ in 0..100 {
        if hub.handle().online_users().await == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        hub.handle().online_users().await,
        1,
        "bob must be bound before the online-phase assertion"
    );
    let before = provider.sent.lock().unwrap().len();
    hub.handle()
        .publish(&text("alice", &bob_id, "bob is online now"))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        provider.sent.lock().unwrap().len(),
        before,
        "online recipients are delivered over QUIC, not pushed"
    );
    // bob receives the parked message from while he was away, then the live one
    let drained = bob_client.next_envelope().await.unwrap();
    assert_eq!(
        drained.body,
        MsgBody::Text("meet me at the secret place".into())
    );
    let live = bob_client.next_envelope().await.unwrap();
    assert_eq!(live.body, MsgBody::Text("bob is online now".into()));
    drop(hub);
}
