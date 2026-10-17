//! wxlike server stack, split from `main.rs` so integration tests can boot
//! the **real** platform + QUIC hub + HTTP routes in-process (the task book
//! bans mocks at this boundary).
//!
//! What lives here beyond the wiring:
//! * `GET /api/history/{conv_id}` — paged conversation history (P1: UI cloud
//!   sync). Authorization is enforced here, not in the client: a direct conv
//!   requires being one of its two halves, a group conv requires membership.
//! * `GET /api/search/messages?q=` — the P1 search protocol over the hub's
//!   store, restricted to the conversations the caller may see.
//!
//! Both read the same `Arc<Store>` the hub persists into, so what you search
//! is exactly what was routed.

use std::sync::Arc;

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use wxlike_core::store::StoredMessage;
use wxlike_core::{Envelope, MsgBody, MsgId, Store};
use wxlike_net::HubHandle;
use wxlike_platform::{Authed, Platform, PlatformState};

/// Shared by the history/search handlers (they need the platform for
/// authorization and the store for the rows).
pub struct HistoryDeps {
    pub platform: PlatformState,
    pub store: Arc<Store>,
}

/// A running stack: one QUIC hub + one HTTP router over one SQLite file.
pub struct Stack {
    pub platform: PlatformState,
    pub store: Arc<Store>,
    pub hub: HubHandle,
    pub http_addr: std::net::SocketAddr,
    pub quic_addr: std::net::SocketAddr,
}

#[derive(Clone, Debug)]
pub struct StackOptions {
    pub hub_id: String,
    /// `None` = in-memory (tests, demos).
    pub db_path: Option<String>,
    /// "memory" | "redis"
    pub mq_mode: String,
    pub http_bind: std::net::SocketAddr,
    pub quic_bind: std::net::SocketAddr,
}

impl Default for StackOptions {
    fn default() -> Self {
        Self {
            hub_id: "hub-1".into(),
            db_path: None,
            mq_mode: "memory".into(),
            http_bind: "127.0.0.1:0".parse().unwrap(),
            quic_bind: "127.0.0.1:0".parse().unwrap(),
        }
    }
}

/// Boots the whole stack on the given binds (OS-assigned ports with `:0`).
pub async fn start_stack(opts: StackOptions) -> anyhow::Result<Stack> {
    let router = build_mq_router(&opts.mq_mode)?;
    let platform = Platform::open(opts.db_path.as_deref())?.shared();

    let store = Arc::new(Store::open(opts.db_path.as_deref())?);
    let offline = wxlike_net::offline::SqliteOffline::open(opts.db_path.as_deref(), 5_000)?;

    // video-service (P2 minimal, ADR-21): colocated with the server until
    // the load split (Q7); its storage lives beside the platform db.
    let video_root = match &opts.db_path {
        Some(path) => {
            let mut dir = std::path::PathBuf::from(path);
            dir.pop();
            dir.join("video")
        }
        None => std::env::temp_dir().join(format!(
            "wxlike-video-{}-{}",
            std::process::id(),
            wxlike_core::types::now_ms()
        )),
    };
    let video_state = wxlike_video::VideoState {
        store: wxlike_video::VideoStore::new(video_root),
    };
    let hub = wxlike_net::run_mq_hub_with(
        opts.hub_id.clone(),
        opts.quic_bind,
        router,
        wxlike_net::MqHubOptions {
            storage: Some(store.clone()),
            offline,
            membership: Some(Arc::new(PlatformMembership(platform.clone()))),
            reclaim_idle_ms: Some(wxlike_net::DEFAULT_CLAIM_IDLE_MS),
            push_hook: Some(wxlike_push::DispatchHook::new(
                Arc::new(PlatformDevices(platform.clone())),
                wxlike_push::provider_from_env(),
            )),
            ..Default::default()
        },
    )
    .await?;
    let quic_addr = hub.local_addr()?;
    let handle = hub.handle();

    platform.set_event_sink(Arc::new(HubSink(handle.clone())));
    let _sweeper = wxlike_platform::spawn_redpacket_sweeper(
        platform.clone(),
        std::time::Duration::from_secs(60),
    );
    std::mem::forget(hub); // run until process exit; the handle above was cloned    let hub_id = opts.hub_id.clone();
    let deps = Arc::new(HistoryDeps {
        platform: platform.clone(),
        store: store.clone(),
    });
    let app = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route(
            "/endpoints",
            get(move || async move {
                Json(serde_json::json!({
                    "hub": opts.hub_id,
                    "quic": quic_addr.to_string(),
                    "alpn": "wxlike/1",
                }))
            }),
        )
        .merge(wxlike_platform::Platform::router(platform.clone()))
        .merge(wxlike_video::routes(video_state))
        .merge(presence_routes(handle.clone()))
        .merge(history_routes(deps))
        .merge(dev_routes(platform.clone()));

    let listener = tokio::net::TcpListener::bind(&opts.http_bind).await?;
    let http_addr = listener.local_addr()?;
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            eprintln!("[wxlike-server] http serve failed: {err}");
        }
    });

    Ok(Stack {
        platform,
        store,
        hub: handle.clone(),
        http_addr,
        quic_addr,
    })
}

fn presence_routes(handle: HubHandle) -> Router {
    Router::new().route(
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
}

fn build_mq_router(mq_mode: &str) -> anyhow::Result<wxlike_mq::MqRouter> {
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
        other => anyhow::bail!("unknown mq mode '{other}' (memory|redis)"),
    }
}

/// Consumer identity per Redis database, so two hubs on one Redis are
/// distinguishable in XPENDING output.
#[cfg(feature = "redis")]
fn url_consumer(url: &str) -> String {
    format!("{url}-pid-{}", std::process::id())
}

// ---------------------------------------------------------------------------
// history + search (P1)
// ---------------------------------------------------------------------------

pub fn history_routes(deps: Arc<HistoryDeps>) -> Router {
    Router::new()
        .route("/api/history/{conv_id}", get(history_page))
        .route("/api/search/messages", get(search_messages))
        .with_state(deps)
}

/// May `user_id` read conversation `conv_id`? Direct chats: the caller must
/// be one of the two id halves. Groups: platform membership. This check is
/// what keeps history/search from becoming a cross-tenant read primitive.
pub fn authorize_conv(platform: &Platform, user_id: &str, conv_id: &str) -> bool {
    if let Some(rest) = conv_id.strip_prefix("d-") {
        let parts: Vec<&str> = rest.split('~').collect();
        return parts.len() == 2 && parts.contains(&user_id);
    }
    platform.is_member(conv_id, user_id).unwrap_or(false)
}

/// Every conversation whose messages `user_id` may see: their groups plus the
/// direct pairs they belong to that actually have rows.
pub fn allowed_convs(platform: &Platform, store: &Store, user_id: &str) -> Vec<String> {
    let mut convs = platform.groups_of_user(user_id).unwrap_or_default();
    if let Ok(all) = store.list_conversations() {
        for conv in all {
            if conv.id.starts_with("d-") && authorize_conv(platform, user_id, &conv.id) {
                convs.push(conv.id);
            }
        }
    }
    convs.sort();
    convs.dedup();
    convs
}

/// One history/search row in the wire shape the UI renders (mirrors the hub
/// event payload so the client maps both the same way).
pub fn message_json(row: &StoredMessage) -> serde_json::Value {
    let env = &row.envelope;
    let mut v = serde_json::json!({
        "conv_id": env.conv_id,
        "seq": row.seq,
        "sender": env.sender,
        "to": env.to,
        "msg_id": hex(env.msg_id),
        "sent_at_ms": env.sent_at_ms,
        "withdrawn": row.withdrawn,
        "quote_of": env.quote_of.map(hex),
    });
    match &env.body {
        MsgBody::Text(t) => {
            v["kind"] = "text".into();
            v["text"] = t.as_str().into();
        }
        MsgBody::Bytes(b) => {
            v["kind"] = "image".into();
            v["media_id"] = String::from_utf8_lossy(b).into_owned().into();
        }
        MsgBody::Notice(n) => {
            v["kind"] = "system".into();
            v["text"] = n.as_str().into();
        }
        MsgBody::Recall { target } => {
            v["kind"] = "recall".into();
            v["target"] = hex(*target).into();
        }
        MsgBody::Custom { kind, payload } => {
            v["kind"] = kind.as_str().into();
            v["payload"] = payload.clone();
        }
    }
    v
}

fn hex(id: MsgId) -> String {
    hex::encode(id)
}

/// Bearer auth for the history/search routes. Same token semantics as the
/// platform's [`Authed`]; it exists only because those routes carry
/// `Arc<HistoryDeps>` as state and `Authed` is implemented over
/// `Arc<Platform>`.
pub struct HistoryAuth {
    pub user_id: String,
}

impl FromRequestParts<Arc<HistoryDeps>> for HistoryAuth {
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &Arc<HistoryDeps>,
    ) -> Result<Self, Self::Rejection> {
        let token = Authed::bearer_from(&parts.headers)
            .ok_or((StatusCode::UNAUTHORIZED, "missing bearer token"))?;
        let token = token.to_string();
        match state.platform.lookup_token(&token) {
            Ok(Some(user_id)) => Ok(HistoryAuth { user_id }),
            _ => Err((StatusCode::UNAUTHORIZED, "invalid or expired token")),
        }
    }
}

#[derive(serde::Deserialize, Default)]
struct HistoryQuery {
    /// Exclusive upper bound; 0/absent = first (newest) page.
    before_seq: Option<i64>,
    limit: Option<usize>,
}

async fn history_page(
    State(deps): State<Arc<HistoryDeps>>,
    authed: HistoryAuth,
    Path(conv_id): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !authorize_conv(&deps.platform, &authed.user_id, &conv_id) {
        return Err((StatusCode::FORBIDDEN, "not your conversation".into()));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let (rows, next) = deps
        .store
        .page_messages(&conv_id, q.before_seq.unwrap_or(0), limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let messages: Vec<_> = rows.iter().map(message_json).collect();
    Ok(Json(serde_json::json!({
        "conv_id": conv_id,
        "messages": messages,
        "next_before_seq": next,
    })))
}

#[derive(serde::Deserialize, Default)]
struct SearchQuery {
    q: String,
    limit: Option<usize>,
}

async fn search_messages(
    State(deps): State<Arc<HistoryDeps>>,
    authed: HistoryAuth,
    Query(q): Query<SearchQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let needle = q.q.trim();
    if needle.is_empty() {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "empty query".into()));
    }
    let convs = allowed_convs(&deps.platform, &deps.store, &authed.user_id);
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let rows = deps
        .store
        .search_messages(needle, &convs, limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let results: Vec<_> = rows.iter().map(message_json).collect();
    Ok(Json(serde_json::json!({
        "query": needle,
        "results": results,
    })))
}

// ---------------------------------------------------------------------------
// handlers carried over from the old main
// ---------------------------------------------------------------------------

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "service": "wxlike-server",
        "version": env!("CARGO_PKG_VERSION"),
        "ok": true,
    }))
}

/// Prometheus exposition (WP-13): connections, throughput, parked backlog,
/// delivery latency histogram (p99 via histogram_quantile).
async fn metrics() -> impl IntoResponse {
    let mut text = wxlike_net::prometheus_text();
    if text.is_empty() {
        // exposition-format comment: a cold-but-healthy hub scrapes 200
        text = "# wxlike hub metrics: nothing recorded yet\n".to_string();
    }
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/plain; version=0.0.4")
        .header(header::CACHE_CONTROL, "no-store")
        .body::<axum::body::Body>(text.into())
        .unwrap_or_default()
}

/// Virtual-money faucet for demos and the UI integration tests (WP-09 is a
/// closed virtual ledger; real funding is a licensed channel and stays out).
/// Opt-in per process: `WXLIKE_DEV_FAUCET=1`.
fn dev_routes(platform: PlatformState) -> Router {
    let mut app = axum::Router::new();
    if std::env::var("WXLIKE_DEV_FAUCET").as_deref() == Ok("1") {
        let state = platform;
        app = app.route(
            "/api/dev/fund",
            axum::routing::post(move |Json(body): Json<serde_json::Value>| {
                let state = state.clone();
                async move {
                    let user_id = body["user_id"].as_str().unwrap_or_default();
                    let amount = body["amount"].as_i64().unwrap_or(0);
                    match state.fund(user_id, amount, "dev faucet") {
                        Ok(()) => (StatusCode::NO_CONTENT, "".to_string()),
                        Err(err) => (
                            StatusCode::UNPROCESSABLE_ENTITY,
                            format!("{{\"error\":\"{err}\"}}"),
                        ),
                    }
                }
            }),
        );
    }
    app
}

/// Pushes platform system envelopes (friend/group lifecycle) into the QUIC hub.
struct HubSink(HubHandle);

#[async_trait::async_trait]
impl wxlike_platform::EventSink for HubSink {
    async fn publish(&self, env: Envelope) -> anyhow::Result<()> {
        self.0.publish(&env).await
    }
}

/// The platform's `devices` table behind the push dispatcher's directory
/// trait (WP-06). Adapter lives here for the same layering reason as below.
struct PlatformDevices(PlatformState);

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
struct PlatformMembership(PlatformState);

#[async_trait::async_trait]
impl wxlike_net::Membership for PlatformMembership {
    async fn members_of(&self, conv_id: &str) -> anyhow::Result<Option<Vec<String>>> {
        Ok(self.0.cached_members(conv_id)?)
    }
}
