//! 视频控制面的准入策略（issue #7 / #8）。
//!
//! 两件事被放在一起，因为它们回答的是同一个问题：**这个请求凭什么值得服务端
//! 花资源**。
//!
//! 1. **谁在调用**（[`VideoAuth`]）。视频 crate 不依赖平台，也不该依赖——
//!    但"匿名上传 500 MiB、匿名重试别人的作业"必须消失。所以这里只定义
//!    "从请求头认出调用者"的钩子，装配层（`server`）用平台会话表实现它。
//!    `auth = None` 的语义是**拒绝**，不是"不校验"（与 hub 的 `MqHubOptions.auth`
//!    同一约定）。
//! 2. **回调能发去哪**（[`CallbackPolicy`]）。客户端给的 `callback_url` 由
//!    服务端网络直接 POST，只校验 `http(s)://` 前缀等于把 SSRF 做成产品特性。
//!    默认策略是**不接受终端用户给的地址**，只认管理员配置的固定目标。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use wxlike_api::ApiError;

use crate::VideoState;

/// 从请求头认出调用者；认不出就是 401。
///
/// **没有认证器时也拒绝**（fail closed）：装配层忘了装认证器是"控制面不可用"，
/// 不是"谁都能用"。
pub fn require_caller(policy: &VideoPolicy, headers: &HeaderMap) -> Result<String, ApiError> {
    let Some(auth) = policy.auth.as_ref() else {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "视频控制面未启用：服务端没有配置认证器",
        ));
    };
    auth.caller(headers)
        .ok_or_else(|| ApiError::unauthorized("需要登录后才能上传或操作转码作业"))
}

/// 控制面的前置闸门：**在解析请求体之前**把匿名请求挡掉。
///
/// 只靠 handler 里的检查是不够的：axum 先跑 extractor 再进 handler，
/// 一个匿名的 `POST /api/video/jobs` 会在 `Multipart` 解析时先回 400，
/// 而且真要是有个 500 MiB 的合法 multipart，服务端已经把它读进内存了。
pub async fn guard(
    State(state): State<VideoState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    require_caller(state.store.policy(), req.headers())?;
    Ok(next.run(req).await)
}

/// 从请求头认出调用者。`None` = 没有有效凭据。
pub trait VideoAuth: Send + Sync + 'static {
    fn caller(&self, headers: &HeaderMap) -> Option<String>;
}

/// 控制面（上传、建作业、查作业、重试）的准入与配额。
pub struct VideoPolicy {
    /// 认证器。`None` = 控制面一律拒绝。
    pub auth: Option<Arc<dyn VideoAuth>>,
    /// 单个用户同时排队/在飞的作业数上限。
    pub max_user_jobs: u32,
    /// 单个用户排队中的源字节上限。
    pub max_user_bytes: u64,
    /// 全站排队中的源字节上限（磁盘预算）。
    pub max_total_bytes: u64,
    /// 出站回调目标策略。
    pub callbacks: CallbackPolicy,
}

/// 默认配额：一个人最多 4 个排队作业、2 GiB；全站 32 GiB。
///
/// 这些数不是拍脑袋的"安全值"，是**能被 ffmpeg 吃掉的量级**：一个 500 MiB
/// 源文件转码时占的内存与 CPU 远大于磁盘，4 个并发已经足够把一台小机器压满。
pub const DEFAULT_MAX_USER_JOBS: u32 = 4;
pub const DEFAULT_MAX_USER_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024 * 1024;

impl Default for VideoPolicy {
    fn default() -> Self {
        Self {
            auth: None,
            max_user_jobs: DEFAULT_MAX_USER_JOBS,
            max_user_bytes: DEFAULT_MAX_USER_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            callbacks: CallbackPolicy::deny_user_urls(),
        }
    }
}

impl VideoPolicy {
    /// 单进程/测试用：把每个请求都当成同一个本地用户。
    ///
    /// 名字里带 `trusted` 是刻意的——它只该出现在"前面已经有别的组件做了认证"
    /// 或者测试里，绝不能出现在公网部署的默认配置里。
    pub fn trusted_local(user: &str) -> Self {
        Self {
            auth: Some(Arc::new(TrustedLocal(user.to_string()))),
            ..Self::default()
        }
    }

    /// 换一个认证器。
    pub fn with_auth(mut self, auth: Arc<dyn VideoAuth>) -> Self {
        self.auth = Some(auth);
        self
    }

    /// 换回调策略。
    pub fn with_callbacks(mut self, callbacks: CallbackPolicy) -> Self {
        self.callbacks = callbacks;
        self
    }
}

/// 把所有请求都算作同一个用户（见 [`VideoPolicy::trusted_local`]）。
struct TrustedLocal(String);

impl VideoAuth for TrustedLocal {
    fn caller(&self, _headers: &HeaderMap) -> Option<String> {
        Some(self.0.clone())
    }
}

/// 出站回调的目标策略。
///
/// 默认**不接受**终端用户给的 URL：回调是服务端发起的出站请求，放开它等于
/// 把服务端的网络位置借给任何能提交作业的人（issue #8）。
#[derive(Clone, Debug, Default)]
pub struct CallbackPolicy {
    /// 允许的目标主机（小写、不含端口）。空 = 不允许用户给地址。
    allow_hosts: Vec<String>,
    /// 是否允许解析到私网/回环地址。**只该在开发与测试里打开**。
    allow_private: bool,
    /// 出站回调带的共享令牌（`x-wxlike-video-token`）。
    ///
    /// 平台侧的 `/api/video/callback` 是"未配置令牌 = 接口关闭"的口径；
    /// 视频侧不带上它就是"回调永远 401"——两边必须用同一个值。
    token: Option<String>,
}

impl CallbackPolicy {
    /// 不接受任何用户给的地址（生产默认）。
    pub fn deny_user_urls() -> Self {
        Self {
            allow_hosts: Vec::new(),
            allow_private: false,
            token: None,
        }
    }

    /// 只接受这些主机（管理员配置的固定目标）。
    pub fn allow_hosts(hosts: impl IntoIterator<Item = String>) -> Self {
        Self {
            allow_hosts: hosts
                .into_iter()
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
            allow_private: false,
            token: None,
        }
    }

    /// 回调共享令牌（与平台的 `WXLIKE_VIDEO_CALLBACK_TOKEN` 同值）。
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token.filter(|t| !t.trim().is_empty());
        self
    }

    /// 发送回调时该带的令牌。
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// 允许私网/回环（测试、本地联调）。生产不要打开。
    pub fn allow_private(mut self, yes: bool) -> Self {
        self.allow_private = yes;
        self
    }

    /// 客户端给的地址能不能用。`Ok` 返回归一化后的 URL。
    ///
    /// 这里只做**静态**检查（协议、主机白名单、主机字面量是不是私网）；
    /// DNS 解析与"解析出来的地址是不是公网"由 [`Self::resolve`] 在提交与
    /// 发送前各查一次——只查一次的话，攻击者用 DNS 重绑定就能绕过。
    pub fn check(&self, url: &str) -> Result<String, String> {
        let parsed = parse_http_url(url)?;
        if self.allow_hosts.is_empty() {
            return Err("本服务不接受客户端指定回调地址".to_string());
        }
        if !self.allow_hosts.iter().any(|h| h == &parsed.host) {
            return Err(format!("回调地址不在允许列表内：{}", parsed.host));
        }
        if !self.allow_private {
            if let Ok(ip) = parsed.host.parse::<IpAddr>() {
                ensure_public(ip)?;
            }
        }
        Ok(parsed.normalized())
    }

    /// 发送前再解析一次并校验（DNS 重绑定防线）。
    pub async fn resolve(&self, url: &str) -> Result<(), String> {
        let parsed = parse_http_url(url)?;
        let port = parsed.port.unwrap_or(if parsed.tls {
            DEFAULT_HTTPS_PORT
        } else {
            DEFAULT_HTTP_PORT
        });
        // 主机本身就是 IP 时不必查 DNS
        if let Ok(ip) = parsed.host.parse::<IpAddr>() {
            return if self.allow_private {
                Ok(())
            } else {
                ensure_public(ip)
            };
        }
        let addrs: Vec<IpAddr> = tokio::net::lookup_host((parsed.host.as_str(), port))
            .await
            .map_err(|e| format!("回调地址解析失败：{e}"))?
            .map(|sa| sa.ip())
            .collect();
        if addrs.is_empty() {
            return Err("回调地址解析不到任何地址".to_string());
        }
        if self.allow_private {
            return Ok(());
        }
        // **任何一个**解析结果是私网就整体拒绝：混合解析是重绑定的常见形态
        for ip in addrs {
            ensure_public(ip)?;
        }
        Ok(())
    }
}

/// 没写端口时的默认端口。
const DEFAULT_HTTPS_PORT: u16 = 443;
const DEFAULT_HTTP_PORT: u16 = 80;

/// 解析后的 HTTP(S) URL（只保留出站回调需要的部分）。
#[derive(Debug, PartialEq, Eq)]
struct ParsedUrl {
    tls: bool,
    host: String,
    port: Option<u16>,
    path: String,
}

impl ParsedUrl {
    fn normalized(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        match self.port {
            Some(port) => format!("{scheme}://{}:{port}{}", self.host, self.path),
            None => format!("{scheme}://{}{}", self.host, self.path),
        }
    }
}

/// 极简 URL 解析：只接受 `http(s)://host[:port][/path][?query]`。
///
/// 不引 `url` crate 是为了不给 video 加依赖；也正因为解析器小，**拒绝**比
/// "尽量理解"更安全：看不懂就报错。
fn parse_http_url(url: &str) -> Result<ParsedUrl, String> {
    let url = url.trim();
    let (tls, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err("回调地址必须以 http:// 或 https:// 开头".to_string());
    };
    if rest.is_empty() {
        return Err("回调地址缺少主机".to_string());
    }
    let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..split];
    let tail = &rest[split..];
    if authority.contains('@') {
        return Err("回调地址不能带用户信息".to_string());
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            let port: u16 = port.parse().map_err(|_| "回调地址端口不合法".to_string())?;
            (host, Some(port))
        }
        Some(_) => return Err("回调地址端口不合法".to_string()),
        None => (authority, None),
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return Err("回调地址缺少主机".to_string());
    }
    if host.contains([' ', '\t', '\n', '\r']) {
        return Err("回调地址主机含空白字符".to_string());
    }
    // IPv6 字面量必须带方括号，否则上面的 `rsplit_once(':')` 会把它切坏
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(&host)
        .to_string();
    let path = if tail.is_empty() {
        "/".to_string()
    } else if tail.starts_with('?') {
        format!("/{tail}")
    } else {
        tail.split('#').next().unwrap_or("/").to_string()
    };
    Ok(ParsedUrl {
        tls,
        host,
        port,
        path,
    })
}

/// 只允许公网单播地址。
fn ensure_public(ip: IpAddr) -> Result<(), String> {
    let bad = match ip {
        IpAddr::V4(v4) => !is_public_v4(v4),
        IpAddr::V6(v6) => !is_public_v6(v6),
    };
    if bad {
        return Err(format!("回调地址指向非公网地址：{ip}"));
    }
    Ok(())
}

/// CGNAT 100.64.0.0/10：运营商内网，从云主机打过去等于打内网。
const CGNAT_SECOND_OCTET: std::ops::Range<u8> = 64..128;
/// 保留段 240.0.0.0/4 的起点。
const RESERVED_FIRST_OCTET: u8 = 240;

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && CGNAT_SECOND_OCTET.contains(&b))
        || a >= RESERVED_FIRST_OCTET)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    // v4-mapped（::ffff:a.b.c.d）按内嵌的 v4 判：这是绕过 v4 检查的老手法
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let seg = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // unique local fc00::/7
        || (seg[0] & 0xfe00) == 0xfc00
        // link-local fe80::/10
        || (seg[0] & 0xffc0) == 0xfe80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes() {
        let p = parse_http_url("https://Example.COM/hook").unwrap();
        assert_eq!(p.host, "example.com");
        assert_eq!(p.normalized(), "https://example.com/hook");
        let p = parse_http_url("http://127.0.0.1:8080").unwrap();
        assert_eq!(p.port, Some(8080));
        assert_eq!(p.normalized(), "http://127.0.0.1:8080/");
        assert!(parse_http_url("ftp://example.com").is_err());
        assert!(parse_http_url("http://user:pw@example.com").is_err());
        assert!(parse_http_url("http://[::1]:9000/x").is_ok());
    }

    #[test]
    fn private_addresses_are_refused() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.5.4",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(ensure_public(ip).is_err(), "{ip} 必须被拒绝");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(ensure_public(ip).is_ok(), "{ip} 应当放行");
        }
    }

    #[test]
    fn default_policy_refuses_every_user_url() {
        let policy = CallbackPolicy::deny_user_urls();
        let err = policy.check("https://example.com/hook").unwrap_err();
        assert!(err.contains("不接受"), "{err}");
    }

    #[test]
    fn allowlist_only_matches_listed_hosts() {
        let policy = CallbackPolicy::allow_hosts(["hooks.example.com".to_string()]);
        assert!(policy.check("https://hooks.example.com/x").is_ok());
        assert!(policy.check("https://evil.example.com/x").is_err());
        // 大小写与前缀不是绕过手段
        assert!(policy.check("https://HOOKS.example.com/x").is_ok());
        assert!(policy.check("https://hooks.example.com.evil.io/x").is_err());
    }

    #[test]
    fn ip_literals_are_checked_even_when_allowlisted() {
        let policy = CallbackPolicy::allow_hosts(["127.0.0.1".to_string()]);
        let err = policy.check("http://127.0.0.1:9000/hook").unwrap_err();
        assert!(err.contains("非公网"), "{err}");
        // 显式放开（测试/本地）才允许
        let policy = policy.allow_private(true);
        assert!(policy.check("http://127.0.0.1:9000/hook").is_ok());
    }
}
