//! 错误分类：把 `crates/client` 的字符串错误翻译成移动端能分支的三类。
//!
//! 为什么这个判断必须在 Rust：客户端语言里如果各写一份"这个错误算网络问题吗"，
//! 三端就会在**同一个失败**上做出不同决定——桌面端重试、移动端提示"服务器错误"、
//! 网页端静默忽略。而"要不要重试"直接决定 outbox 的行为（`SendFailure::retryable`）。
//!
//! 分类依据只有两个，都来自被调用方本身的写法：
//! * `crates/cmd` 的 `Deny` 文案（权限先于网络，见 `Policy::authorize`）；
//! * `crates/client::Http` 拼出的 `"… (状态码)"` 后缀与 ureq 的传输层错误。

/// 移动端只需要知道"该怪谁"：网络、凭据、权限，其余归到 `Unexpected`。
///
/// 字段叫 `detail` 不叫 `message`：UniFFI 的 Kotlin 后端会让错误类型继承
/// `java.lang.Exception`，而 `Exception.message` 已经存在——同名字段在生成代码里
/// 直接编不过（`overload resolution ambiguity`），这是 B1 的 Kotlin 验收抓到的。
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum WxlikeError {
    /// 连不上、超时、断网。**值得重试**——outbox 会自己退避。
    #[error("network: {detail}")]
    Network { detail: String },
    /// 没登录 / token 过期（401）。重试无意义，要重新登录。
    #[error("unauthorized: {detail}")]
    Unauthorized { detail: String },
    /// 服务端 403，或本地 `Policy` 直接拒绝。
    #[error("forbidden: {detail}")]
    Permission { detail: String },
    /// 其余（4xx 参数错误、5xx、解码失败、本地库错误）。
    ///
    /// 卡片要求"网络/鉴权/权限三类"，这里加第四类而不是把 422/500 塞进
    /// `Network`：塞进去就等于让 outbox 去重试一条**永远不会成功**的消息，
    /// 用户等 30 秒后退避用尽才看到失败。
    #[error("unexpected: {detail}")]
    Unexpected { detail: String },
}

impl WxlikeError {
    fn wrap(detail: &str, make: impl FnOnce(String) -> Self) -> Self {
        make(detail.to_string())
    }

    pub fn network(detail: impl Into<String>) -> Self {
        Self::Network {
            detail: detail.into(),
        }
    }

    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::Unauthorized {
            detail: detail.into(),
        }
    }

    pub fn permission(detail: impl Into<String>) -> Self {
        Self::Permission {
            detail: detail.into(),
        }
    }

    pub fn unexpected(detail: impl Into<String>) -> Self {
        Self::Unexpected {
            detail: detail.into(),
        }
    }

    /// 该不该重试。只有网络类会——其余重试只是浪费用户的电量和退避时间。
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Network { .. })
    }
}

/// 权限拒绝的文案特征，全部来自 `crates/cmd::policy::Deny` 的 `Display`。
const DENY_MARKERS: &[&str] = &[
    "is not allowed",
    "unknown command",
    "needs a `conv` param",
    "write access was not granted",
    "destructive access was not granted",
    "this session is read-only",
];

/// 传输层错误的文案特征。
///
/// **只用 ureq 自己产生的字符串，不用操作系统的。** Windows 会把
/// `Connection refused` 本地化成"由于目标计算机积极拒绝，无法连接"（测试里
/// 真抓到过），所以匹配的是 `ErrorKind` 的 Display——它在任何语言环境下都一样。
/// 比较统一走小写，`Timeout` / `timed out` 都能命中。
const NETWORK_MARKERS: &[&str] = &[
    "connection failed",
    "connect error",
    "connection closed",
    "stale connection",
    "dns failed",
    "could not resolve",
    "timed out",
    "timeout",
    "connection refused",
    "connection reset",
    "broken pipe",
    "network unreachable",
    "network is unreachable",
    "connection aborted",
    // ureq 2.12 换了错误文案：顶层是 "Network Error: …"，读到一半断了会是
    // "Error encountered in the status line"。两条都是传输层，跟本地语言环境无关。
    "network error",
    "error encountered in the status line",
];

/// 状态码之外的兜底：命中这些前缀说明请求根本没成型（传输层）。
fn looks_like_transport(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    NETWORK_MARKERS.iter().any(|m| lower.contains(m))
}

/// `"... (401)"` → `Some(401)`。只看末尾那一段，别的括号（如
/// `"(grant it with --allow-conv)"`）不是状态码，解析不出来就忽略。
fn trailing_status(message: &str) -> Option<u16> {
    let body = message.trim_end().strip_suffix(')')?;
    let inner = &body[body.rfind('(')? + 1..];
    if inner.len() != 3 || !inner.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    inner.parse().ok()
}

/// 把一条字符串错误归到四类之一。
pub fn classify(message: &str) -> WxlikeError {
    // 1. 权限：`Policy` 的拒绝发生在发请求之前，文案是确定的。
    if DENY_MARKERS.iter().any(|m| message.contains(m)) {
        return WxlikeError::wrap(message, WxlikeError::permission);
    }
    // 2. 状态码：`Http::expect_json` 把服务端原文 + `(status)` 拼在末尾。
    if let Some(status) = trailing_status(message) {
        return match status {
            401 | 407 => WxlikeError::wrap(message, WxlikeError::unauthorized),
            403 => WxlikeError::wrap(message, WxlikeError::permission),
            other => WxlikeError::unexpected(format!("{message} [{other}]")),
        };
    }
    // 3. 传输层：没有状态码，说明请求根本没成型。
    if looks_like_transport(message) {
        return WxlikeError::wrap(message, WxlikeError::network);
    }
    // 4. 剩下的是"我们这边的问题"——不猜它是网络错误。
    WxlikeError::wrap(message, WxlikeError::unexpected)
}

impl From<rusqlite::Error> for WxlikeError {
    fn from(err: rusqlite::Error) -> Self {
        Self::unexpected(format!("local database: {err}"))
    }
}

/// `crates/core` 用的是 anyhow（`Outbox::open` 之类），绑定层要能把它
/// 归到"我们这边的问题"，而不是装作是网络故障。
impl From<anyhow::Error> for WxlikeError {
    fn from(err: anyhow::Error) -> Self {
        Self::unexpected(format!("{err:#}"))
    }
}

impl From<serde_json::Error> for WxlikeError {
    fn from(err: serde_json::Error) -> Self {
        Self::unexpected(format!("local encode: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_status_suffix_maps_to_the_right_class() {
        assert!(matches!(
            classify("wrong password (401)"),
            WxlikeError::Unauthorized { .. }
        ));
        assert!(matches!(
            classify("you are not a member (403)"),
            WxlikeError::Permission { .. }
        ));
        // 422（正文为空/超长）重试也不会成功，不能算网络错误
        let err = classify("message too long (422)");
        assert!(matches!(err, WxlikeError::Unexpected { .. }), "{err}");
        assert!(!err.retryable());
    }

    #[test]
    fn policy_denies_are_permission_not_network() {
        for msg in [
            "conversation d-a~bob is not allowed: outside the allowlist (grant it with --allow-conv)",
            "msg.send writes and write access was not granted (add --allow-write)",
            "msg.recall is destructive and destructive access was not granted (add --allow-destructive)",
            "unknown command \"msg.drop\" (call im_list_commands)",
        ] {
            assert!(
                matches!(classify(msg), WxlikeError::Permission { .. }),
                "{msg}"
            );
        }
    }

    #[test]
    fn transport_failures_are_retryable_network_errors() {
        // 真实形态：`{method} {path}: {ureq error}`
        for msg in [
            "GET /api/me: Connection refused (os error 111)",
            "POST /api/conv/d-a~b/send: io-error Connection reset by peer",
            "GET /api/me: http://wxlike.invalid/api/me: Connection Failed: Dns Failed: DomainNameNotFound",
            "POST /api/conv/d-a~b/send: timed out",
            // Windows 中文环境下实测到的原文（测试里被抓到过）：OS 那半句是本地的，
            // ureq 的 "Connection Failed: Connect error" 才是稳定的。
            "GET /api/me: http://127.0.0.1:5647/api/me: Connection Failed: Connect error: \
             由于目标计算机积极拒绝，无法连接。 (os error 10061)",
            // 切到飞行模式后 DNS 已经解不出来
            "GET /api/me: http://wxlike.invalid/api/me: Connection Failed: Dns Failed: \
             不知道这样的主机。 (os error 11001)",
            // ureq 2.12 的实际形态（连上了但对端没回状态行）
            "POST /api/login: http://127.0.0.1:18099/api/login: Network Error: Network Error: \
             Error encountered in the status line: 由于连接方在一段时间后没有正确答复\
             或连接的主机没有反应，连接尝试失败。 (os error 10060)",
        ] {
            let err = classify(msg);
            assert!(matches!(err, WxlikeError::Network { .. }), "{msg} -> {err}");
            assert!(err.retryable(), "{msg} 该重试");
        }
    }

    /// 我们不认识的错误一律归到 `Unexpected`，不猜它是网络问题——
    /// 猜成网络问题就等于让队列去重试一条永远不会成功的东西。
    #[test]
    fn unknown_shapes_are_not_guessed_as_network() {
        let err = classify("decode /api/me: expected value at line 1 column 1");
        assert!(matches!(err, WxlikeError::Unexpected { .. }), "{err}");
        assert!(!err.retryable());
    }

    /// 权限文案里带着括号，但它不是状态码——不能被当成 HTTP 错误。
    #[test]
    fn non_status_parentheses_are_not_read_as_http_status() {
        assert_eq!(trailing_status("ok (grant it with --allow-conv)"), None);
        assert_eq!(trailing_status("denied (403)"), Some(403));
        assert_eq!(trailing_status("(403)"), Some(403));
        assert_eq!(trailing_status("no parens here"), None);
        assert_eq!(trailing_status("weird (4030)"), None);
    }

    #[test]
    fn local_database_errors_are_not_network_errors() {
        let sqlite = rusqlite::Connection::open_in_memory().unwrap();
        let err = WxlikeError::from(sqlite.execute("SELECT nope", []).unwrap_err());
        assert!(matches!(err, WxlikeError::Unexpected { .. }), "{err}");
        assert!(!err.retryable());
    }
}
