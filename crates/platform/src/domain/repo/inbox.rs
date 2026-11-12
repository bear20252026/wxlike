//! **仓储层**（`inbox`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use rusqlite::{params, Connection};

/// 事务内插入一条通知。空 `dedupe` = 不参与去重（如每条评论各一条）。
#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn notify(
    conn: &Connection,
    user_id: &str,
    kind: &str,
    actor: &str,
    target_kind: &str,
    target_id: &str,
    text: &str,
    dedupe: &str,
    now: i64,
) -> Result<()> {
    if user_id == actor {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO notifications(id, user_id, kind, actor, target_kind, target_id, text,
                                   dedupe_key, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(user_id, dedupe_key) WHERE dedupe_key <> '' DO NOTHING",
        params![
            crate::auth::random_hex(8),
            user_id,
            kind,
            actor,
            target_kind,
            target_id,
            text,
            dedupe,
            now
        ],
    )?;
    Ok(())
}
