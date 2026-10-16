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

use axum::routing::get;
use axum::Json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    let hub = wxlike_net::run_mq_hub(hub_id.clone(), quic_addr, router).await?;
    let hub_addr = hub.local_addr()?;
    println!("[wxlike-server] QUIC hub on {hub_addr}");

    // Platform data plane is on by default (WP-01): accounts, sessions, profiles.
    let platform = wxlike_platform::Platform::open(db_path)?.shared();
    println!("[wxlike-server] platform db = {}", db_env);
    println!("[wxlike-server] HTTP  on {http_addr}  (GET /health /endpoints, /api/*)");
    std::mem::forget(hub); // run until process exit

    let app = axum::Router::new()
        .route("/health", get(health))
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
        .merge(wxlike_platform::Platform::router(platform));

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
                let mq = Arc::new(wxlike_mq::redis_backend::RedisMq::connect(&url)?);
                let routes = Arc::new(wxlike_mq::redis_backend::RedisRoutes::connect(&url)?);
                Ok(wxlike_mq::MqRouter::new(mq, routes))
            }
            #[cfg(not(feature = "redis"))]
            {
                anyhow::bail!("server built without --features redis; rebuild with: cargo build -p wxlike-server --features redis")
            }
        }
        other => anyhow::bail!("unknown WXLIKE_MQ='{other}' (memory|redis)"),
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "service": "wxlike-server",
        "version": env!("CARGO_PKG_VERSION"),
        "ok": true,
    }))
}
