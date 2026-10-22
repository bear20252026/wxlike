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

/// A2：`/app` 静态托管（`WXLIKE_WEB_DIR`）。
mod web_static;

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use wxlike_core::store::StoredMessage;
use wxlike_core::{Envelope, MsgBody, MsgId, Store};
use wxlike_net::HubHandle;
use wxlike_platform::extract::ApiJson;
use wxlike_platform::{Authed, Platform, PlatformState};

/// Shared by the history/search handlers (they need the platform for
/// authorization and the store for the rows).
pub struct HistoryDeps {
    pub platform: PlatformState,
    pub store: Arc<Store>,
    /// P3：排序与翻页游标以信箱为准（归档只提供正文）。
    pub mailbox: Arc<wxlike_message::MessageStore>,
    /// 发送走 hub 的入站路径（与 QUIC 帧同一条），不另写一套。
    pub hub: HubHandle,
    /// P3 ADR-22: read-only Tantivy replica over the mailbox (a projection —
    /// visibility is always re-checked against the store; the writer lives in
    /// the background indexer thread).
    pub search: std::sync::Arc<wxlike_search::SearchIndex>,
}

/// A running stack: one QUIC hub + one HTTP router over one SQLite file.
pub struct Stack {
    pub platform: PlatformState,
    pub store: Arc<Store>,
    pub hub: HubHandle,
    pub http_addr: std::net::SocketAddr,
    pub quic_addr: std::net::SocketAddr,
    /// 通知 HTTP 服务停止接新连接并排空在飞请求。
    shutdown: tokio::sync::watch::Sender<bool>,
    http_task: tokio::task::JoinHandle<()>,
}

impl Stack {
    /// 优雅关闭：停止接新请求 → 等在飞请求完成（上限 `grace`）→ WAL 落盘。
    ///
    /// 调用方拿到信号（SIGTERM/SIGINT）后调它，然后正常退出进程。
    /// QUIC hub 与 MQ 不单独拆——进程退出时一并回收；这里保证的是
    /// **HTTP 请求不被腰斩**与**SQLite 干净落盘**这两件会留下可见损坏的事。
    pub async fn shutdown(self, grace: std::time::Duration) {
        let _ = self.shutdown.send(true);
        match tokio::time::timeout(grace, self.http_task).await {
            Ok(Ok(())) => tracing::info!("http 已排空并停止"),
            Ok(Err(err)) => tracing::warn!(%err, "http 任务异常结束"),
            Err(_) => tracing::warn!(
                "在 {:?} 内没排空完，强制退出（未完成的请求会被切断）",
                grace
            ),
        }
        self.platform.checkpoint_wal();
    }
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
///
/// 这个函数只做**编排**：读配置 → 开存储 → 起 hub → 拼路由 → 起 HTTP。
/// 每一步的细节都在下面几个 `build_*` 里，改一步不用在 200 行里找位置。
pub async fn start_stack(opts: StackOptions) -> anyhow::Result<Stack> {
    let router = build_mq_router(&opts.mq_mode).await?;
    let platform =
        Platform::open_with(opts.db_path.as_deref(), platform_options_from_env())?.shared();

    let store = Arc::new(Store::open(opts.db_path.as_deref())?);
    let mailbox = Arc::new(wxlike_message::MessageStore::open(opts.db_path.as_deref())?);
    let offline = wxlike_net::offline::SqliteOffline::open(opts.db_path.as_deref(), 5_000)?;
    let search = build_search(opts.db_path.as_deref(), &store)?;
    let video_state = wxlike_video::VideoState {
        // 控制面（上传/建作业/查作业/重试）要认证、配额与回调白名单（issue #7 #8）。
        // 忘了装认证器的后果是"视频控制面不可用"，不是"谁都能用"。
        store: wxlike_video::VideoStore::new(sidecar_dir(opts.db_path.as_deref(), "video"))
            .with_policy(video_policy_from_env(platform.clone())),
    };

    let hub = build_hub(&opts, router, &store, &mailbox, offline, &platform).await?;
    let quic_addr = hub.local_addr()?;
    let handle = hub.handle();

    platform.set_event_sink(Arc::new(HubSink(handle.clone())));
    // 红包清扫 60s 一轮；对账 300s 一轮（比清扫慢，避免与大表扫描抢锁）。
    // 对账不通过时由 reconcile 内部写审计并打日志，运营看板读 reconcile_runs。
    let _sweeper = wxlike_platform::spawn_redpacket_sweeper(
        platform.clone(),
        std::time::Duration::from_secs(60),
    );
    let _reconciler =
        wxlike_platform::spawn_reconciler(platform.clone(), std::time::Duration::from_secs(300));
    std::mem::forget(hub); // run until process exit; the handle above was cloned

    let deps = Arc::new(HistoryDeps {
        platform: platform.clone(),
        store: store.clone(),
        mailbox: mailbox.clone(),
        hub: handle.clone(),
        search,
    });
    let app = build_app(
        opts.hub_id.clone(),
        quic_addr,
        platform.clone(),
        video_state,
        handle.clone(),
        deps,
    );
    let (http_addr, shutdown_tx, http_task) = serve_http(opts.http_bind, app).await?;

    Ok(Stack {
        platform,
        store,
        hub: handle,
        http_addr,
        quic_addr,
        shutdown: shutdown_tx,
        http_task,
    })
}

/// 平台选项：三条凭据都**没有公开默认值**。
///
/// 推流密钥没配就随机生成（重启后旧 token 失效，可接受——它本来就是短期的）；
/// 管理面与视频回调没配就是**关闭**，不是"放行"。默认值等于把这三件事一起敞开。
fn platform_options_from_env() -> wxlike_platform::PlatformOptions {
    let mut options = wxlike_platform::PlatformOptions::default();
    match std::env::var("WXLIKE_LIVE_INGEST_SECRET") {
        Ok(secret) if !secret.is_empty() => options.live_ingest_secret = secret,
        _ => {
            options.live_ingest_secret = wxlike_platform::random_hex(32);
            tracing::warn!(
                "WXLIKE_LIVE_INGEST_SECRET 未设置：已生成本次启动的随机密钥（重启后旧推流 token 失效）"
            );
        }
    }
    options.admin_token = non_empty_env("WXLIKE_ADMIN_TOKEN");
    options.video_callback_token = non_empty_env("WXLIKE_VIDEO_CALLBACK_TOKEN");
    options
}

/// 读一个"配了就用、没配就是 `None`"的环境变量；空串与纯空白算没配。
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 与数据库同级的子目录（`<db 所在目录>/<name>`）；内存库给一个唯一临时目录。
///
/// 两处（搜索索引、视频对象）原本各写了一遍"pop 再 join"，改动时容易只改一处。
fn sidecar_dir(db_path: Option<&str>, name: &str) -> std::path::PathBuf {
    match db_path {
        Some(path) => {
            let mut dir = std::path::PathBuf::from(path);
            dir.pop();
            dir.join(name)
        }
        None => {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            std::env::temp_dir().join(format!(
                "wxlike-{name}-{}-{}-{}",
                std::process::id(),
                wxlike_core::types::now_ms(),
                SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ))
        }
    }
}

/// 打开搜索索引，并把写侧挂到 store 的 append 监听上。
///
/// 读侧**不静默降级到内存索引**：那会让搜索"看着能用、结果全空"，与写侧
/// 刻意返回错误而不是 panic 的策略矛盾。索引打不开就是配置/磁盘问题，必须
/// 启动失败。
fn build_search(
    db_path: Option<&str>,
    store: &Store,
) -> anyhow::Result<Arc<wxlike_search::SearchIndex>> {
    // P3 ADR-22: the Tantivy writer runs in its own thread (commit/fsync off the
    // gateway path — blocking the hub on a search commit stalled deliveries in
    // testing); the query side is a read-only replica whose reader reloads
    // before each search, so a committed batch is visible.
    let search_dir = sidecar_dir(db_path, "search-index");
    let search: Arc<wxlike_search::SearchIndex> = Arc::new(
        wxlike_search::SearchIndex::open_query(&search_dir)
            .map_err(|e| anyhow::anyhow!("open tantivy query index at {search_dir:?}: {e}"))?,
    );

    struct Indexer(wxlike_search::IndexerHandle);
    impl wxlike_core::store::AppendListener for Indexer {
        fn on_append(&self, conv_id: &str, seq: i64, env: &wxlike_core::Envelope) {
            // searchable text = the preview line (all kinds have one);
            // recall filtering happens at query time against the store
            let text = env.body.preview_text();
            if text.trim().is_empty() {
                return;
            }
            self.0.submit(conv_id, seq, &env.sender, text);
        }
    }
    // the writer thread owns its own index instance on the same directory
    // (tantivy allows exactly one writer per directory)
    let writer_index = wxlike_search::SearchIndex::open_writer(&search_dir)
        .map_err(|e| anyhow::anyhow!("open tantivy writer at {search_dir:?}: {e}"))?;
    store.set_append_listener(Arc::new(Indexer(wxlike_search::IndexerHandle::spawn(
        writer_index,
    ))));
    Ok(search)
}

/// 起 QUIC hub：信箱、成员、策略、离线队列、推送钩子都在这里接上。
async fn build_hub(
    opts: &StackOptions,
    router: wxlike_mq::MqRouter,
    store: &Arc<Store>,
    mailbox: &Arc<wxlike_message::MessageStore>,
    offline: Arc<dyn wxlike_net::offline::OfflineStore>,
    platform: &PlatformState,
) -> anyhow::Result<wxlike_net::MqHub> {
    wxlike_net::run_mq_hub_with(
        opts.hub_id.clone(),
        opts.quic_bind,
        router,
        wxlike_net::MqHubOptions {
            storage: Some(store.clone()),
            // 信箱：读扩散的唯一真相。用**独立连接**打开同一个库——
            // MessageStore 的序列号分配是 `UPDATE … RETURNING`（原子），
            // 加上 WAL + busy_timeout，两个连接并发写是安全的。
            mailbox: Some(mailbox.clone()),
            offline,
            membership: Some(Arc::new(PlatformMembership(platform.clone()))),
            policy: Some(Arc::new(PlatformPolicy(platform.clone()))),
            // 身份只从凭据解析：`Hello` 不再接受客户端自报的 user_id（issue #3）。
            // 没有这一行，hub 会拒绝所有连接（fail closed）——这是故意的。
            auth: Some(Arc::new(SessionAuth {
                platform: platform.clone(),
            })),
            // QUIC 证书要跟着数据目录走：换了证书，所有客户端 pin 都对不上，
            // 表现与"有人做中间人"完全相同（issue #12）。
            // 跟着数据目录走：内存库（测试）每次给一个唯一临时目录，
            // 文件库用 db 的父目录 —— 重启后证书不变，pin 仍然匹配。
            cert_dir: Some(sidecar_dir(opts.db_path.as_deref(), "quic")),
            reclaim_idle_ms: Some(wxlike_net::DEFAULT_CLAIM_IDLE_MS),
            push_hook: Some(wxlike_push::AggregatingDispatchHook::spawn(
                wxlike_push::DispatchHook::new(
                    Arc::new(PlatformDevices(platform.clone())),
                    wxlike_push::provider_from_env(),
                ),
                Arc::new(PlatformRules(platform.clone())),
                std::env::var("WXLIKE_PUSH_WINDOW_MS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(30_000),
            )),
            ..Default::default()
        },
    )
    .await
}

/// 视频控制面的认证器：**平台会话表就是真相**。
///
/// 与 hub 的 `SessionAuth` 同一份凭据、同一个 `lookup_token`——视频面不该有
/// 第二套"谁能上传"的答案。认 `Authorization: Bearer <token>`。
struct SessionVideoAuth {
    platform: PlatformState,
}

impl wxlike_video::VideoAuth for SessionVideoAuth {
    fn caller(&self, headers: &axum::http::HeaderMap) -> Option<String> {
        let token = headers
            .get(axum::http::header::AUTHORIZATION)?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")?
            .trim();
        if token.is_empty() {
            return None;
        }
        match self.platform.lookup_token(token) {
            Ok(found) => found,
            Err(err) => {
                // 库坏了不等于"凭据有效"：拒绝并留日志（与 hub 同一约定）
                tracing::error!(error = %err, "lookup_token failed during video auth");
                None
            }
        }
    }
}

/// 视频控制面策略：认证器 + 回调白名单（都是运维配置，不是用户输入）。
///
/// - `WXLIKE_VIDEO_CALLBACK_ALLOW=host1,host2`：允许客户端把回调发到这些主机。
///   **不配就是不允许**——回调是服务端发起的出站请求，默认放开等于把内网
///   探测能力借给任何能提交作业的人。
/// - `WXLIKE_VIDEO_CALLBACK_ALLOW_PRIVATE=1`：额外允许私网/回环目标（本地联调）。
fn video_policy_from_env(platform: PlatformState) -> wxlike_video::VideoPolicy {
    let hosts: Vec<String> = non_empty_env("WXLIKE_VIDEO_CALLBACK_ALLOW")
        .map(|raw| {
            raw.split(',')
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let callbacks = wxlike_video::CallbackPolicy::allow_hosts(hosts)
        .allow_private(non_empty_env("WXLIKE_VIDEO_CALLBACK_ALLOW_PRIVATE").as_deref() == Some("1"))
        // 与平台侧的 `/api/video/callback` 用同一个共享令牌：不配对配置的话
        // 回调会一直 401（平台侧"未配置令牌 = 接口关闭"）。
        .with_token(non_empty_env("WXLIKE_VIDEO_CALLBACK_TOKEN"));
    wxlike_video::VideoPolicy::default()
        .with_auth(std::sync::Arc::new(SessionVideoAuth { platform }))
        .with_callbacks(callbacks)
}

/// 把 platform 的 `sessions` 表包装成 hub 的认证器（issue #3 / A01）。
///
/// hub 不认识 session 表，也不该认识——`crates/net` 只定义
/// [`wxlike_net::HubAuthenticator`]，实现由装配层给。这里就是那一层：
/// `Hello { token }` 里的 token 走 `Platform::lookup_token`（它自己会删过期行），
/// 解析出 `user_id` 后才注册 peers、才 `drain` 离线队列。
struct SessionAuth {
    platform: PlatformState,
}

impl wxlike_net::HubAuthenticator for SessionAuth {
    fn authenticate(&self, token: &str) -> Option<wxlike_net::HubIdentity> {
        if token.is_empty() {
            return None;
        }
        match self.platform.lookup_token(token) {
            Ok(Some(user_id)) => Some(wxlike_net::HubIdentity {
                user_id,
                // 发给客户端的会话标识**不是 token 本身**：用它的指纹，
                // 客户端拿去做"这是我的哪个设备"的对照，泄露了也换不来凭据。
                session_id: session_fingerprint(token),
            }),
            Ok(None) => None,
            Err(err) => {
                // 库坏了不等于"凭据无效"，但也不能放行——两者都返回 None，
                // 区别只在日志里，运维能从 error 与 warn 分开看。
                tracing::error!(error = %err, "lookup_token failed during hub hello");
                None
            }
        }
    }

    fn is_revoked(&self, token: &str) -> bool {
        // 会话被删（退出登录 / 远程下线）或过期 → 长连接也该断。
        // `lookup_token` 对过期行会顺手删掉，所以这里直接复用它的语义。
        // 查库失败按"没被吊销"处理：网络抖动不该把所有人踢下线，
        // 真正的过期会在下一次成功查询时生效。
        match self.platform.lookup_token(token) {
            Ok(Some(_)) => false,
            Ok(None) => true,
            Err(err) => {
                tracing::error!(error = %err, "lookup_token failed during hub heartbeat");
                false
            }
        }
    }
}

/// token 的短指纹（16 hex）。用既有的 `sha256_hex`，不再写第二份 hash。
fn session_fingerprint(token: &str) -> String {
    wxlike_core::msgmeta::sha256_hex(token.as_bytes())[..16].to_string()
}

/// 拼路由：健康检查、平台、视频、WS、在线状态、历史、静态页、开发辅助。
fn build_app(
    hub_id: String,
    quic_addr: std::net::SocketAddr,
    platform: PlatformState,
    video_state: wxlike_video::VideoState,
    handle: HubHandle,
    deps: Arc<HistoryDeps>,
) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route(
            "/endpoints",
            get(move || {
                let hub_id = hub_id.clone();
                async move {
                    Json(serde_json::json!({
                        "hub": hub_id,
                        "quic": quic_addr.to_string(),
                        "alpn": "wxlike/1",
                    }))
                }
            }),
        )
        .merge(wxlike_platform::Platform::router(platform.clone()))
        .merge(wxlike_video::routes(video_state))
        .merge(ws_routes(handle.clone()))
        .merge(presence_routes(handle.clone()))
        .merge(history_routes(deps))
        .merge(web_static::routes())
        .merge(dev_routes(platform))
}

/// 起 HTTP 服务并返回（地址、停机信号、任务句柄）。
async fn serve_http(
    bind: std::net::SocketAddr,
    app: Router,
) -> anyhow::Result<(
    std::net::SocketAddr,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
)> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let http_addr = listener.local_addr()?;
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let http_task = tokio::spawn(async move {
        // with_graceful_shutdown：收到信号后不再 accept，已建立的连接把
        // 手头请求处理完。没有它，launchd 的 SIGTERM 会直接切断在飞请求。
        let stop = async move {
            // 注意：发送端全部被 drop（例如调用方直接丢掉 Stack）时 `changed()`
            // 返回 Err。那**不是**停止请求——测试与嵌入式用法会丢掉 Stack 但
            // 希望服务继续跑。只有真正收到 `true` 才停。
            while shutdown_rx.changed().await.is_ok() {
                if *shutdown_rx.borrow() {
                    return;
                }
            }
            std::future::pending::<()>().await;
        };
        // `into_make_service_with_connect_info` 让 handler 能拿到**真实对端地址**：
        // 限流的来源默认用它，而不是客户端可伪造的 `x-forwarded-for`（issue #19）。
        if let Err(err) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(stop)
        .await
        {
            eprintln!("[wxlike-server] http serve failed: {err}");
        }
    });
    Ok((http_addr, shutdown_tx, http_task))
}

/// P3 ADR-8: the WebSocket gateway. Upgraded sockets speak the SAME wire
/// protocol (bincode WireFrame, Hello first) as QUIC streams — one binary WS
/// message per frame — and enter the hub through the same serve path, so
/// multi-device fan-out, parking, dedup and receipts behave identically.
fn ws_routes(handle: HubHandle) -> Router {
    use axum::extract::ws::WebSocketUpgrade;
    Router::new().route(
        "/ws",
        get(move |upgrade: WebSocketUpgrade| async move {
            // AR-2/H-9 (audit round 4): one resource boundary for both
            // transports. The QUIC path caps frames at MAX_FRAME before
            // allocating; an unauthenticated WS peer used to be able to push
            // the axum default (64 MiB) per message.
            upgrade
                .max_message_size(wxlike_net::framing::MAX_FRAME as usize)
                .max_frame_size(wxlike_net::framing::MAX_FRAME as usize)
                .on_upgrade(move |socket| async move {
                    let conn = wxlike_net::conn::ws::peer_conn(socket);
                    let _ = handle.attach_transport(conn).await;
                })
        }),
    )
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

/// The NatsRoutes table shares the backend's JetStream context.
#[cfg(feature = "nats")]
fn mq_context(mq: &wxlike_mq::nats_backend::NatsMq) -> async_nats::jetstream::Context {
    mq.context()
}

async fn build_mq_router(mq_mode: &str) -> anyhow::Result<wxlike_mq::MqRouter> {
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
        "nats" => {
            #[cfg(feature = "nats")]
            {
                let url =
                    std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
                let mq = Arc::new(wxlike_mq::nats_backend::NatsMq::connect(&url).await?);
                let routes =
                    Arc::new(wxlike_mq::nats_backend::NatsRoutes::connect(&mq_context(&mq)).await?);
                Ok(wxlike_mq::MqRouter::with_reliable(mq.clone(), routes, mq))
            }
            #[cfg(not(feature = "nats"))]
            {
                anyhow::bail!("server built without --features nats; rebuild with: cargo build -p wxlike-server --features nats")
            }
        }
        other => anyhow::bail!("unknown mq mode '{other}' (memory|redis|nats)"),
    }
}

/// Consumer identity per Redis database, so two hubs on one Redis are
/// distinguishable in XPENDING output.
///
/// AR-2/H-10 (audit round 4): the URL itself is deliberately NOT part of the
/// name — Redis connection strings carry the password (`redis://:pw@host`),
/// and consumer names surface in XINFO CONSUMERS / XPENDING output and slow
/// logs. A short SHA-8 of the URL keeps per-database distinctness without
/// ever printing the credentials.
#[cfg(feature = "redis")]
fn url_consumer(url: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let hash = hex::encode(&Sha256::digest(url.as_bytes())[..4]);
    format!("hub-{hash}-pid-{}", std::process::id())
}

// ---------------------------------------------------------------------------
// history + search (P1)
// ---------------------------------------------------------------------------

pub fn history_routes(deps: Arc<HistoryDeps>) -> Router {
    Router::new()
        .route("/api/history/{conv_id}", get(history_page))
        .route("/api/conv/{conv_id}/send", post(send_message))
        .route("/api/conv/{conv_id}/recall", post(recall_message))
        .route("/api/conv/{conv_id}/forward", post(forward_message))
        .route("/api/conv/{conv_id}/msg/{msg_id}", get(one_message))
        .route("/api/search/messages", get(search_messages))
        .with_state(deps)
}

/// May `user_id` read conversation `conv_id`? Direct chats: the caller must
/// be one of the two id halves. Groups: platform membership. This check is
/// what keeps history/search from becoming a cross-tenant read primitive.
pub fn authorize_conv(platform: &Platform, user_id: &str, conv_id: &str) -> bool {
    // 规则只有一份（`Platform::may_read_conv`）：媒体下载也要用它，
    // 两处各写一遍迟早漂移。
    platform.may_read_conv(user_id, conv_id)
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
    message_json_with_seq(row, row.seq)
}

/// 消失消息判定：`ttl_ms == 0` 表示不消失；时间戳缺失时**不隐藏**。
fn secret_ttl_expired(ttl_ms: u64, v: &serde_json::Value) -> bool {
    if ttl_ms == 0 {
        return false;
    }
    match v["sent_at_ms"].as_u64() {
        None => false,
        Some(sent) => sent.saturating_add(ttl_ms) <= wxlike_core::types::now_ms(),
    }
}

/// 同上，但用调用方给的序列号（history 用信箱序列号，search 用归档 seq）。
pub fn message_json_with_seq(row: &StoredMessage, seq: i64) -> serde_json::Value {
    let env = &row.envelope;
    let mut v = serde_json::json!({
        "conv_id": env.conv_id,
        "seq": seq,
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
    let before = q.before_seq.unwrap_or(0);
    let owner = deps
        .platform
        .owner_kind_of(&conv_id)
        .unwrap_or(wxlike_message::OwnerType::Group);

    // P3：**信箱决定顺序**，归档只提供正文。
    // 这样翻页游标是会话内序列号（1,2,3…），与未读/同步用的是同一套编号；
    // 归档的全局 seq 不再泄露给客户端。
    //
    // 消失消息（P4 / issue #15）：服务端不持明文，但**必须**守住"到点不再
    // 下发"这条线，否则换设备或重装后拉历史会让消失的消息复活。
    let ttl_ms = deps
        .platform
        .secret_state(&conv_id)
        .map(|s| s.ttl_ms)
        .unwrap_or(0);
    let (page, next) = deps
        .mailbox
        .fetch_before((owner, &conv_id), before.max(0) as u64, limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if page.is_empty() {
        // 老数据（镜像之前写入的）没有信箱行：退回归档分页，保证历史不丢
        let (rows, next) = deps
            .store
            .page_messages(&conv_id, before, limit)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let messages: Vec<_> = rows
            .iter()
            .map(message_json)
            .filter(|v| !secret_ttl_expired(ttl_ms, v))
            .collect();
        return Ok(Json(serde_json::json!({
            "conv_id": conv_id,
            "messages": messages,
            "next_before_seq": next,
        })));
    }

    let ids: Vec<Vec<u8>> = page
        .iter()
        .filter_map(|m| hex::decode(&m.msg_id).ok())
        .collect();
    let bodies = deps
        .store
        .by_msg_ids(&ids)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let messages: Vec<_> = page
        .iter()
        .filter_map(|m| {
            let id = hex::decode(&m.msg_id).ok()?;
            let row = bodies.get(&id)?;
            Some(message_json_with_seq(row, m.seq as i64))
        })
        .filter(|v| !secret_ttl_expired(ttl_ms, v))
        .collect();
    Ok(Json(serde_json::json!({
        "conv_id": conv_id,
        "messages": messages,
        "next_before_seq": next,
    })))
}

#[derive(serde::Deserialize)]
struct SendBody {
    text: String,
    /// 幂等键：同一个键重试会落在**同一条消息**上（msg_id 由它派生）。
    #[serde(default)]
    idempotency_key: Option<String>,
}

/// `POST /api/conv/{conv}/send`：HTTP 控制面的发消息入口。
///
/// 走的是 hub 的入站路径（`HubHandle::submit`），因此投递策略、落库、
/// 镜像信箱、扇出与 QUIC 帧**完全一致**——不另写一套发送逻辑。
async fn send_message(
    State(deps): State<Arc<HistoryDeps>>,
    authed: HistoryAuth,
    Path(conv_id): Path<String>,
    ApiJson(body): ApiJson<SendBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !authorize_conv(&deps.platform, &authed.user_id, &conv_id) {
        return Err((StatusCode::FORBIDDEN, "not your conversation".into()));
    }
    let text = body.text.trim();
    if text.is_empty() || text.chars().count() > 5_000 {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "text required".into()));
    }

    // 直聊必须显式给收件人：hub 对非群会话按 `env.to` 投递
    let to = match conv_id.strip_prefix("d-") {
        Some(pair) => pair
            .split('~')
            .filter(|u| *u != authed.user_id)
            .map(str::to_string)
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    let mut env = Envelope::direct(&conv_id, authed.user_id.clone(), to, 0, text);
    if let Some(key) = body
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        // 幂等：同 key → 同 msg_id → 被既有的去重与唯一索引挡住
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        hasher.update(key.as_bytes());
        hasher.update(authed.user_id.as_bytes());
        hasher.update(conv_id.as_bytes());
        let digest = hasher.finalize();
        let mut id: MsgId = [0u8; 16];
        id.copy_from_slice(&digest[..16]);
        env.msg_id = id;
    }

    deps.hub
        .submit(&authed.user_id, &env)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "msg_id": hex(env.msg_id),
        "conv_id": conv_id,
        "accepted": true,
    })))
}

#[derive(serde::Deserialize)]
struct RecallBody {
    msg_id: String,
}

/// `POST /api/conv/{conv}/recall`：撤回自己的消息。
///
/// hub 内部对非作者是**静默丢弃**（不给探测口），但 HTTP 调用方需要知道
/// 到底成没成，所以这里先查作者再提交——鉴权在入口做，语义才清楚。
async fn recall_message(
    State(deps): State<Arc<HistoryDeps>>,
    authed: HistoryAuth,
    Path(conv_id): Path<String>,
    ApiJson(body): ApiJson<RecallBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !authorize_conv(&deps.platform, &authed.user_id, &conv_id) {
        return Err((StatusCode::FORBIDDEN, "not your conversation".into()));
    }
    let id = hex::decode(body.msg_id.trim())
        .map_err(|_| (StatusCode::BAD_REQUEST, "msg_id must be hex".to_string()))?;
    if id.len() != 16 {
        return Err((StatusCode::BAD_REQUEST, "msg_id must be 16 bytes".into()));
    }
    let mut target: MsgId = [0u8; 16];
    target.copy_from_slice(&id);

    let author = deps
        .store
        .author_of(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    match author {
        None => return Err((StatusCode::NOT_FOUND, "no such message".into())),
        Some(author) if author != authed.user_id => {
            return Err((StatusCode::FORBIDDEN, "只能撤回自己的消息".into()))
        }
        Some(_) => {}
    }

    let env = Envelope::recall(&conv_id, authed.user_id.clone(), target);
    deps.hub
        .submit(&authed.user_id, &env)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({
        "recalled": true,
        "msg_id": body.msg_id,
    })))
}

#[derive(serde::Deserialize)]
struct ForwardBody {
    from_conv: String,
    to_conv: String,
    msg_id: String,
    idempotency_key: String,
}

/// `POST /api/conv/{conv}/forward`：把一条消息转发到另一个会话。
///
/// `conv` 是**目标**会话；`from_conv` 必须在 body 里，且两边都要鉴权——
/// 只查目标会让 agent 把任意会话的内容转发出去。
async fn forward_message(
    State(deps): State<Arc<HistoryDeps>>,
    authed: HistoryAuth,
    Path(to_conv): Path<String>,
    ApiJson(body): ApiJson<ForwardBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // 路径与 body 都带目标会话：不一致说明调用方拼错了，宁可 400 也别猜
    if body.to_conv != to_conv {
        return Err((StatusCode::BAD_REQUEST, "to_conv 与路径不一致".to_string()));
    }
    for conv in [&body.from_conv, &to_conv] {
        if !authorize_conv(&deps.platform, &authed.user_id, conv) {
            return Err((
                StatusCode::FORBIDDEN,
                format!("not your conversation: {conv}"),
            ));
        }
    }
    // 秘密会话的消息不可转发（issue #15）：转发会破坏"两端各持一份密钥"的前提，
    // 消失消息在第三方副本上也会失效。桌面端早就有这道拦截，但 HTTP 控制面
    // 是另一条入口——**服务端必须自己守**，不能只靠客户端自觉。
    if deps
        .platform
        .is_secret_conv(&body.from_conv)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        return Err((
            StatusCode::FORBIDDEN,
            "secret conversations cannot be forwarded".to_string(),
        ));
    }
    // 目标会话同样：往秘密会话里塞一条普通会话的消息，接收方会以为它受保护
    if deps
        .platform
        .is_secret_conv(&to_conv)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    {
        return Err((
            StatusCode::FORBIDDEN,
            "cannot forward into a secret conversation".to_string(),
        ));
    }
    let id = hex::decode(body.msg_id.trim())
        .map_err(|_| (StatusCode::BAD_REQUEST, "msg_id must be hex".to_string()))?;
    let mut found = deps
        .store
        .by_msg_ids(std::slice::from_ref(&id))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let Some(row) = found.remove(&id) else {
        return Err((StatusCode::NOT_FOUND, "no such message".into()));
    };
    if row.withdrawn {
        return Err((StatusCode::CONFLICT, "源消息已撤回".into()));
    }

    let to = match to_conv.strip_prefix("d-") {
        Some(pair) => pair
            .split('~')
            .filter(|u| *u != authed.user_id)
            .map(str::to_string)
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    let mut env = Envelope::with_body(
        &to_conv,
        authed.user_id.clone(),
        to,
        0,
        row.envelope.body.clone(),
    );
    // 幂等：同 key + 同目标会话 → 同 msg_id
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(body.idempotency_key.as_bytes());
    hasher.update(authed.user_id.as_bytes());
    hasher.update(to_conv.as_bytes());
    let digest = hasher.finalize();
    let mut new_id: MsgId = [0u8; 16];
    new_id.copy_from_slice(&digest[..16]);
    env.msg_id = new_id;

    deps.hub
        .submit(&authed.user_id, &env)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({
        "forwarded": true,
        "conv_id": to_conv,
        "msg_id": hex(env.msg_id),
    })))
}

/// `GET /api/conv/{conv}/msg/{msg_id}`：取单条消息。
///
/// 信箱只存 id 与顺序，正文在归档里——所以先按 id 取归档行，再用**信箱的**
/// 序列号回填 `seq`，与 history 保持同一套编号。
async fn one_message(
    State(deps): State<Arc<HistoryDeps>>,
    authed: HistoryAuth,
    Path((conv_id, msg_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !authorize_conv(&deps.platform, &authed.user_id, &conv_id) {
        return Err((StatusCode::FORBIDDEN, "not your conversation".into()));
    }
    let id = hex::decode(msg_id.trim())
        .map_err(|_| (StatusCode::BAD_REQUEST, "msg_id must be hex".to_string()))?;
    let mut found = deps
        .store
        .by_msg_ids(std::slice::from_ref(&id))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let Some(row) = found.remove(&id) else {
        return Err((StatusCode::NOT_FOUND, "no such message".into()));
    };
    // 跨会话取消息要拒绝：否则知道 msg_id 就能读到别人会话的内容
    if row.envelope.conv_id != conv_id {
        return Err((
            StatusCode::NOT_FOUND,
            "no such message in this conversation".into(),
        ));
    }
    let owner = if conv_id.starts_with("d-") {
        wxlike_message::OwnerType::Direct
    } else {
        wxlike_message::OwnerType::Group
    };
    let seq = deps
        .mailbox
        .seq_of((owner, &conv_id), &msg_id)
        .ok()
        .flatten()
        .map(|s| s as i64)
        .unwrap_or(row.seq);
    Ok(Json(message_json_with_seq(&row, seq)))
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

    // index search (candidate generation) -> store truth (withdraw check +
    // authoritative rendering). The index is a projection; the store rules.
    let candidates = deps
        .search
        .search(needle, &convs, limit)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut results = Vec::new();
    for hit in candidates {
        if !convs.contains(&hit.conv_id) {
            continue; // re-check: the index could be stale relative to authz
        }
        if deps
            .store
            .is_withdrawn_by_seq(&hit.conv_id, hit.seq)
            .unwrap_or(true)
        {
            continue; // recalled rows never surface through search
        }
        if let Ok(Some(row)) = deps.store.message_at(&hit.conv_id, hit.seq) {
            let v = message_json(&row);
            // 消失消息到期后连搜索结果都不能出现（issue #15）：历史里已经
            // 过滤了，搜索漏掉的话"消失"就只完成了一半。
            let ttl_ms = deps
                .platform
                .secret_state(&hit.conv_id)
                .map(|s| s.ttl_ms)
                .unwrap_or(0);
            if !secret_ttl_expired(ttl_ms, &v) {
                results.push(v);
            }
        }
        if results.len() >= limit {
            break;
        }
    }
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
/// Opt-in per process: `WXLIKE_DEV_FAUCET=1` **and** `WXLIKE_ADMIN_TOKEN`
/// configured — the faucet mints money, so it demands the admin token
/// (`x-admin-token` header) like every other admin surface (AR-2/H-1, audit
/// round 4). Without an admin token the faucet stays off: "没配就是关".
fn dev_routes(platform: PlatformState) -> Router {
    /// Per-call cap: enough for any demo, far below the ledger ceiling — an
    /// authorized-but-compromised caller must not be able to hyperinflate
    /// the virtual economy in one request.
    const FAUCET_MAX_PER_CALL: i64 = 100_000_00;
    let mut app = axum::Router::new();
    if std::env::var("WXLIKE_DEV_FAUCET").as_deref() == Ok("1") {
        if platform
            .options()
            .admin_token
            .as_deref()
            .is_none_or(|t| t.is_empty())
        {
            eprintln!(
                "[wxlike-server] WXLIKE_DEV_FAUCET=1 but WXLIKE_ADMIN_TOKEN is not set; \
                 the faucet stays OFF (it mints money — it will not run unauthenticated)"
            );
            return app;
        }
        eprintln!(
            "[wxlike-server] DEV FAUCET ENABLED: /api/dev/fund is live, admin-token gated, \
             max {FAUCET_MAX_PER_CALL} per call — do not expose this process to the internet"
        );
        let state = platform;
        app = app.route(
            "/api/dev/fund",
            axum::routing::post(
                move |headers: axum::http::HeaderMap,
                      ApiJson(body): ApiJson<serde_json::Value>| {
                    let state = state.clone();
                    async move {
                        let presented = headers
                            .get("x-admin-token")
                            .and_then(|v| v.to_str().ok());
                        if !state.admin_ok(presented) {
                            return (
                                StatusCode::FORBIDDEN,
                                "{\"error\":\"admin token required\"}".to_string(),
                            );
                        }
                        let user_id = body["user_id"].as_str().unwrap_or_default();
                        let amount = body["amount"].as_i64().unwrap_or(0);
                        if amount <= 0 || amount > FAUCET_MAX_PER_CALL {
                            return (
                                StatusCode::UNPROCESSABLE_ENTITY,
                                format!(
                                    "{{\"error\":\"amount must be 1..={FAUCET_MAX_PER_CALL}\"}}"
                                ),
                            );
                        }
                        // The fund ledger row itself is the audit trail; the
                        // log line gives operators a real-time view.
                        tracing::warn!(
                            user = user_id,
                            amount,
                            "dev faucet fund (admin-authenticated)"
                        );
                        match state.fund(user_id, amount, "dev faucet") {
                            Ok(()) => (StatusCode::NO_CONTENT, "".to_string()),
                            Err(err) => (
                                StatusCode::UNPROCESSABLE_ENTITY,
                                format!("{{\"error\":\"{err}\"}}"),
                            ),
                        }
                    }
                },
            ),
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

/// P3 投递策略适配器：把 hub 的裁决咨询翻译成平台关系/禁言表查询。
/// 查询失败一律放行（fail-open）——拦截增强不得成为可用性单点。
struct PlatformPolicy(PlatformState);

#[async_trait::async_trait]
impl wxlike_net::DeliveryPolicy for PlatformPolicy {
    async fn blocked(&self, from: &str, to: &str) -> bool {
        match self.0.is_blocked(from, to) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(from, to, "policy blocked lookup failed (fail-open): {err}");
                false
            }
        }
    }

    async fn muted(&self, group: &str, user: &str) -> bool {
        match self.0.is_muted(group, user) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(group, user, "policy mute lookup failed (fail-open): {err}");
                false
            }
        }
    }

    /// P4 发送闸门：风控限流 + 内容审核（ADR-20 前两层）。
    ///
    /// 失败语义是 **fail-open**：风控/审核查询出错时放行并记日志——拦截
    /// 增强不能变成可用性单点（与 P3 投递策略同一约定）。
    async fn send_gate(&self, from: &str, conv: &str, text: &str) -> wxlike_net::SendGate {
        let now = wxlike_core::types::now_ms();
        match self.0.risk().check(wxlike_risk::Action::Send, from, now) {
            Ok(wxlike_risk::Verdict::Allow) => {}
            Ok(wxlike_risk::Verdict::Throttle { retry_after_ms }) => {
                return wxlike_net::SendGate::Throttle { retry_after_ms };
            }
            Ok(wxlike_risk::Verdict::Deny { reason }) => {
                return wxlike_net::SendGate::Reject { reason };
            }
            Ok(wxlike_risk::Verdict::Challenge { .. }) => {
                // 发送路径没有挑战交互，按限流处理
                return wxlike_net::SendGate::Throttle {
                    retry_after_ms: 1_000,
                };
            }
            Err(err) => tracing::warn!(from, "risk check failed (fail-open): {err}"),
        }
        if let Err(err) = self.0.risk().record(wxlike_risk::Action::Send, from, now) {
            tracing::warn!(from, "risk record failed: {err}");
        }
        match self.0.moderate_for_send(from, conv, text) {
            Ok(v) => match v.decision.outcome {
                wxlike_moderation::Outcome::Publish
                | wxlike_moderation::Outcome::PublishFlagged => wxlike_net::SendGate::Allow,
                wxlike_moderation::Outcome::Hold => wxlike_net::SendGate::Reject {
                    reason: "held for review".into(),
                },
                wxlike_moderation::Outcome::Reject => wxlike_net::SendGate::Reject {
                    reason: "blocked by moderation".into(),
                },
            },
            Err(err) => {
                tracing::warn!(from, conv, "moderation failed (fail-open): {err}");
                wxlike_net::SendGate::Allow
            }
        }
    }

    async fn direct_allowed(&self, from: &str, to: &str) -> bool {
        let friends = self.0.are_friends(from, to).unwrap_or(false);
        if friends {
            return true;
        }
        // 不变式 9：非好友需对方接受过陌生人请求（接受→正常会话）
        self.0.stranger_accepted(from, to).unwrap_or(false)
    }
}

/// P3 免打扰规则适配器：hub 决策层的查询落到平台通知设置表。
struct PlatformRules(PlatformState);

#[async_trait::async_trait]
impl wxlike_push::MuteRuleDirectory for PlatformRules {
    async fn rule_of(&self, user: &str, conv: &str) -> Option<wxlike_push::MuteRule> {
        let raw = self.0.notification_rule(user, conv).ok()??;
        let level = wxlike_push::MuteLevel::parse(&raw.level)?;
        Some(wxlike_push::MuteRule::new(level, &raw.keywords))
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
