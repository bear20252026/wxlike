//! **仓储层**（`notes`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::{params, OptionalExt as _, StorageConnection};

use crate::domain::notes::Note;

pub(in crate::domain) fn note_row(
    c: &StorageConnection<'_>,
    user: &str,
    id: &str,
) -> Result<Option<Note>> {
    c.query_row(
        "SELECT id, user_id, title, body, version, created_at, updated_at
         FROM notes WHERE id = ?1 AND user_id = ?2",
        params![id, user],
        note_map,
    )
    .optional()
}

pub(in crate::domain) fn note_map(r: &crate::storage::StorageRow<'_>) -> Result<Note> {
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

pub(in crate::domain) fn note_exists(c: &StorageConnection<'_>, id: &str) -> Result<bool> {
    let existing: Option<String> = c
        .query_row(
            "SELECT user_id FROM notes WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(existing.is_some())
}

pub(in crate::domain) fn insert_note(
    c: &StorageConnection<'_>,
    id: &str,
    user: &str,
    title: &str,
    body: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO notes(id, user_id, title, body, version, created_at, updated_at)
         VALUES(?1, ?2, ?3, ?4, 1, ?5, ?5)",
        params![id, user, title, body, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn insert_version(
    c: &StorageConnection<'_>,
    id: &str,
    version: i64,
    title: &str,
    body: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO note_versions(note_id, version, title, body, saved_at)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![id, version, title, body, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn note_version(
    c: &StorageConnection<'_>,
    user: &str,
    id: &str,
) -> Result<Option<i64>> {
    c.query_row(
        "SELECT version FROM notes WHERE id = ?1 AND user_id = ?2",
        params![id, user],
        |r| r.get(0),
    )
    .optional()
}

pub(in crate::domain) fn update_note(
    c: &StorageConnection<'_>,
    user: &str,
    id: &str,
    title: &str,
    body: &str,
    version: i64,
    now: i64,
) -> Result<()> {
    c.execute(
        "UPDATE notes SET title = ?1, body = ?2, version = ?3, updated_at = ?4
         WHERE id = ?5 AND user_id = ?6",
        params![title, body, version, now, id, user],
    )?;
    Ok(())
}

pub(in crate::domain) fn notes_page(
    c: &StorageConnection<'_>,
    user: &str,
    before_at: i64,
    before_id: &str,
    limit: i64,
) -> Result<Vec<Note>> {
    let stmt = c.prepare(
        "SELECT id, user_id, title, body, version, created_at, updated_at
         FROM notes WHERE user_id = ?1
           AND (updated_at < ?2 OR (updated_at = ?2 AND id < ?3))
         ORDER BY updated_at DESC, id DESC LIMIT ?4",
    )?;
    let it = stmt.query_map(params![user, before_at, before_id, limit], note_map)?;
    it.collect::<std::result::Result<Vec<_>, _>>()
}

delete_owned_row!("notes");
