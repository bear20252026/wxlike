//! 查询侧内核：G02（消息检索）与 G05（公开内容检索）**共用**的一条流水线。
//!
//! 为什么要抽出来：两个域原本各写一份"清洗用户输入 → 切词 → 摘要"的逻辑，
//! 于是一边改了转义另一边忘了。这里把它收成一份，并且刻意保持
//! **无第三方依赖、无 IO**——`SearchIndex`（tantivy）和
//! `wxlike_platform::content_index`（SQLite）都在它两侧插自己的适配器，
//! 谁也不能绕过这层的校验与转义。
//!
//! 三条硬规则：
//!
//! 1. **用户输入永远不是查询语言。** [`NormalizedQuery`] 只产出 token 与
//!    LIKE 模式串，布尔算子/字段名/引号在解析阶段就变成字面量。
//! 2. **通配符按字面量。** LIKE 侧 `%`、`_`、`\` 一律转义（[`escape_like`]）。
//! 3. **摘要是有界窗口。** [`NormalizedQuery::snippet`] 只回 `max` 个字符，
//!    不复制整篇正文（最小权限：检索响应不该是全文的搬运工）。

use std::fmt;

/// 查询串长度上限（两个域共用同一个上限，别再各写一份）。
pub const MAX_QUERY_CHARS: usize = 100;
/// token 数上限：超长输入截断，防查询风暴。
pub const MAX_TOKENS: usize = 16;
/// 摘要默认窗口（字符）。
pub const DEFAULT_SNIPPET_CHARS: usize = 120;
/// 中文/日文汉字区（BMP 基本区 + 扩展 A）。检索按二元组切，因为 CJK 没有词边界。
pub fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF)
}

/// 查询被拒绝的原因。**不**用 anyhow：调用方要据此映射 422，
/// 需要能匹配到具体分支，而不是读错误字符串。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryError {
    Empty,
    TooLong,
    ControlChar,
    MalformedCursor,
    UnknownScope,
}

impl QueryError {
    /// 面向 API 的稳定文案（同时是给客户端看的唯一信息——
    /// 不透露"命中了但不可见"这种存在性线索）。
    pub fn as_str(self) -> &'static str {
        match self {
            QueryError::Empty => "query must not be empty",
            QueryError::TooLong => "query too long",
            QueryError::ControlChar => "query contains control characters",
            QueryError::MalformedCursor => "malformed cursor",
            QueryError::UnknownScope => "unknown search scope",
        }
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for QueryError {}

/// 清洗后的查询。构造不出来就等于没通过闸。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedQuery {
    text: String,
    tokens: Vec<String>,
}

impl NormalizedQuery {
    /// 去空白 → 限长 → 禁控制字符 → 小写。空串与超长都是**拒绝**，
    /// 不是"截断后继续"（截断会把 `%` 之类的残留送进 LIKE）。
    pub fn parse(input: &str) -> Result<Self, QueryError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(QueryError::Empty);
        }
        if trimmed.chars().count() > MAX_QUERY_CHARS {
            return Err(QueryError::TooLong);
        }
        if trimmed.chars().any(char::is_control) {
            return Err(QueryError::ControlChar);
        }
        let text = trimmed.to_lowercase();
        Ok(Self {
            tokens: tokenize(&text),
            text,
        })
    }

    /// 归一化后的原始串（小写、已去首尾空白）。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 切好的 token（AND 语义用）。
    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 子串/LIKE 检索用的模式串。转义在 [`escape_like`]，这里只加两端通配。
    pub fn like_pattern(&self) -> String {
        format!("%{}%", escape_like(&self.text))
    }

    /// 有界摘要：优先从命中处开始，找不到命中词就取开头。
    ///
    /// 位置换算走"小写串的字符序"：`to_lowercase` 可能改变字节长度
    /// （如 `İ`），直接拿字节下标切原串会在非 UTF-8 边界 panic。
    pub fn snippet(&self, body: &str, max: usize) -> String {
        let lower = body.to_lowercase();
        let skip = lower
            .find(&self.text)
            .map(|byte| lower[..byte].chars().count())
            .unwrap_or(0);
        body.chars().skip(skip).take(max.max(1)).collect()
    }

    /// 热搜词提取：ASCII 词（≥3 且非停用词）+ CJK 二元组。
    /// 与 [`tokenize`] 不同：那是"检索单元"，这是"聚合单元"。
    pub fn terms(&self) -> Vec<String> {
        terms(&self.text)
    }
}

/// 索引侧/查询侧共用的切词：字母数字与 CJK 连成一段（ngram 分词器在
/// 索引侧再切一次，所以 "pineapple" 能命中 "pineapples"）。
///
/// 这层同时是 Tantivy 查询语法防火墙：`AND` / `field:` / 引号在这里
/// 只会变成普通 token，永远不会进解析器。
pub fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in input.chars() {
        if ch.is_alphanumeric() {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.truncate(MAX_TOKENS);
    out
}

/// LIKE 通配符转义：`%`、`_`、`\`。用户输入里的这些只能当字面量。
/// 调用方的 LIKE 必须带 `ESCAPE '\'`，否则 `\\` 本身又不对了。
pub fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

/// 热搜/统计用的词条切分：ASCII 词 + CJK 二元组，稳定排序去重。
pub fn terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            ascii.push(ch.to_ascii_lowercase());
        } else if is_cjk(ch) {
            flush_ascii(&mut ascii, &mut out);
            cjk.push(ch);
        } else {
            flush_ascii(&mut ascii, &mut out);
            flush_cjk(&mut cjk, &mut out);
        }
    }
    flush_ascii(&mut ascii, &mut out);
    flush_cjk(&mut cjk, &mut out);
    out.sort();
    out.dedup();
    out
}

fn flush_ascii(buf: &mut String, out: &mut Vec<String>) {
    if buf.chars().count() >= 3 && !STOPWORDS.contains(&buf.as_str()) {
        out.push(std::mem::take(buf));
    }
    buf.clear();
}

fn flush_cjk(buf: &mut Vec<char>, out: &mut Vec<String>) {
    if buf.len() >= 2 {
        for pair in buf.windows(2) {
            out.push(pair.iter().collect());
        }
    }
    buf.clear();
}

/// 英文高频词：进热搜等于把榜单让给"the"。
const STOPWORDS: [&str; 6] = ["the", "and", "for", "with", "this", "that"];

/// 统一分页游标：`"{created_at}:{doc_id}"`。
///
/// `None` = 从最新开始，用 `(i64::MAX, SENTINEL)` 表示"比任何行都新"，
/// 这样比较式 `created_at < ? OR (created_at = ? AND doc_id < ?)` 对
/// 所有页统一成立，不需要为首页写一条特殊 SQL。
pub const FIRST_CURSOR: (i64, &str) = (i64::MAX, "\u{10ffff}");
/// 游标里 doc id 的最大长度：内容 id 全部由服务端生成（ASCII 字母数字、
/// 定长上限），不接受其他形态，防止把外部拼出的游标当检索入口。
pub const MAX_CURSOR_ID_LEN: usize = 64;

/// 校验游标里的 id 形态（ASCII 字母数字、非空、≤64）。
pub fn cursor_id_is_valid(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_CURSOR_ID_LEN && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// 解析 `"{at}:{id}"`。返回 `(before_at, before_id)`。
pub fn parse_cursor(cursor: Option<&str>, limit_ms: bool) -> Result<(i64, String), QueryError> {
    let Some(raw) = cursor.filter(|c| !c.is_empty()) else {
        return Ok((FIRST_CURSOR.0, FIRST_CURSOR.1.to_string()));
    };
    let Some((at, id)) = raw.split_once(':') else {
        return Err(QueryError::MalformedCursor);
    };
    let at: i64 = at.parse().map_err(|_| QueryError::MalformedCursor)?;
    if limit_ms && at < 0 {
        return Err(QueryError::MalformedCursor);
    }
    if !cursor_id_is_valid(id) {
        return Err(QueryError::MalformedCursor);
    }
    Ok((at, id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_empty_long_and_control_input() {
        assert_eq!(NormalizedQuery::parse("  "), Err(QueryError::Empty));
        assert_eq!(
            NormalizedQuery::parse(&"x".repeat(MAX_QUERY_CHARS + 1)),
            Err(QueryError::TooLong)
        );
        assert_eq!(
            NormalizedQuery::parse("ok\u{7}bad"),
            Err(QueryError::ControlChar)
        );
        assert_eq!(
            NormalizedQuery::parse("  RustLang  ").unwrap().text(),
            "rustlang"
        );
    }

    #[test]
    fn like_pattern_is_escaped() {
        let q = NormalizedQuery::parse("100%_x\\y").unwrap();
        assert_eq!(q.like_pattern(), "%100\\%\\_x\\\\y%");
    }

    #[test]
    fn tokenizer_keeps_cjk_runs_and_blocks_query_syntax() {
        assert_eq!(tokenize("慢速模式 slow mode"), ["慢速模式", "slow", "mode"]);
        assert_eq!(tokenize("AND"), ["AND"]);
        assert_eq!(tokenize("sender:alice"), ["sender", "alice"]);
        let many = tokenize(&vec!["a"; MAX_TOKENS + 20].join(" "));
        assert_eq!(many.len(), MAX_TOKENS);
    }

    #[test]
    fn snippet_is_bounded_and_survives_case_folding_length_change() {
        let body = "前言".repeat(200) + "慢速模式" + &"后续".repeat(200);
        let q = NormalizedQuery::parse("慢速模式").unwrap();
        let s = q.snippet(&body, 12);
        assert_eq!(s.chars().count(), 12);
        assert!(s.starts_with("慢速模式"));
        // İ -> "i̇" ：小写化后字节数变了，按字节切会 panic
        let q = NormalizedQuery::parse("İ").unwrap();
        assert_eq!(q.snippet("İx", 4), "İx");
        let q = NormalizedQuery::parse("missing").unwrap();
        assert_eq!(q.snippet("abcdef", 3), "abc");
    }

    #[test]
    fn terms_are_ascii_words_and_cjk_bigrams() {
        let t = terms("慢速模式 rust language the");
        assert!(t.contains(&"慢速".to_string()));
        assert!(t.contains(&"速模".to_string()));
        assert!(t.contains(&"rust".to_string()));
        assert!(!t.contains(&"the".to_string()));
        assert!(!t.contains(&"ab".to_string()), "ASCII 词要 >=3");
        assert_eq!(t, {
            let mut v = t.clone();
            v.sort();
            v.dedup();
            v
        });
    }

    #[test]
    fn cursor_shape_is_strict() {
        assert_eq!(
            parse_cursor(None, true).unwrap(),
            (i64::MAX, "\u{10ffff}".into())
        );
        assert_eq!(
            parse_cursor(Some("9:abc123"), true).unwrap(),
            (9, "abc123".into())
        );
        for bad in [
            "9",
            "x:abc",
            "9:a b",
            "9:",
            "9:abc:9",
            "-1:abc",
            "9:../../x",
        ] {
            assert_eq!(
                parse_cursor(Some(bad), true),
                Err(QueryError::MalformedCursor),
                "{bad}"
            );
        }
        // 非毫秒域（如 seq 游标）允许负数，但 id 形态照样收紧
        assert_eq!(
            parse_cursor(Some("-5:ab"), false).unwrap(),
            (-5, "ab".into())
        );
    }
}
