//! 分页与游标的共享逻辑：两端存储都调用它，避免"一边报错一边静默"。
//!
//! 放在单独文件里而不是 `store.rs`：`store.rs` 只该有端口声明，实现细节
//! 混进去会让"这个端口要什么"读起来很费劲（也是质量闸门里函数长度统计的坑）。

use wxlike_types::{ConvId, Cursor, CursorKind, EventEnvelope};

use super::store::StoreError;

/// 取游标之后的序列号（`None` = 从头），并校验它属于哪个流/会话。
///
/// # Errors
/// 游标格式非法、类型不是事件游标，或会话作用域与读取目标不一致。
pub fn event_cursor_floor(
    cursor: Option<&Cursor>,
    conv: Option<&ConvId>,
) -> Result<u64, StoreError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let value = cursor
        .decode()
        .map_err(|err| StoreError::Corrupt(err.to_string()))?;
    if value.kind != CursorKind::Event {
        return Err(StoreError::Corrupt(format!(
            "事件读取收到 {:?} 游标",
            value.kind
        )));
    }
    if value.conv.as_ref() != conv {
        return Err(StoreError::Corrupt(
            "游标绑定的会话与读取目标不一致".to_string(),
        ));
    }
    Ok(value.seq.get())
}

/// 校验游标只前进：`current` 是已经落盘的值（`None` 表示还没写过）。
///
/// 回退一律报 `Corrupt`——静默接受回退的表现是"重放已确认的事件"，
/// 用户看到的是消息重复。作用域不同的两个游标也不能比较：跨会话比较会
/// 把另一个会话的高 seq 当成"已经消费到"，直接丢事件。
///
/// # Errors
/// 回退、作用域不一致，或当前值/新值格式非法。
pub fn check_cursor_advance(current: Option<&str>, incoming: &Cursor) -> Result<(), StoreError> {
    let next = incoming
        .decode()
        .map_err(|err| StoreError::Corrupt(err.to_string()))?;
    let Some(current) = current else {
        return Ok(());
    };
    let previous = Cursor::parse_text(current)
        .and_then(|cursor| cursor.decode())
        .map_err(|err| StoreError::Corrupt(err.to_string()))?;
    if previous.kind != next.kind || previous.conv != next.conv {
        return Err(StoreError::Corrupt(
            "游标作用域不一致（kind / conversation）".to_string(),
        ));
    }
    if next.seq.get() < previous.seq.get() {
        return Err(StoreError::Corrupt(format!(
            "游标不许回退：{} < {}",
            next.seq.get(),
            previous.seq.get()
        )));
    }
    Ok(())
}

/// 按会话、`seq` 过滤、排序并截断成一页。
///
/// 事件是"可重建的缓存"，顺序与去重由这里统一保证；调用方只负责把候选集
/// 读出来（SQLite 用 SQL 排序，IndexedDB 只能全量读再排序）。
#[must_use]
pub fn page_events(
    mut rows: Vec<EventEnvelope>,
    conv: Option<&ConvId>,
    after: u64,
    limit: u32,
) -> Vec<EventEnvelope> {
    rows.retain(|event| event.conv.as_ref() == conv && event.seq.get() > after);
    rows.sort_by_key(|event| event.seq.get());
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    rows
}
