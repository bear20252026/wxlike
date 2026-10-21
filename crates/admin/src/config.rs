//! 运行配置：全部走环境变量，与 `wxlike-server` 同一套惯例（容器 / systemd /
//! launchd 三处给法一致）。
//!
//! 只有一条硬规则值得单独说明：**缺 `WXLIKE_ADMIN_TOKEN` 就拒绝启动**。
//! 这个后台看的是全体用户、群、审计与工单，是本项目里权限最高的读面。
//! "没配 token 就默认放开"会让它在第一次部署时恰好以最大权限跑起来——
//! 与推流密钥被审计整改的是同一类错误（见 `805e8cb`），所以这里不留默认值。
//!
//! 解析函数收成一个 `get` 闭包而不是直接读 `std::env`：环境变量是进程全局的，
//! 直接读就没法并行测试（用例之间会互相踩）。

use std::path::{Path, PathBuf};
use std::time::Duration;

/// 默认监听地址。运营后台默认只绑回环：它是给本机/内网运维用的，
/// 要公网必须显式改 `WXLIKE_ADMIN_BIND`（改的那一刻就该上反代 + TLS）。
pub const DEFAULT_BIND: &str = "127.0.0.1:8090";

/// 平台 HTTP 地址的默认值（写操作只走它）。
pub const DEFAULT_PLATFORM_URL: &str = "http://127.0.0.1:8099";

/// 后台会话默认 8 小时：够一个班次，又不至于让令牌长期有效。
pub const DEFAULT_SESSION_TTL_MS: u64 = 8 * 60 * 60 * 1000;

/// 平台数据目录下的 wxlike 目录（与 `wxlike-server` 的解析一致）。
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join("wxlike"))
        .unwrap_or_else(|| PathBuf::from("data"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// 平台库（**只读**打开）。
    pub platform_db: PathBuf,
    /// adminx 自己的账号 / 会话 / 留痕库（唯一允许 adminx 写的文件）。
    pub accounts_db: PathBuf,
    /// 读面令牌：`Authorization: Bearer <token>`。
    pub token: String,
    /// 转发到平台管理面时用的 `x-admin-token`。默认与 [`Config::token`] 相同，
    /// 但允许分开配——轮换后台登录令牌时不该被迫同时轮换平台令牌。
    pub platform_token: String,
    /// 平台 HTTP 地址。
    pub platform_url: String,
    pub bind: String,
    pub report_sla_ms: u64,
    pub session_ttl_ms: u64,
    pub shutdown_grace: Duration,
}

/// 配置错误的三种形状，消息里都带"该怎么修"。
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(
        "WXLIKE_ADMIN_TOKEN 未设置或为空：拒绝启动。这个后台能读全体用户/群/审计/工单，\
         不允许无令牌运行。生成一个：openssl rand -hex 32（或 `wxlike-admin rand-token`）"
    )]
    MissingToken,
    #[error(
        "WXLIKE_DB 不能是 \":memory:\"：adminx 是独立进程，内存库只对 wxlike-server 自己可见，\
         读到的永远是空。请指向平台真的 SQLite 文件。"
    )]
    InMemoryPlatformDb,
    #[error("环境变量 {0} 的值无法解析：{1}")]
    BadValue(String, String),
}

/// 空白视作未设置：`TOKEN=""` 与 `TOKEN="   "` 都不是可用凭据。
fn value(get: &dyn Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    get(key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn number(
    get: &dyn Fn(&str) -> Option<String>,
    key: &str,
    default: u64,
) -> Result<u64, ConfigError> {
    match value(get, key) {
        None => Ok(default),
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|_| ConfigError::BadValue(key.to_string(), format!("{raw:?} 不是非负整数"))),
    }
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::resolve(&|k| std::env::var(k).ok(), &data_dir())
    }

    /// 真正的解析。`base`（平台数据目录）参数化，是为了让"未设置时落到哪个
    /// 文件"这件事可断言——否则默认路径这整段逻辑没有任何测试覆盖。
    pub fn resolve(get: &dyn Fn(&str) -> Option<String>, base: &Path) -> Result<Self, ConfigError> {
        let Some(token) = value(get, "WXLIKE_ADMIN_TOKEN") else {
            return Err(ConfigError::MissingToken);
        };
        let platform_db_env = value(get, "WXLIKE_DB")
            .unwrap_or_else(|| base.join("wxlike.db").to_string_lossy().into());
        if platform_db_env == ":memory:" {
            return Err(ConfigError::InMemoryPlatformDb);
        }
        let accounts_db = value(get, "WXLIKE_ADMIN_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|| base.join("adminx.db"));
        let platform_token =
            value(get, "WXLIKE_ADMIN_PLATFORM_TOKEN").unwrap_or_else(|| token.clone());
        Ok(Self {
            platform_db: PathBuf::from(platform_db_env),
            accounts_db,
            token,
            platform_token,
            platform_url: value(get, "WXLIKE_PLATFORM_URL")
                .unwrap_or_else(|| DEFAULT_PLATFORM_URL.into()),
            bind: value(get, "WXLIKE_ADMIN_BIND").unwrap_or_else(|| DEFAULT_BIND.into()),
            report_sla_ms: number(
                get,
                "WXLIKE_ADMIN_REPORT_SLA_MS",
                wxlike_report::DEFAULT_SLA_MS,
            )?,
            session_ttl_ms: number(get, "WXLIKE_ADMIN_SESSION_TTL_MS", DEFAULT_SESSION_TTL_MS)?,
            shutdown_grace: Duration::from_secs(number(get, "WXLIKE_ADMIN_SHUTDOWN_GRACE", 10)?),
        })
    }

    /// `--check` 用：打印解析结果，但**不打令牌本身**（只说长度与来源）。
    pub fn describe(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "  平台库(只读)    {}", self.platform_db.display());
        let _ = writeln!(out, "  账号库          {}", self.accounts_db.display());
        let _ = writeln!(out, "  监听            {}", self.bind);
        let _ = writeln!(out, "  平台地址        {}", self.platform_url);
        let _ = writeln!(
            out,
            "  后台令牌        已设置（{} 字符，不打印）",
            self.token.chars().count()
        );
        let _ = writeln!(
            out,
            "  平台令牌        {}",
            if self.platform_token == self.token {
                "与后台令牌相同"
            } else {
                "单独配置（WXLIKE_ADMIN_PLATFORM_TOKEN）"
            }
        );
        let _ = writeln!(
            out,
            "  工单 SLA        {} ms · 会话 TTL {} ms · 排空上限 {} s",
            self.report_sla_ms,
            self.session_ttl_ms,
            self.shutdown_grace.as_secs()
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// 用一张表当环境视图：可并行、可断言默认值，不碰真进程环境。
    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn resolve(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let env = map(pairs);
        let get = move |k: &str| env.get(k).cloned();
        Config::resolve(&get, Path::new("/data/wxlike"))
    }

    #[test]
    fn missing_token_refuses_to_start() {
        let err = resolve(&[("WXLIKE_ADMIN_DB", "/tmp/none.db")]).unwrap_err();
        assert!(matches!(err, ConfigError::MissingToken), "{err}");
        // 消息里必须带"怎么修"，否则运维只能去翻代码
        assert!(err.to_string().contains("rand -hex"));
    }

    #[test]
    fn blank_token_is_not_a_token() {
        assert!(matches!(
            resolve(&[("WXLIKE_ADMIN_TOKEN", "   ")]).unwrap_err(),
            ConfigError::MissingToken
        ));
    }

    #[test]
    fn in_memory_platform_db_is_refused() {
        // 独立进程读内存库 = 永远读到空表，这是"看着能用、结果全空"的教科书样本
        let err =
            resolve(&[("WXLIKE_ADMIN_TOKEN", "t0ken"), ("WXLIKE_DB", ":memory:")]).unwrap_err();
        assert!(matches!(err, ConfigError::InMemoryPlatformDb), "{err}");
    }

    #[test]
    fn numbers_are_validated_not_guessed() {
        let err = resolve(&[
            ("WXLIKE_ADMIN_TOKEN", "t0ken"),
            ("WXLIKE_ADMIN_REPORT_SLA_MS", "yesterday"),
        ])
        .unwrap_err();
        assert!(
            matches!(err, ConfigError::BadValue(ref k, _) if k == "WXLIKE_ADMIN_REPORT_SLA_MS"),
            "{err}"
        );
    }

    #[test]
    fn defaults_are_explicit() {
        let cfg = resolve(&[("WXLIKE_ADMIN_TOKEN", "t0ken")]).unwrap();
        assert_eq!(cfg.bind, DEFAULT_BIND);
        assert_eq!(cfg.platform_url, DEFAULT_PLATFORM_URL);
        assert_eq!(cfg.report_sla_ms, wxlike_report::DEFAULT_SLA_MS);
        assert_eq!(cfg.session_ttl_ms, DEFAULT_SESSION_TTL_MS);
        // 未设 WXLIKE_DB 时落在平台数据目录，与 wxlike-server 同一个文件
        assert_eq!(cfg.platform_db, PathBuf::from("/data/wxlike/wxlike.db"));
        // 账号库是 adminx 自己的文件，与平台库同目录但不同名
        assert_eq!(cfg.accounts_db, PathBuf::from("/data/wxlike/adminx.db"));
        let relocated = resolve(&[
            ("WXLIKE_ADMIN_TOKEN", "t0ken"),
            ("WXLIKE_ADMIN_DB", "/srv/adminx.db"),
        ])
        .unwrap();
        assert_eq!(relocated.accounts_db, PathBuf::from("/srv/adminx.db"));
        // 轮换后台令牌不该被迫轮换平台令牌
        assert_eq!(cfg.platform_token, "t0ken");
        let split = resolve(&[
            ("WXLIKE_ADMIN_TOKEN", "t0ken"),
            ("WXLIKE_ADMIN_PLATFORM_TOKEN", "p1atform"),
        ])
        .unwrap();
        assert_eq!(split.platform_token, "p1atform");
        // describe 不能把任何一个令牌打出来
        let text = split.describe();
        assert!(
            !text.contains("t0ken") && !text.contains("p1atform"),
            "{text}"
        );
    }
}
