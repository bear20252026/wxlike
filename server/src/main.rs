//! wxlike server: platform HTTP API (axum) + MQ-routed QUIC hub.
//!
//! Everything real lives in [`wxlike_server::start_stack`]; this binary is the
//! **service shell**: 解析配置、按平台惯例定路径、前台运行、接信号、优雅退出。
//! 业务逻辑一行都不在这里，也不为任何操作系统做特殊适配。
//!
//! 为什么前台运行：launchd 与 systemd 都要求进程在前台，由它们监控生命周期。
//! 自己 fork 到后台反而会让 supervisor 以为服务已退出（反复拉起或直接放弃）。
//! 同理不需要 PID 文件——supervisor 自己知道 PID。
//!
//! 配置（全部走环境变量，容器/systemd/launchd 三处给法一致）：
//!   WXLIKE_HUB_ID        hub 标识，默认 "hub-1"
//!   WXLIKE_MQ            "memory"（默认）| "redis"（需 --features redis）
//!   WXLIKE_REDIS_URL     默认 redis://127.0.0.1:6379
//!   WXLIKE_DB            SQLite 路径；默认 <平台数据目录>/wxlike/wxlike.db（":memory:" 允许）
//!   WXLIKE_MEDIA_DIR     媒体目录；默认 <平台数据目录>/wxlike/media
//!   WXLIKE_MEDIA_SECRET  对象下载短期链接的 HMAC 密钥（#295 media 域）。
//!                        **必填**：没配 = 拒绝启动，除非显式 WXLIKE_DEV_INSECURE=1。
//!                        多实例部署必须是同一个值，否则 A 实例签的链接在 B 实例验不过。
//!   WXLIKE_HTTP          HTTP 绑定，默认 0.0.0.0:8099
//!   WXLIKE_QUIC          QUIC 绑定，默认 0.0.0.0:9443
//!   WXLIKE_SHUTDOWN_GRACE 排空在飞请求的上限秒数，默认 10
//!
//! 数据目录按平台惯例（`dirs` crate）：
//!   macOS   ~/Library/Application Support/wxlike
//!   Linux   ~/.local/share/wxlike
//!   Windows %APPDATA%\wxlike
//!
//! 自检：`wxlike-server --check` 打印解析后的全部配置后退出，不绑定端口。

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// 解析后的运行配置。`--check` 打印的就是它。
struct Config {
    hub_id: String,
    mq_mode: String,
    db_env: String,
    db_path: Option<String>,
    media_dir: PathBuf,
    http: String,
    quic: String,
    grace: Duration,
}

/// 平台数据目录下的 wxlike 目录。
fn data_dir() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join("wxlike"))
        .unwrap_or_else(|| PathBuf::from("data"))
}

fn resolve() -> Config {
    let base = data_dir();
    let db_env = std::env::var("WXLIKE_DB")
        .unwrap_or_else(|_| base.join("wxlike.db").to_string_lossy().to_string());
    let db_path = if db_env == ":memory:" {
        None
    } else {
        Some(db_env.clone())
    };
    let media_dir = std::env::var("WXLIKE_MEDIA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| base.join("media"));
    let grace = std::env::var("WXLIKE_SHUTDOWN_GRACE")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(10));
    Config {
        hub_id: std::env::var("WXLIKE_HUB_ID").unwrap_or_else(|_| "hub-1".into()),
        mq_mode: std::env::var("WXLIKE_MQ").unwrap_or_else(|_| "memory".into()),
        db_env,
        db_path,
        media_dir,
        http: std::env::var("WXLIKE_HTTP").unwrap_or_else(|_| "0.0.0.0:8099".into()),
        quic: std::env::var("WXLIKE_QUIC").unwrap_or_else(|_| "0.0.0.0:9443".into()),
        grace,
    }
}

/// 该 env 是否"没配"（未设置，或只给了空白）。空白不等于密钥——
/// `"   "` 会被平台归一化成随机值，靠它跑生产就是拿随机值当密钥。
fn secret_missing(key: &str) -> bool {
    std::env::var(key)
        .map(|v| v.trim().is_empty())
        .unwrap_or(true)
}

/// 开发放行的唯一开关：`WXLIKE_DEV_INSECURE=1`。
fn dev_insecure() -> bool {
    matches!(std::env::var("WXLIKE_DEV_INSECURE").as_deref(), Ok("1"))
}

/// 需要显式密钥的两处，口径一致：**没配就拒绝启动**。
///
/// * `WXLIKE_LIVE_INGEST_SECRET` —— 直播 ingest token 的 HMAC 密钥；
/// * `WXLIKE_MEDIA_SECRET`（#295 media 域挂载）—— 对象下载短期链接的 HMAC
///   密钥。没有它，签好的链接**重启即失效、多实例互不认**（A 实例签的链接在
///   B 实例上验不过），而单进程开发里一切"看起来正常"——正是最容易被漏掉的
///   那种坏。media 域装配处还另有一道长度闸（`media_mount::MIN_SECRET_BYTES`）。
fn check_secrets() -> anyhow::Result<()> {
    for (key, why) in [
        (
            "WXLIKE_LIVE_INGEST_SECRET",
            "否则任何人都能用已知密钥签直播 ingest token",
        ),
        (
            "WXLIKE_MEDIA_SECRET",
            "否则下载签名链接重启即失效、多实例互不认（media 域装配另有长度闸）",
        ),
    ] {
        if secret_missing(key) && !dev_insecure() {
            anyhow::bail!(
                "{key} 未设置：拒绝启动。
             公网部署必须配置它（{why}）；
             本地开发可设 WXLIKE_DEV_INSECURE=1 放行（会生成本次启动的随机密钥）。"
            );
        }
    }
    Ok(())
}

fn print_check(cfg: &Config) {
    println!("wxlike-server 配置自检（不绑定端口、不写库）");
    println!("  hub            {}", cfg.hub_id);
    println!("  MQ             {}", cfg.mq_mode);
    println!("  数据库         {}", cfg.db_env);
    println!("  媒体目录       {}", cfg.media_dir.display());
    println!("  HTTP 绑定      {}", cfg.http);
    println!("  QUIC 绑定      {}", cfg.quic);
    println!("  关闭排空上限   {}s", cfg.grace.as_secs());
    println!(
        "  TURN           {}",
        std::env::var("WXLIKE_TURN_URL").unwrap_or_else(|_| "（未配置）".into())
    );
    println!(
        "  STUN           {}",
        std::env::var("WXLIKE_STUN_URL").unwrap_or_else(|_| "（默认公共 STUN）".into())
    );
    println!(
        "  SFU 广告地址   {}",
        std::env::var("WXLIKE_SFU_ADVERTISE").unwrap_or_else(|_| "（自动探测）".into())
    );
    println!(
        "  直播推流密钥   {}",
        secret_status("WXLIKE_LIVE_INGEST_SECRET")
    );
    println!("  媒体签名密钥   {}", secret_status("WXLIKE_MEDIA_SECRET"));
}

/// `--check` 的密钥行：只报"配了没有 + 长度"，绝不回显密钥本身。
fn secret_status(key: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => format!("已设置（{} 字符）", v.len()),
        _ => "⚠️ 未设置——启动会被拒绝（开发用 WXLIKE_DEV_INSECURE=1）".to_string(),
    }
}

/// 信号监听器：**尽早安装**，之后才去启动服务。
///
/// 为什么必须先装再启动：注册之前到达的 SIGTERM 走默认动作，进程直接被杀
/// （退出码 None），看起来像"优雅关闭没生效"。CI 的 Linux 任务就是这么红的——
/// 测试看到 "HTTP on" 那行才发信号，而那行是在 `start_stack` 之后、注册之前
/// 打印的，中间有个窗口。macOS 上侥幸没踩到，Linux 上必踩。
///
/// macOS/Linux 下 **SIGTERM 才是主角**：`launchctl bootout`、`systemctl stop`、
/// `docker stop` 发的都是它。只接 SIGINT（Ctrl-C）等于"用 supervisor 停服务
/// 时必然硬杀"。
#[cfg(unix)]
struct Signals {
    term: Option<tokio::signal::unix::Signal>,
    int: Option<tokio::signal::unix::Signal>,
}

#[cfg(not(unix))]
struct Signals;

#[cfg(unix)]
impl Signals {
    fn install() -> Self {
        use tokio::signal::unix::{signal, SignalKind};
        let term = signal(SignalKind::terminate()).ok();
        let int = signal(SignalKind::interrupt()).ok();
        if term.is_none() && int.is_none() {
            tracing::warn!("SIGTERM/SIGINT 都注册失败，只能靠 ctrl_c");
        }
        Self { term, int }
    }

    async fn wait(&mut self) {
        match (self.term.as_mut(), self.int.as_mut()) {
            (Some(term), Some(int)) => {
                tokio::select! {
                    _ = term.recv() => tracing::info!("收到 SIGTERM"),
                    _ = int.recv() => tracing::info!("收到 SIGINT"),
                }
            }
            (Some(term), None) => {
                term.recv().await;
                tracing::info!("收到 SIGTERM");
            }
            (None, Some(int)) => {
                int.recv().await;
                tracing::info!("收到 SIGINT");
            }
            (None, None) => {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("收到 Ctrl-C");
            }
        }
    }
}

#[cfg(not(unix))]
impl Signals {
    fn install() -> Self {
        Self
    }

    async fn wait(&mut self) {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("收到 Ctrl-C");
    }
}

async fn run(cfg: Config) -> anyhow::Result<()> {
    // 先装信号，再启动：注册前收到的 SIGTERM 会直接杀进程
    let mut signals = Signals::install();

    // 密钥类 env 的硬闸：缺了就拒绝启动，而不是"警告后继续"。
    // 警告会被忽略，而"知道默认 key 就能签 ingest token / 就能算出下载链接签名"
    // 是真实可利用的洞。开发/CI 用 WXLIKE_DEV_INSECURE=1 显式放行
    // （届时退化成"本次启动的随机密钥"）。
    check_secrets()?;
    std::fs::create_dir_all(&cfg.media_dir)?;
    if let Some(path) = cfg.db_path.as_deref() {
        if let Some(dir) = std::path::Path::new(path).parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
    }

    println!("[wxlike-server] hub '{}' mq={}", cfg.hub_id, cfg.mq_mode);
    let stack = wxlike_server::start_stack(wxlike_server::StackOptions {
        hub_id: cfg.hub_id.clone(),
        db_path: cfg.db_path.clone(),
        mq_mode: cfg.mq_mode.clone(),
        http_bind: cfg.http.parse()?,
        quic_bind: cfg.quic.parse()?,
    })
    .await?;

    println!("[wxlike-server] QUIC hub on {}", stack.quic_addr);
    println!("[wxlike-server] platform db = {}", cfg.db_env);
    println!(
        "[wxlike-server] HTTP  on {}  (GET /health /endpoints /metrics, /api/*)",
        stack.http_addr
    );
    println!("[wxlike-server] 前台运行中，等待 SIGTERM/SIGINT");

    signals.wait().await;
    println!(
        "[wxlike-server] graceful shutdown 开始（排空上限 {}s）",
        cfg.grace.as_secs()
    );
    stack.shutdown(cfg.grace).await;
    println!("[wxlike-server] graceful shutdown 完成");
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
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

    let cfg = resolve();
    if std::env::args().any(|a| a == "--check") {
        print_check(&cfg);
        return ExitCode::SUCCESS;
    }

    match run(cfg).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("[wxlike-server] 启动失败: {err:#}");
            // 非零退出：launchd 的 KeepAlive.SuccessfulExit=false 与 systemd 的
            // Restart=always 都靠退出码区分"正常停止"与"崩了"。
            ExitCode::FAILURE
        }
    }
}
