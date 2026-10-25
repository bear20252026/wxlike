//! 列表端点共用的小工具：游标解析、LIKE 转义、默认页大小、统一响应尾。
//!
//! 这些不是业务逻辑，是"每个列表端点都要写一遍"的机械件。集中一处的原因
//! 很实际：九个 G05 模块各自抄一份后，跨文件重复窗口和质量闸门先红给你看。

use axum::Json;

use crate::error::{PlatformError, Result};
use crate::validate;

/// 未显式给 limit 时的默认页大小。
pub(crate) const DEFAULT_PAGE: i64 = 20;

/// `"{created_at}:{id}"` 两段游标。空游标 = 从头开始（哨兵值取最大）。
pub(crate) fn parse_cursor(cursor: Option<&str>) -> Result<(i64, String)> {
    match cursor {
        None | Some("") => Ok((i64::MAX, "\u{10ffff}".to_string())),
        Some(c) => {
            let (at, id) = c
                .split_once(':')
                .ok_or(PlatformError::BadInput("malformed cursor"))?;
            let at: i64 = at
                .parse()
                .map_err(|_| PlatformError::BadInput("malformed cursor"))?;
            let id = validate::id(id)?;
            Ok((at, id.to_string()))
        }
    }
}

/// LIKE 的通配符 `%`/`_`/`\` 一律按字面量转义（用户输入不当模式用）。
pub(crate) fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// DELETE/UPDATE 影响行数为 0 即"不存在或不是你的"（防探测统一 404）。
pub(crate) fn ensure_updated(changed: usize) -> Result<()> {
    if changed == 0 {
        return Err(PlatformError::NotFound);
    }
    Ok(())
}

/// 列表端点统一的响应尾：items + 游标 + 快照版本。
pub(crate) fn listing_response<T: serde::Serialize, C: serde::Serialize>(
    items: Vec<T>,
    next_cursor: C,
    has_more: bool,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "items": items,
        "next_cursor": next_cursor,
        "has_more": has_more,
        "snapshot_version": wxlike_core::types::now_ms(),
    }))
}
