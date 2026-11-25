//! **仓储层**（`video_refs`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::domain::video_refs::{CallbackResult, RefRow};
use crate::error::Result;
use crate::storage::{params, OptionalExt as _, StorageConnection};


/// 转码回调的幂等落库：`video_id` 是主键，后到的覆盖先到的。
#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn store(
    conn: &StorageConnection<'_>,
    result: &CallbackResult,
    job_id: &str,
    state: &str,
    renditions: &str,
    attempts: i64,
    at: i64,
) -> Result<()> {
    conn.execute(
        r#"
        INSERT INTO video_refs (
            video_id, job_id, state, playlist_url, play_url, thumb_url,
            duration_ms, width, height, renditions, attempts, updated_at_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT(video_id) DO UPDATE SET
            job_id       = excluded.job_id,
            state        = excluded.state,
            playlist_url = excluded.playlist_url,
            play_url     = excluded.play_url,
            thumb_url    = excluded.thumb_url,
            duration_ms  = excluded.duration_ms,
            width        = excluded.width,
            height       = excluded.height,
            renditions   = excluded.renditions,
            attempts     = excluded.attempts,
            updated_at_ms= excluded.updated_at_ms
        "#,
        params![
            result.video_id,
            job_id,
            state,
            result.playlist_url,
            result.play_url.as_deref().unwrap_or_default(),
            result.thumb_url.as_deref().unwrap_or_default(),
            result.duration_ms as i64,
            result.width as i64,
            result.height as i64,
            renditions,
            attempts,
            at,
        ],
    )?;
    Ok(())
}

pub(in crate::domain) fn load(
    conn: &StorageConnection<'_>,
    video_id: &str,
) -> Result<Option<RefRow>> {
    conn
        .query_row(
            r#"
            SELECT video_id, job_id, state, playlist_url, play_url, thumb_url,
                   duration_ms, width, height, renditions, attempts, updated_at_ms
            FROM video_refs WHERE video_id = ?1
            "#,
            params![video_id],
            |r| wxlike_core::row_tuple!(r; 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11),
        )
        .optional()
}
