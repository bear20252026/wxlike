//! **仓储层**（`live`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::{params, OptionalExt as _, StorageConnection};


/// 在播会话的展示字段；峰值观众只在库里维护，读模型不需要。
pub(in crate::domain) struct LiveSnapshot {
    pub host: String,
    pub stream_key: String,
    pub recording: i64,
    pub viewers: i64,
}

/// 停播需要定位的会话（id 与主播）。
pub(in crate::domain) struct LiveSessionRef {
    pub id: String,
    pub host: String,
}

pub(in crate::domain) fn live_state_for_stream(
    c: &StorageConnection<'_>,
    stream_key: &str,
) -> Result<Option<String>> {
    let state = c
        .query_row(
            "SELECT state FROM live_sessions WHERE stream_key = ?1 ORDER BY started_at DESC LIMIT 1",
            params![stream_key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(state)
}

pub(in crate::domain) fn active_session_exists(
    c: &StorageConnection<'_>,
    channel: &str,
) -> Result<bool> {
    let id = c
        .query_row(
            "SELECT id FROM live_sessions WHERE channel_id = ?1 AND state = 'live'",
            params![channel],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(id.is_some())
}

pub(in crate::domain) fn insert_live_session(
    c: &StorageConnection<'_>,
    id: &str,
    channel: &str,
    host: &str,
    stream_key: &str,
    recording: bool,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO live_sessions(id, channel_id, host, stream_key, state, recording,
                               started_at, ended_at, viewers, peak_viewers)
         VALUES(?1, ?2, ?3, ?4, 'live', ?5, ?6, NULL, 0, 0)",
        params![id, channel, host, stream_key, i64::from(recording), now],
    )?;
    Ok(())
}

pub(in crate::domain) fn active_session_ref(
    c: &StorageConnection<'_>,
    channel: &str,
) -> Result<Option<LiveSessionRef>> {
    let row = c
        .query_row(
            "SELECT id, host FROM live_sessions WHERE channel_id = ?1 AND state = 'live'",
            params![channel],
            |r| {
                Ok(LiveSessionRef {
                    id: r.get(0)?,
                    host: r.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn end_live_session(
    c: &StorageConnection<'_>,
    id: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "UPDATE live_sessions SET state = 'ended', ended_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    c.execute(
        "DELETE FROM live_viewers WHERE session_id = ?1",
        params![id],
    )?;
    Ok(())
}

pub(in crate::domain) fn active_live_snapshot(
    c: &StorageConnection<'_>,
    channel: &str,
) -> Result<Option<LiveSnapshot>> {
    let row = c
        .query_row(
            "SELECT host, stream_key, recording, viewers
             FROM live_sessions WHERE channel_id = ?1 AND state = 'live'",
            params![channel],
            |r| {
                Ok(LiveSnapshot {
                    host: r.get(0)?,
                    stream_key: r.get(1)?,
                    recording: r.get(2)?,
                    viewers: r.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub(in crate::domain) fn active_session_id(
    c: &StorageConnection<'_>,
    channel: &str,
) -> Result<Option<String>> {
    let id = c
        .query_row(
            "SELECT id FROM live_sessions WHERE channel_id = ?1 AND state = 'live'",
            params![channel],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(id)
}

pub(in crate::domain) fn insert_viewer(
    c: &StorageConnection<'_>,
    session: &str,
    user: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO live_viewers(session_id, user_id, joined_at) VALUES(?1, ?2, ?3)
         ON CONFLICT(session_id, user_id) DO NOTHING",
        params![session, user, now],
    )?;
    Ok(())
}

/// 重算观众数并同步峰值，返回当前观众数。
pub(in crate::domain) fn refresh_viewer_counts(
    c: &StorageConnection<'_>,
    session: &str,
) -> Result<i64> {
    c.execute(
        "UPDATE live_sessions SET viewers = (SELECT COUNT(*) FROM live_viewers WHERE session_id = ?1),
            peak_viewers = MAX(peak_viewers, (SELECT COUNT(*) FROM live_viewers WHERE session_id = ?1))
         WHERE id = ?1",
        params![session],
    )?;
    let viewers = c.query_row(
        "SELECT viewers FROM live_sessions WHERE id = ?1",
        params![session],
        |r| r.get(0),
    )?;
    Ok(viewers)
}

pub(in crate::domain) fn delete_viewer(
    c: &StorageConnection<'_>,
    session: &str,
    user: &str,
) -> Result<()> {
    c.execute(
        "DELETE FROM live_viewers WHERE session_id = ?1 AND user_id = ?2",
        params![session, user],
    )?;
    Ok(())
}

/// 弹幕收件人：当前在房且不含发送者自己。
pub(in crate::domain) fn danmaku_recipients(
    c: &StorageConnection<'_>,
    session: &str,
    sender: &str,
) -> Result<Vec<String>> {
    let stmt =
        c.prepare("SELECT user_id FROM live_viewers WHERE session_id = ?1 AND user_id <> ?2")?;
    let rows = stmt.query_map(params![session, sender], |r| r.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}
