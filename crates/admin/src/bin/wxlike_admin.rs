//! `wxlike-admin` 二进制：服务壳 + 账号管理子命令。
//!
//! 这里**不放业务逻辑**，只做：解析配置、按平台惯例定路径、前台运行、接信号、
//! 优雅退出。子命令（`account …`）直接操作 adminx 自己的账号库——建号这件事
//! 故意不给 HTTP 接口：一个能自造管理员的后台面板等于没有边界。
//!
//! 配置（与 `wxlike-server` 同一套风格，全走环境变量）：
//!   WXLIKE_ADMIN_TOKEN        **必填**，缺失拒绝启动（读面令牌）
//!   WXLIKE_ADMIN_BIND         默认 127.0.0.1:8090
//!   WXLIKE_ADMIN_DB           adminx 账号库，默认 <数据目录>/wxlike/adminx.db
//!   WXLIKE_DB                 平台库（只读打开），默认 <数据目录>/wxlike/wxlike.db
//!   WXLIKE_ADMIN_PLATFORM_TOKEN  转发用令牌，默认复用 WXLIKE_ADMIN_TOKEN
//!   WXLIKE_PLATFORM_URL       平台地址，默认 http://127.0.0.1:8099
//!   WXLIKE_ADMIN_REPORT_SLA_MS / WXLIKE_ADMIN_SESSION_TTL_MS / WXLIKE_ADMIN_SHUTDOWN_GRACE
//!
//! 自检：`wxlike-admin --check` 打印解析后的配置（不打印令牌）后退出。

use std::process::ExitCode;
use std::time::Duration;

use wxlike_admin::config::Config;
use wxlike_admin::rbac::Role;

fn die(msg: &str) -> ExitCode {
    eprintln!("[adminx] {msg}");
    ExitCode::FAILURE
}

/// 新口令从环境读，不从命令行参数读：argv 在 `ps` 与 shell history 里都是明文。
fn new_password_from_env() -> anyhow::Result<String> {
    let raw = std::env::var("WXLIKE_ADMIN_NEW_PASSWORD").map_err(|_| {
        anyhow::anyhow!("缺少 WXLIKE_ADMIN_NEW_PASSWORD：新口令只从环境变量收（argv 会被 ps 与 shell 历史记录）")
    })?;
    let pw = raw.trim_end_matches('\n').to_string();
    if pw.chars().count() < 10 {
        anyhow::bail!("WXLIKE_ADMIN_NEW_PASSWORD 太短：管理员口令至少 10 个字符");
    }
    Ok(pw)
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn print_help() {
    print!(
        "wxlike-admin — wxlike 运营后台（只读看板 + 审计检索 + 工单 + RBAC/MFA）\n\n\
         用法：\n\
         \x20 wxlike-admin                      起服务（需要 WXLIKE_ADMIN_TOKEN）\n\
         \x20 wxlike-admin --check              打印解析后的配置后退出（不打印令牌）\n\
         \x20 wxlike-admin rand-token           生成一个 64 字符随机令牌\n\
         \x20 wxlike-admin account add --name N [--role viewer|moderator|superadmin]\n\
         \x20     新口令从 WXLIKE_ADMIN_NEW_PASSWORD 收\n\
         \x20 wxlike-admin account list\n\
         \x20 wxlike-admin account passwd --name N\n\
         \x20 wxlike-admin account disable|enable --name N\n\
         \x20 wxlike-admin account totp-init --name N      打印密钥与 otpauth URI\n\
         \x20 wxlike-admin account totp-confirm --name N --code 123456\n\n\
         环境变量见 crates/admin/src/bin/wxlike_admin.rs 的模块注释。\n"
    );
}

/// 子命令一律先解析配置（`--check` 之外不猜默认令牌，所以 account 也要带
/// `WXLIKE_ADMIN_TOKEN`：同一份"必须显式配置"的纪律）。
fn account_command(args: &[String]) -> anyhow::Result<()> {
    let Some(action) = args.first().map(|s| s.as_str()) else {
        anyhow::bail!("account 后面要跟动作：add | list | passwd | disable | enable | totp-init | totp-confirm");
    };
    let cfg = Config::from_env()?;
    if action == "list" {
        let store = wxlike_admin::AccountStore::open(&cfg.accounts_db)?;
        let rows = store.list()?;
        if rows.is_empty() {
            println!(
                "{} 里还没有账号：任何写操作都不可用",
                cfg.accounts_db.display()
            );
            return Ok(());
        }
        println!(
            "{:<24} {:<12} {:<8} {:<8} 更新于",
            "账号", "角色", "MFA", "停用"
        );
        for a in rows {
            println!(
                "{:<24} {:<12} {:<8} {:<8} {}",
                a.name,
                a.role.as_str(),
                if a.mfa_enabled {
                    "已开"
                } else if a.totp_provisioned {
                    "待确认"
                } else {
                    "未绑"
                },
                if a.disabled { "是" } else { "否" },
                a.updated_at
            );
        }
        return Ok(());
    }
    let name = flag(&args[1..], "--name")
        .ok_or_else(|| anyhow::anyhow!("account {action} 需要 --name <账号名>"))?;
    let store = wxlike_admin::AccountStore::open(&cfg.accounts_db)?;
    match action {
        "add" => {
            let role = flag(&args[1..], "--role").unwrap_or_else(|| "viewer".into());
            let role = Role::parse(&role).ok_or_else(|| {
                anyhow::anyhow!("角色 {role:?} 不认识（viewer | moderator | superadmin）")
            })?;
            let pw = new_password_from_env()?;
            let acc = store.add(&name, &pw, role)?;
            println!(
                "已创建 {}（角色 {}）。接下来：\n  1) account totp-init --name {}\n  2) account totp-confirm --name {} --code <6 位码>\n  没开 MFA 的账号只能读，任何写操作都会被拒。",
                acc.name, acc.role.as_str(), acc.name, acc.name
            );
        }
        "passwd" => {
            let pw = new_password_from_env()?;
            let acc = store.set_password(&name, &pw)?;
            println!("已更新 {} 的口令，并且吊销了它的全部会话", acc.name);
        }
        "disable" => {
            let acc = store.set_disabled(&name, true)?;
            let revoked = store.revoke_sessions(&acc.id)?;
            println!("已停用 {}（吊销 {revoked} 个会话）", acc.name);
        }
        "enable" => {
            let acc = store.set_disabled(&name, false)?;
            println!("已启用 {}", acc.name);
        }
        "totp-init" => {
            let secret = store.totp_enroll(&name)?;
            println!("把下面这个密钥录进验证器 App（只打印这一次）：\n  {secret}\n  otpauth://totp/wxlike-admin:{name}?secret={secret}&issuer=wxlike-admin");
            println!("录好后执行：account totp-confirm --name {name} --code <当前码>");
        }
        "totp-confirm" => {
            let code = flag(&args[1..], "--code")
                .ok_or_else(|| anyhow::anyhow!("totp-confirm 需要 --code <6 位动态码>"))?;
            store.totp_confirm(&name, &code)?;
            println!(
                "{} 的 MFA 已激活。重新登录（带码）才会拿到可写的会话。",
                name
            );
        }
        other => anyhow::bail!("未知子命令 account {other}"),
    }
    Ok(())
}

/// SIGTERM/SIGINT。与 `wxlike-server` 同理：supervisor 发的是 SIGTERM，
/// 只接 Ctrl-C 等于"用 systemctl stop 停服务时必然硬杀"。
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("注册 SIGTERM");
    let mut int = signal(SignalKind::interrupt()).expect("注册 SIGINT");
    tokio::select! {
        _ = term.recv() => tracing::info!("收到 SIGTERM"),
        _ = int.recv() => tracing::info!("收到 SIGINT"),
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("收到 Ctrl-C");
}

async fn run(cfg: Config) -> anyhow::Result<()> {
    let app = wxlike_admin::App::open(&cfg)?;
    let missing = app.read().missing_tables()?;
    if !missing.is_empty() {
        anyhow::bail!(
            "平台库缺表 {missing:?}：adminx 不建表也不改表（schema 归串行轨道），\n  \
             请先用同版本的 wxlike-server 打开过一次这个库，或检查 WXLIKE_DB 是否指错。"
        );
    }
    if !app.accounts().has_any_account()? {
        eprintln!(
            "[adminx] 提示：{} 里还没有管理员账号，写面不可用（读面正常）。\n  \
             建号：WXLIKE_ADMIN_NEW_PASSWORD=… wxlike-admin account add --name ops --role superadmin",
            cfg.accounts_db.display()
        );
    }
    let (addr, _handle) = app.bind_and_serve(&cfg.bind).await?;
    println!(
        "[adminx] 只读看板 http://{addr}/  （平台库 {}，打开方式 {}）",
        cfg.platform_db.display(),
        app.read().open_mode()
    );
    println!(
        "[adminx] 写操作转发到 {}（x-admin-token 已配置）",
        cfg.platform_url
    );
    println!("[adminx] 前台运行中，等待 SIGTERM/SIGINT");

    shutdown_signal().await;
    println!(
        "[adminx] graceful shutdown 开始（排空上限 {}s）",
        cfg.shutdown_grace.as_secs()
    );
    // 面板没有"在飞的写"要排空（写都转发给平台，由平台自己保证落库），
    // 这里只给监听器一个关闭窗口，让 keep-alive 连接自然断开。
    tokio::time::sleep(Duration::from_millis(50)).await;
    println!("[adminx] graceful shutdown 完成");
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    let filter = std::env::var("WXLIKE_ADMIN_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(&filter).unwrap_or_else(|_| {
                eprintln!("[adminx] bad log filter {filter:?}, falling back to info");
                tracing_subscriber::EnvFilter::new("info")
            }),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("--help") | Some("-h") | Some("help") => {
            print_help();
            return ExitCode::SUCCESS;
        }
        Some("rand-token") => {
            // 用平台同一个随机源，别在这里自己攒熵
            println!("{}", wxlike_platform::random_hex(32));
            return ExitCode::SUCCESS;
        }
        Some("account") => {
            if let Err(err) = account_command(&args[1..]) {
                return die(&format!("account 子命令失败: {err:#}"));
            }
            return ExitCode::SUCCESS;
        }
        _ => {}
    }

    let cfg = match Config::from_env() {
        Ok(cfg) => cfg,
        Err(err) => return die(&format!("配置错误: {err}")),
    };
    if args.iter().any(|a| a == "--check") {
        println!("wxlike-admin 配置自检（不绑定端口、不写库）");
        print!("{}", cfg.describe());
        // 自检也验一遍平台库能不能只读打开：路径写错要在部署时就发现
        return match wxlike_admin::PlatformRead::open(&cfg.platform_db) {
            Ok(read) => {
                println!("  平台库可读      是（打开方式 {}）", read.open_mode());
                let missing = read.missing_tables().unwrap_or_default();
                if missing.is_empty() {
                    println!("  表齐全          是");
                } else {
                    println!("  表齐全          否，缺 {missing:?}");
                }
                match wxlike_admin::AccountStore::open(&cfg.accounts_db) {
                    Ok(store) => {
                        let n = store.list().map(|v| v.len()).unwrap_or_default();
                        println!("  账号库可写      是（{n} 个管理员账号）");
                    }
                    Err(err) => println!("  账号库可写      否: {err}"),
                }
                ExitCode::SUCCESS
            }
            Err(err) => die(&format!("平台库打不开: {err}")),
        };
    }

    match run(cfg).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => die(&format!("启动失败: {err:#}")),
    }
}
