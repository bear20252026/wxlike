//! `Authorization: Bearer <token>` 的解析——**全服务端只有一份**。
//!
//! 为什么单独一处：这个仓库里曾经有 6 份各自手写的解析，其中 5 份写的是
//! `strip_prefix("Bearer ")`。那是**大小写敏感**的，于是 `Authorization:
//! bearer abc`（scheme 按 RFC 6750 §2.1 是大小写不敏感的，小写完全合法）
//! 被它们判成"没带凭据"，而 `platform::Authed` 那份大小写不敏感、接受。
//! 同一个客户端只改个头写法，在媒体面 401、在平台面 200——这种分裂靠
//! "每处都记得"是守不住的，只能收成一处。
//!
//! 三条语义，改这里之前先读：
//!
//! 1. **scheme 大小写不敏感**（RFC 6750 §2.1：`Bearer` 不大写也对）。
//! 2. **分隔符是任意空白**，不是"恰好一个空格"：`Bearer\tabc` 合法，
//!    `Bearer  abc`（`1*SP`）也合法。
//! 3. **头值两端的可选空白（OWS）不属于凭据**：`  Bearer abc  ` 与
//!    `Bearer abc` 等价（RFC 9110 §5.5）。令牌 trim 后必须非空。
//!
//! 刻意**不**做的事：不校验令牌的字符集（RFC 6750 的 `b64token` 字母表）。
//! 令牌只作为查表键使用，多接受一种写法不会放开任何权限；反过来，严格校验
//! 会让某个带填充的写法突然 401，而那是传输层写法问题、不是鉴权问题。
//! 含空白的令牌（`Bearer a b`）因此会被当成一个整体查表键——必然查不中，
//! 自然落回 401，不需要在这里单独拒绝。

/// 从 `Authorization` 头值里解析出的 Bearer 凭据。
///
/// 零拷贝：只借输入字符串，不分配。`Copy` 是为了让它能直接进
/// `Iterator::map` 这类按值消费的链（调用点几乎都是那个形状）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bearer<'a>(&'a str);

impl<'a> Bearer<'a> {
    /// 解析头值。`None` = 这不是一个合法的 `Bearer` 凭据。
    ///
    /// 调用方拿到 `None` 后**自己**决定怎么拒（各服务的错误类型不同：
    /// `ApiError` / `AdminError` / `TokenError` / `MediaError`），
    /// 所以这里只表达"解没解出来"，不替调用方选拒绝形状。
    #[must_use]
    pub fn parse(value: &'a str) -> Option<Self> {
        // 先去掉头值两端的 OWS：`Authorization:  Bearer abc` 与
        // `Authorization: Bearer abc` 必须等价，否则"冒号后多一个空格"
        // 这种纯写法差异会变成 401。
        let value = value.trim();
        let (scheme, rest) = value.split_once(char::is_whitespace)?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return None;
        }
        let token = rest.trim();
        if token.is_empty() {
            return None;
        }
        Some(Self(token))
    }

    /// 凭据原串（不含 `Bearer ` 前缀）。
    #[must_use]
    pub fn token(self) -> &'a str {
        self.0
    }
}

impl std::fmt::Display for Bearer<'_> {
    /// 还原成标准写法，供测试与日志比对。
    ///
    /// 日志里**不要**直接打这个：它含凭据原串。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bearer {}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6750 §2.1：scheme 大小写不敏感；RFC 9110 §5.5：头值两端 OWS 不算数。
    /// 这张表就是"合法写法"的全集，谁把它改窄了这里立刻红。
    #[test]
    fn accepts_every_legal_spelling() {
        for (value, want) in [
            ("Bearer abc", Some("abc")),
            ("bearer abc", Some("abc")),
            ("BEARER abc", Some("abc")),
            ("BeArEr abc", Some("abc")),
            ("Bearer\tabc", Some("abc")),
            ("Bearer  abc", Some("abc")),
            ("  Bearer abc  ", Some("abc")),
            ("\tBearer\tabc\t", Some("abc")),
        ] {
            assert_eq!(Bearer::parse(value).map(Bearer::token), want, "{value:?}");
        }
    }

    /// 拒的面同样要钉住：没有 scheme、scheme 不是 bearer、令牌为空。
    #[test]
    fn rejects_missing_scheme_wrong_scheme_and_empty_token() {
        for value in [
            "",
            "   ",
            "Bearer",
            "Bearer ",
            "Bearer    ",
            "Basic abc",
            "bearerx abc",
            "xa Bearer abc",
        ] {
            assert_eq!(Bearer::parse(value), None, "{value:?}");
        }
    }

    /// 令牌字符集不校验（见模块文档）：含空白的写法当成一个整体查表键。
    #[test]
    fn token_alphabet_is_not_validated() {
        assert_eq!(
            Bearer::parse("Bearer abc def").map(Bearer::token),
            Some("abc def")
        );
        assert_eq!(Bearer::parse("Bearer ==").map(Bearer::token), Some("=="));
    }

    #[test]
    fn display_restores_the_canonical_spelling() {
        let parsed = Bearer::parse("  bearer\tabc.def  ").unwrap();
        assert_eq!(parsed.token(), "abc.def");
        assert_eq!(parsed.to_string(), "Bearer abc.def");
    }
}
