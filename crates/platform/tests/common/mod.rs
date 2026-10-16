//! Shared helpers for platform integration tests: a real axum router driven by
//! `tower::oneshot`, plus an `EventSink` recorder that captures the system
//! envelopes the platform pushes through the hub abstraction.
//!
//! Each `tests/*.rs` file compiles its own copy of this module and uses a
//! different subset of it, so unused helpers are expected, not dead code.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt as _;
use wxlike_core::Envelope;
use wxlike_platform::{Platform, PlatformOptions, PlatformState};

pub fn platform() -> PlatformState {
    let opts = PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        ..Default::default()
    };
    Platform::in_memory_with(opts).unwrap().shared()
}

pub fn app(state: PlatformState) -> Router {
    Platform::router(state)
}

pub fn req(method: &str, path: &str, body: Option<Value>, token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b.body(match body {
        Some(v) => Body::from(v.to_string()),
        None => Body::empty(),
    })
    .unwrap()
}

pub async fn call(app: &Router, r: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(r).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, body)
}

/// Registers a user and returns `(user_id, token)`.
pub async fn signup(app: &Router, name: &str) -> (String, String) {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/register",
            Some(json!({"name": name, "password": "pw"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup {name}: {body}");
    (
        body["user_id"].as_str().unwrap().to_string(),
        body["token"].as_str().unwrap().to_string(),
    )
}

/// Captures system envelopes instead of sending them over QUIC.
#[derive(Clone, Default)]
pub struct Recorder {
    events: Arc<Mutex<Vec<Envelope>>>,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn take(&self) -> Vec<Envelope> {
        let mut v = self.events.lock().unwrap();
        std::mem::take(&mut *v)
    }

    pub fn count(&self) -> usize {
        self.events.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl wxlike_platform::EventSink for Recorder {
    async fn publish(&self, env: Envelope) -> anyhow::Result<()> {
        self.events.lock().unwrap().push(env);
        Ok(())
    }
}
