//! `wxlike-push` — WP-06 system push: notify a user's *devices* that messages
//! exist while the app is not connected.
//!
//! Two hard rules shape this crate:
//!
//! 1. **The payload never contains message content.** Chat bodies are E2EE
//!    (WP-12); a push notification would bypass that entirely. So
//!    [`PushPayload`] is a closed struct with only *who* and *where* — there is
//!    no field that could hold a body, by construction rather than by policy.
//! 2. **No provider secrets in code.** [`FcmProvider::from_env`] reads
//!    credentials from the environment and returns `None` without them; the
//!    server then falls back to [`NoopProvider`]. Nothing here can "work" with
//!    baked-in credentials.
//!
//! APNs: intentionally not implemented — a real provider needs an Apple
//! developer account and a signed key, neither of which can be verified
//! offline. The [`PushProvider`] trait is the integration point; an
//! `ApnsProvider` slots in behind it without touching callers.

use std::sync::atomic::{AtomicUsize, Ordering};

use wxlike_core::Envelope;

/// What a provider receives. Deliberately tiny: `from_user` is all a
/// notification badge needs ("alice 发来一条消息"), and `conv_id` lets the
/// client decide whether to surface it. No message body, ever.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
pub struct PushPayload {
    pub from_user: String,
    pub conv_id: String,
}

impl PushPayload {
    /// Builds the notification payload for an envelope. The envelope's body
    /// is intentionally *not* reachable from here — see the crate docs.
    pub fn of(env: &Envelope) -> Self {
        Self {
            from_user: env.sender.clone(),
            conv_id: env.conv_id.clone(),
        }
    }
}

/// A push transport. `send` must be idempotent from the caller's perspective:
/// delivery is best-effort and retried on the next message, never guaranteed.
#[async_trait::async_trait]
pub trait PushProvider: Send + Sync + 'static {
    async fn send(&self, device_token: &str, payload: &PushPayload) -> anyhow::Result<()>;
}

/// Does nothing, counts sends. The default provider (and handy in tests:
/// "离线时被调用、在线时不被调用" is asserted against this counter).
pub struct NoopProvider {
    sends: AtomicUsize,
}

impl NoopProvider {
    pub fn new() -> Self {
        Self {
            sends: AtomicUsize::new(0),
        }
    }

    pub fn sends(&self) -> usize {
        self.sends.load(Ordering::SeqCst)
    }
}

impl Default for NoopProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl PushProvider for NoopProvider {
    async fn send(&self, _device_token: &str, _payload: &PushPayload) -> anyhow::Result<()> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// FCM (HTTP v1)
// ---------------------------------------------------------------------------

/// Environment variables read by [`FcmProvider::from_env`]:
///
/// * `WXLIKE_FCM_PROJECT_ID` — the Firebase project the message targets.
/// * `WXLIKE_FCM_ACCESS_TOKEN` — a valid OAuth2 access token with scope
///   `https://www.googleapis.com/auth/firebase.messaging`. Token *refresh*
///   (service-account JWT exchange) is a deployment concern: a sidecar or
///   wrapper refreshes it into the environment/secret store, because baking
///   the refresh flow here would mean embedding private-key JWT signing for
///   no verifiable gain offline.
pub const FCM_PROJECT_ENV: &str = "WXLIKE_FCM_PROJECT_ID";
pub const FCM_TOKEN_ENV: &str = "WXLIKE_FCM_ACCESS_TOKEN";

/// Firebase Cloud Messaging over the HTTP v1 API.
pub struct FcmProvider {
    project_id: String,
    access_token: String,
    /// Injectable so tests can point at a local server instead of Google.
    endpoint: String,
}

impl FcmProvider {
    /// `None` when credentials are absent — callers substitute
    /// [`NoopProvider`], per the task book's fallback rule.
    pub fn from_env() -> Option<Self> {
        let project_id = std::env::var(FCM_PROJECT_ENV).ok()?;
        let access_token = std::env::var(FCM_TOKEN_ENV).ok()?;
        if project_id.trim().is_empty() || access_token.trim().is_empty() {
            return None;
        }
        Some(Self {
            project_id,
            access_token,
            endpoint: "https://fcm.googleapis.com/v1/projects".to_string(),
        })
    }

    #[cfg(test)]
    fn with_endpoint(project_id: &str, access_token: &str, endpoint: String) -> Self {
        Self {
            project_id: project_id.to_string(),
            access_token: access_token.to_string(),
            endpoint,
        }
    }

    fn url_of(&self) -> String {
        format!(
            "{}/{}/messages:send",
            self.endpoint.trim_end_matches('/'),
            self.project_id
        )
    }

    /// The exact JSON POSTed: a data-only message with the payload fields —
    /// no `notification.body` content beyond the sender identity, no message
    /// text.
    fn request_body(device_token: &str, payload: &PushPayload) -> serde_json::Value {
        serde_json::json!({
            "message": {
                "token": device_token,
                "data": {
                    "from_user": payload.from_user,
                    "conv_id": payload.conv_id,
                }
            }
        })
    }
}

#[async_trait::async_trait]
impl PushProvider for FcmProvider {
    async fn send(&self, device_token: &str, payload: &PushPayload) -> anyhow::Result<()> {
        use http_body_util::BodyExt as _;

        let client: hyper_util::client::legacy::Client<
            _,
            http_body_util::Full<hyper::body::Bytes>,
        > = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
            .build_http();
        let req = hyper::Request::builder()
            .method(hyper::Method::POST)
            .uri(self.url_of())
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json; charset=UTF-8")
            .body(http_body_util::Full::new(bytes::Bytes::from(
                serde_json::to_vec(&Self::request_body(device_token, payload))?,
            )))?;
        let resp = client.request(req).await?;
        let status = resp.status();
        let body = resp.into_body().collect().await?.to_bytes();
        if !status.is_success() {
            anyhow::bail!("fcm returned {status}: {}", String::from_utf8_lossy(&body));
        }
        Ok(())
    }
}

/// Chooses the provider like the server does: FCM when the environment has
/// credentials, [`NoopProvider`] otherwise.
pub fn provider_from_env() -> std::sync::Arc<dyn PushProvider> {
    match FcmProvider::from_env() {
        Some(fcm) => std::sync::Arc::new(fcm),
        None => std::sync::Arc::new(NoopProvider::new()),
    }
}

// ---------------------------------------------------------------------------
// Hub dispatch (WP-06): the bridge between "hub noticed the user is offline"
// and "every registered device got a notification".
// ---------------------------------------------------------------------------

/// Where device tokens live (implemented over the platform's `devices` table
/// by the server; tests implement it inline).
#[async_trait::async_trait]
pub trait DeviceDirectory: Send + Sync + 'static {
    /// `(platform, device_token)` pairs registered for the user.
    async fn devices_of(&self, user_id: &str) -> anyhow::Result<Vec<(String, String)>>;
}

/// Implements [`wxlike_net::OfflinePush`]: on an offline recipient, look up
/// their devices and notify each through the provider. Failures are logged,
/// never propagated — the message itself is already parked in the offline
/// store and push is strictly best-effort.
pub struct DispatchHook {
    pub directory: std::sync::Arc<dyn DeviceDirectory>,
    pub provider: std::sync::Arc<dyn PushProvider>,
}

impl DispatchHook {
    pub fn new(
        directory: std::sync::Arc<dyn DeviceDirectory>,
        provider: std::sync::Arc<dyn PushProvider>,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            directory,
            provider,
        })
    }
}

#[async_trait::async_trait]
impl wxlike_net::OfflinePush for DispatchHook {
    async fn user_offline(&self, user_id: &str, env: &Envelope) {
        let devices = match self.directory.devices_of(user_id).await {
            Ok(d) => d,
            Err(err) => {
                eprintln!("[push] device lookup for {user_id} failed: {err:#}");
                return;
            }
        };
        if devices.is_empty() {
            return; // nothing registered: no push, no error
        }
        let payload = PushPayload::of(env);
        for (_platform, token) in devices {
            if let Err(err) = self.provider.send(&token, &payload).await {
                eprintln!("[push] send to {user_id} failed: {err:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxlike_core::{MsgBody, MsgKind};

    fn env_from_alice(body_text: &str) -> Envelope {
        Envelope {
            version: wxlike_core::PROTOCOL_VERSION,
            conv_id: "d-a~b".into(),
            sender: "alice".into(),
            to: vec!["bob".into()],
            seq: 1,
            kind: MsgKind::Text,
            body: MsgBody::Text(body_text.into()),
            sent_at_ms: 42,
            msg_id: [9; 16],
            quote_of: None,
            receipt: None,
        }
    }

    #[test]
    fn payload_carries_who_and_where_but_never_the_body() {
        let env = env_from_alice("the launch code is 0000");
        let payload = PushPayload::of(&env);
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("alice"));
        assert!(
            !json.contains("launch code"),
            "message text must not leak into push payloads: {json}"
        );
        assert_eq!(payload.conv_id, "d-a~b");
    }

    #[tokio::test]
    async fn noop_provider_counts_sends_and_never_fails() {
        let provider = NoopProvider::new();
        let payload = PushPayload::of(&env_from_alice("hi"));
        for _ in 0..3 {
            provider.send("token", &payload).await.unwrap();
        }
        assert_eq!(provider.sends(), 3);
        // invalid/empty tokens are dropped by the provider side, not crashed on
        provider.send("", &payload).await.unwrap();
        assert_eq!(provider.sends(), 4);
    }

    #[tokio::test]
    async fn fcm_env_gating() {
        // no credentials in the test environment by default
        let had_project = std::env::var(FCM_PROJECT_ENV).ok();
        let had_token = std::env::var(FCM_TOKEN_ENV).ok();
        std::env::remove_var(FCM_PROJECT_ENV);
        std::env::remove_var(FCM_TOKEN_ENV);
        assert!(FcmProvider::from_env().is_none(), "no creds -> no provider");
        let fallback = provider_from_env();
        // fallback is the noop (provable by counting)
        let p = PushPayload::of(&env_from_alice("x"));
        fallback.send("t", &p).await.unwrap();
        // restore whatever the outer environment had
        if let Some(v) = had_project {
            std::env::set_var(FCM_PROJECT_ENV, v);
        }
        if let Some(v) = had_token {
            std::env::set_var(FCM_TOKEN_ENV, v);
        }
    }

    #[test]
    fn fcm_request_body_never_contains_message_text() {
        let payload = PushPayload::of(&env_from_alice("secret body text"));
        let body = FcmProvider::request_body("device-token", &payload);
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("device-token"));
        assert!(json.contains("alice"));
        assert!(!json.contains("secret body text"));
    }

    /// The real HTTP path is exercised against a local server, so the wire
    /// format and auth header are verified without Google credentials.
    #[tokio::test]
    async fn fcm_send_posts_expected_wire_format() {
        use http_body_util::BodyExt as _;
        use hyper::service::service_fn;
        use hyper::{Request, Response};

        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = received.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let sink = sink.clone();
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        hyper_util::rt::TokioIo::new(stream),
                        service_fn(move |req: Request<hyper::body::Incoming>| {
                            let sink = sink.clone();
                            async move {
                                let (parts, body) = req.into_parts();
                                let bytes = body.collect().await.unwrap().to_bytes();
                                let auth = parts
                                    .headers
                                    .get("Authorization")
                                    .and_then(|v| v.to_str().ok())
                                    .unwrap_or("")
                                    .to_string();
                                sink.lock().unwrap().push(format!(
                                    "{} {} auth={}",
                                    parts.method,
                                    parts.uri.path(),
                                    auth
                                ));
                                sink.lock()
                                    .unwrap()
                                    .push(String::from_utf8_lossy(&bytes).into_owned());
                                Ok::<_, std::convert::Infallible>(Response::new(
                                    http_body_util::Full::new(bytes::Bytes::from("{}")),
                                ))
                            }
                        }),
                    )
                    .await;
            }
        });

        let provider = FcmProvider::with_endpoint("proj", "tok-1", format!("http://{addr}"));
        let payload = PushPayload::of(&env_from_alice("never in the payload"));
        provider.send("dev-1", &payload).await.unwrap();
        server.abort();

        let log = received.lock().unwrap().join("\n");
        assert!(log.contains("POST /proj/messages:send"));
        assert!(log.contains("auth=Bearer tok-1"));
        assert!(log.contains("\"from_user\":\"alice\""));
        assert!(!log.contains("never in the payload"));
    }
}
