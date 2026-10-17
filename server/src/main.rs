//! wxlike dev server: platform HTTP API (axum) + MQ-routed QUIC hub.
//!
//! Everything real lives in [`wxlike_server::start_stack`]; this binary only
//! reads the environment and keeps the process alive.
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
        Some(db_env.clone())
    };

    println!("[wxlike-server] hub '{hub_id}' mq={mq_mode}");
    let stack = wxlike_server::start_stack(wxlike_server::StackOptions {
        hub_id: hub_id.clone(),
        db_path,
        mq_mode: mq_mode.clone(),
        http_bind: http_addr.parse()?,
        quic_bind: quic_addr,
    })
    .await?;

    println!("[wxlike-server] QUIC hub on {}", stack.quic_addr);
    println!("[wxlike-server] platform db = {db_env}");
    println!(
        "[wxlike-server] HTTP  on {}  (GET /health /endpoints /metrics, /api/*)",
        stack.http_addr
    );

    tokio::signal::ctrl_c().await?;
    println!("[wxlike-server] shutting down");
    Ok(())
}
