//! 文本检索的公共转义。
//!
//! SQL `LIKE` 的 `%` / `_` 是通配符，用户搜 `a%b` 时不能让它变成"任意字符"。
//! admin 报告搜索与 search 查询原先各写一份 `escape_like`，两处的字符集
//! 还不完全一致（一处漏了 `\\` 的容量预留）；集中到这里后只有一个口径。

/// 给 `LIKE` 模式转义 `%`、`_`、`\`，调用方的 SQL 必须带 `ESCAPE '\'`。
#[must_use]
pub fn escape_like(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 4);
    for ch in raw.chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::escape_like;

    #[test]
    fn escapes_wildcards_and_backslashes() {
        assert_eq!(escape_like("a%b_c\\d"), "a\\%b\\_c\\\\d");
        assert_eq!(escape_like("plain"), "plain");
    }
}
