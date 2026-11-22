//! **仓储层**（`media`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::StorageDialect;
use rusqlite::{params, Connection, OptionalExtension as _};

/// 内容寻址行的幂等写入。SQLite 用 `ON CONFLICT` 保留首次归属；PostgreSQL
/// 兼容视图没有唯一约束，由 `media_insert_trg` 在 `media_object` 上做 upsert。
#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn insert(
    conn: &Connection,
    dialect: StorageDialect,
    id: &str,
    size: i64,
    content_type: &str,
    uploader: &str,
    conv_id: &str,
    public: bool,
    has_thumb: bool,
    now: i64,
) -> Result<()> {
    let sql = match dialect {
        StorageDialect::Sqlite => {
            "INSERT INTO media(id, size, content_type, uploader, conv_id, public, has_thumb, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET size = excluded.size,
                -- 一旦公开就不回收：头像被别的用户引用着
                public = MAX(media.public, excluded.public),
                -- 同一份字节可能被不同会话引用：保留第一次的归属，
                -- 空归属则补上（内容寻址下，归属是引用关系而不是字节属性）
                conv_id = CASE WHEN media.conv_id = '' THEN excluded.conv_id
                               ELSE media.conv_id END"
        }
        StorageDialect::Postgres => {
            "INSERT INTO media(id, size, content_type, uploader, conv_id, public, has_thumb, created_at)
             VALUES($1, $2, $3, $4, $5, $6, $7, $8)"
        }
    };
    conn.execute(
        sql,
        params![
            id,
            size,
            content_type,
            uploader,
            conv_id,
            public as i64,
            has_thumb as i64,
            now
        ],
    )?;
    Ok(())
}

pub(in crate::domain) fn owner(
    conn: &Connection,
    media_id: &str,
) -> Result<Option<(String, String, bool)>> {
    Ok(conn
        .query_row(
            "SELECT uploader, conv_id, public FROM media WHERE id = ?1",
            params![media_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                ))
            },
        )
        .optional()?)
}

pub(in crate::domain) fn meta(
    conn: &Connection,
    media_id: &str,
) -> Result<Option<(u64, String, bool)>> {
    Ok(conn
        .query_row(
            "SELECT size, content_type, has_thumb FROM media WHERE id = ?1",
            params![media_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                ))
            },
        )
        .optional()?)
}
