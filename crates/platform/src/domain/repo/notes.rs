//! **仓储层**（`notes`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use rusqlite::{params, OptionalExtension as _};

use crate::domain::notes::Note;

pub(in crate::domain) fn note_row(
    c: &rusqlite::Connection,
    user: &str,
    id: &str,
) -> Result<Option<Note>> {
    Ok(c.query_row(
        "SELECT id, user_id, title, body, version, created_at, updated_at
         FROM notes WHERE id = ?1 AND user_id = ?2",
        params![id, user],
        note_map,
    )
    .optional()?)
}

pub(in crate::domain) fn note_map(r: &rusqlite::Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        id: r.get(0)?,
        user_id: r.get(1)?,
        title: r.get(2)?,
        body: r.get(3)?,
        version: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}
