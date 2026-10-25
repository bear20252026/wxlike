//! 分页与游标的共享逻辑：两端存储都调用它，避免"一边报错一边静默"。
//!
//! 放在单独文件里而不是 `store.rs`：`store.rs` 只该有端口声明，实现细节
//! 混进去会让"这个端口要什么"读起来很费劲（也是质量闸门里函数长度统计的坑）。

use wxlike_types::{Cursor, EventEnvelope};

use super::store::StoreError;

/// 取游标之后的序列号（`None` = 从头）。
///
/// # Errors
/// 游标格式非法。
pub fn cursor_floor(cursor: Option<&Cursor>) -> Result<u64, StoreError> {
    match cursor {
        None => Ok(0),
        Some(cursor) => cursor
            .decode()
            .map(|value| value.seq.get())
            .map_err(|err| StoreError::Corrupt(err.to_string())),
    }
}

/// 校验游标只前进：`current` 是已经落盘的值（`None` 表示还没写过）。
///
/// 回退一律报 `Corrupt`——静默接受回退的表现是"重放已确认的事件"，
/// 用户看到的是消息重复。
///
/// # Errors
/// 回退，或当前值/新值格式非法。
pub fn check_cursor_advance(current: Option<&str>, incoming: &Cursor) -> Result<(), StoreError> {
    let next = cursor_floor(Some(incoming))?;
    let Some(current) = current else {
        return Ok(());
    };
    let previous =
        Cursor::decode_seq(current).map_err(|err| StoreError::Corrupt(err.to_string()))?;
    if next < previous {
        return Err(StoreError::Corrupt(format!(
            "游标不许回退：{next} < {previous}"
        )));
    }
    Ok(())
}

/// 按 `seq` 过滤、排序并截断成一页。
///
/// 事件是"可重建的缓存"，顺序与去重由这里统一保证；调用方只负责把候选集
/// 读出来（SQLite 用 SQL 排序，IndexedDB 只能全量读再排序）。
#[must_use]
pub fn page_events(mut rows: Vec<EventEnvelope>, after: u64, limit: u32) -> Vec<EventEnvelope> {
    rows.retain(|event| event.seq.get() > after);
    rows.sort_by_key(|event| event.seq.get());
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    rows
}
