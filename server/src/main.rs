//! wxlike dev server: platform HTTP API (axum) + MQ-routed QUIC hub.
//!
//! Backend selection (env):
//!   WXLIKE_HUB_ID      hub identity, default "hub-1"
//!   WXLIKE_MQ          "memory" (default) | "redis" (needs --features redis)
//!   WXLIKE_REDIS_URL   default redis://127.0.0.1:6379
//!   WXLIKE_DB          platform SQLite path, default "data/wxlike.db" (":memory:" allowed)
//!   WXLIKE_HTTP        HTTP bind address, default 0.0.0.0:8099
//!   WXLIKE_QUIC        QUIC bind address, default 0.0.0.0:9443
//!
//! Run two hubs against one Redis to see cross-node routing:
//!   WXLIKE_HUB_ID=hub-1 ./wxlike-server &
//!   WXLIKE_HUB_ID=hub-2 ./wxlike-server -- (different ports via WXLIKE_QUIC/WXLIKE_HTTP)

use std::sync::Arc;

use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // WXLIKE_LOG=e.g. "wxlike_server=debug,wxlike_net=debug,info" (RUST_LOG also honored)
    let filter = std::env::var("WXLIKE_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&filter).unwrap_or_else(|_| {
                eprintln!("[wxlike-server] bad log filter '{filter}', falling back to info");
                tracing_subscriber::EnvFilter::new("info")
            }),
        )
        .init();
    let hub_id = std::env::var("WXLIKE_HUB_ID").unwrap_or_else(|_| "hub-1".into());
    let http_addr = std::env::var("WXLIKE_HTTP").unwrap_or_else(|_| "0.0.0.0:8099".into());
    let quic_addr: std::net::SocketAddr = std::env::var("WXLIKE_QUIC")
        .unwrap_or_else(|_| "0.0.0.0:9443".into())
        .parse()?;
    let mq_mode = std::env::var("WXLIKE_MQ").unwrap_or_else(|_| "memory".into());
    let db_env = std::env::var("WXLIKE_DB").unwrap_or_else(|_| "data/wxlike.db".into());
    let db_path = if db_env == ":memory:" {
        None
    } else {
        Some(db_env.as_str())
    };

    let router = build_router(&mq_mode)?;
    println!("[wxlike-server] hub '{hub_id}' mq={mq_mode}");

    // Platform data plane is on by default (WP-01): accounts, sessions, profiles.
    // It is created first because the hub reads group membership from it (WP-04).
    let platform = wxlike_platform::Platform::open(db_path)?.shared();

    // The hub persists messages (msg_id dedup, recall, receipt routing — WP-03)
    // and parks frames for offline users on disk rather than in RAM (WP-10), so
    // a restart does not lose what was waiting for someone who was away.
    let store = Arc::new(wxlike_core::Store::open(db_path)?);
    let offline = wxlike_net::offline::SqliteOffline::open(db_path, 5_000)?;
    let hub = wxlike_net::run_mq_hub_with(
        hub_id.clone(),
        quic_addr,
        router,
        wxlike_net::MqHubOptions {
            storage: Some(store),
            offline,
            membership: Some(Arc::new(PlatformMembership(platform.clone()))),
            reclaim_idle_ms: Some(wxlike_net::DEFAULT_CLAIM_IDLE_MS),
            // WP-06: a recipient offline everywhere gets a device push whose
            // payload says only who wrote — never what.
            push_hook: Some(wxlike_push::DispatchHook::new(
                Arc::new(PlatformDevices(platform.clone())),
                wxlike_push::provider_from_env(),
            )),
            ..Default::default()
        },
    )
    .await?;
    let hub_addr = hub.local_addr()?;
    println!("[wxlike-server] QUIC hub on {hub_addr}");

    // Friend/group lifecycle events are pushed through the hub, so online peers
    // see them the same way they see chat traffic (WP-02).
    platform.set_event_sink(Arc::new(HubSink(hub.handle())));
    // Unclaimed red packets must come back after 24 h even if nobody asks (WP-09).
    let _sweeper = wxlike_platform::spawn_redpacket_sweeper(
        platform.clone(),
        std::time::Duration::from_secs(60),
    );
    println!("[wxlike-server] platform db = {}", db_env);
    println!("[wxlike-server] HTTP  on {http_addr}  (GET /health /endpoints /metrics, /api/*)");

    let presence_router = {
        let handle = hub.handle(); // clone before the hub is forgotten below
        axum::Router::new().route(
            "/api/presence/{user_id}",
            get(
                move |axum::extract::Path(user_id): axum::extract::Path<String>| {
                    let handle = handle.clone();
                    async move {
                        let online = handle.is_online(&user_id).await;
                        axum::Json(serde_json::json!({ "user_id": user_id, "online": online }))
                    }
                },
            ),
        )
    };
    std::mem::forget(hub); // run until process exit (the handle above was cloned)

    let app = axum::Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route(
            "/endpoints",
            get(move || async move {
                Json(serde_json::json!({
                    "hub": hub_id,
                    "quic": hub_addr.to_string(),
                    "alpn": "wxlike/1",
                }))
            }),
        )
        .merge(wxlike_platform::Platform::router(platform))
        .merge(presence_router);

    let listener = tokio::net::TcpListener::bind(&http_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn build_router(mq_mode: &str) -> anyhow::Result<wxlike_mq::MqRouter> {
    match mq_mode {
        "memory" => {
            let mq = Arc::new(wxlike_mq::memory::MemoryMq::new());
            let routes = wxlike_mq::memory::MemoryRoutes::new();
            Ok(wxlike_mq::MqRouter::new(mq, routes))
        }
        "redis" => {
            #[cfg(feature = "redis")]
            {
                let url = std::env::var("WXLIKE_REDIS_URL")
                    .unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
                // Streams, not lists: entries stay pending until the hub has
                // delivered them, and the reclaim patrol recovers what a dead
                // hub left in flight.
                let streams =
                    wxlike_mq::streams::RedisStreamsMq::connect(&url, Some(&url_consumer(&url)))?
                        .with_timeouts(5_000, wxlike_net::DEFAULT_CLAIM_IDLE_MS);
                let streams = Arc::new(streams);
                let routes = Arc::new(wxlike_mq::redis_backend::RedisRoutes::connect(&url)?);
                Ok(wxlike_mq::MqRouter::with_reliable(
                    streams.clone(),
                    routes,
                    streams,
                ))
            }
            #[cfg(not(feature = "redis"))]
            {
                anyhow::bail!("server built without --features redis; rebuild with: cargo build -p wxlike-server --features redis")
            }
        }
        other => anyhow::bail!("unknown WXLIKE_MQ='{other}' (memory|redis)"),
    }
}

/// Consumer identity per Redis database, so two hubs on one Redis are
/// distinguishable in XPENDING output.
#[cfg(feature = "redis")]
fn url_consumer(url: &str) -> String {
    format!("{}-pid-{}", url, std::process::id())
}

/// Prometheus exposition (WP-13): connections, throughput, parked backlog,
/// delivery latency histogram (p99 via histogram_quantile).
async fn metrics() -> impl IntoResponse {
    let text = wxlike_net::prometheus_text();
    if text.is_empty() {
        return axum::response::Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body::<axum::body::Body>("metrics recorder unavailable".to_string().into())
            .unwrap_or_default();
    }
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; version=0.0.4")
        .header(header::CACHE_CONTROL, "no-store")
        .body::<axum::body::Body>(text.into())
        .unwrap_or_default()
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "service": "wxlike-server",
        "version": env!("CARGO_PKG_VERSION"),
        "ok": true,
    }))
}

/// Pushes platform system envelopes (friend/group lifecycle) into the QUIC hub.
struct HubSink(wxlike_net::HubHandle);

#[async_trait::async_trait]
impl wxlike_platform::EventSink for HubSink {
    async fn publish(&self, env: wxlike_core::Envelope) -> anyhow::Result<()> {
        self.0.publish(&env).await
    }
}

/// The platform's `devices` table behind the push dispatcher's directory
/// trait (WP-06). Adapter lives here for the same layering reason as below.
struct PlatformDevices(std::sync::Arc<wxlike_platform::Platform>);

#[async_trait::async_trait]
impl wxlike_push::DeviceDirectory for PlatformDevices {
    async fn devices_of(&self, user_id: &str) -> anyhow::Result<Vec<(String, String)>> {
        Ok(self
            .0
            .devices_of(user_id)?
            .into_iter()
            .map(|d| (d.platform, d.device_token))
            .collect())
    }
}

/// Hands the hub the platform's group membership table (WP-04). The adapter
/// lives here so `wxlike-platform` does not have to depend on `wxlike-net`.
struct PlatformMembership(std::sync::Arc<wxlike_platform::Platform>);

#[async_trait::async_trait]
impl wxlike_net::Membership for PlatformMembership {
    async fn members_of(&self, conv_id: &str) -> anyhow::Result<Option<Vec<String>>> {
        Ok(self.0.cached_members(conv_id)?)
    }
}
