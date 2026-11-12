//! **仓储层**（`stickers`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension as _};

use crate::domain::stickers::{Sticker, StickerPack};

pub(in crate::domain) fn pack_row(
    c: &Connection,
    id: &str,
    viewer: &str,
) -> Result<Option<StickerPack>> {
    let head: Option<(String, String, String, String, i64)> = c
        .query_row(
            "SELECT id, owner, name, license, created_at FROM sticker_packs WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((id, owner, name, license, created_at)) = head else {
        return Ok(None);
    };
    let mut stmt = c.prepare(
        "SELECT id, pack_id, media_id, name, sort_order FROM stickers
         WHERE pack_id = ?1 ORDER BY sort_order, id",
    )?;
    let stickers = stmt
        .query_map(params![id], |r| {
            Ok(Sticker {
                id: r.get(0)?,
                pack_id: r.get(1)?,
                media_id: r.get(2)?,
                name: r.get(3)?,
                sort_order: r.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let installed: i64 = c.query_row(
        "SELECT COUNT(*) FROM user_stickers us JOIN stickers s ON s.id = us.sticker_id
         WHERE us.user_id = ?1 AND s.pack_id = ?2",
        params![viewer, id],
        |r| r.get(0),
    )?;
    let cover = stickers
        .first()
        .map(|s| s.media_id.clone())
        .unwrap_or_default();
    Ok(Some(StickerPack {
        id,
        owner,
        name,
        license,
        created_at,
        sticker_count: stickers.len() as i64,
        installed: installed > 0,
        cover_media_id: cover,
        stickers,
    }))
}
