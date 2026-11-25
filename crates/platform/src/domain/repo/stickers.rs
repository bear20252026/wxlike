//! **仓储层**（`stickers`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::{params, OptionalExt as _, StorageConnection};

use crate::domain::stickers::{Sticker, StickerPack};

pub(in crate::domain) fn pack_row(
    c: &StorageConnection<'_>,
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
    let stmt = c.prepare(
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

/// 媒体校验原料；大小与类型判断留在 domain。
pub(in crate::domain) struct MediaInfo {
    pub(crate) uploader: String,
    pub(crate) size: i64,
    pub(crate) content_type: String,
}

pub(in crate::domain) fn insert_pack(
    c: &StorageConnection<'_>,
    id: &str,
    owner: &str,
    name: &str,
    license: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO sticker_packs(id, owner, name, license, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![id, owner, name, license, now],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(in crate::domain) fn insert_sticker(
    c: &StorageConnection<'_>,
    id: &str,
    pack_id: &str,
    media_id: &str,
    name: &str,
    sort_order: i64,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO stickers(id, pack_id, media_id, name, sort_order, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, pack_id, media_id, name, sort_order, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn media_info(
    c: &StorageConnection<'_>,
    media_id: &str,
) -> Result<Option<MediaInfo>> {
    c.query_row(
        "SELECT uploader, size, content_type FROM media WHERE id = ?1",
        params![media_id],
        |r| {
            Ok(MediaInfo {
                uploader: r.get(0)?,
                size: r.get(1)?,
                content_type: r.get(2)?,
            })
        },
    )
    .optional()
}

/// 本人创建或已安装的包 id。
pub(in crate::domain) fn packs_for(c: &StorageConnection<'_>, user: &str) -> Result<Vec<String>> {
    let stmt = c.prepare(
        "SELECT p.id FROM sticker_packs p
         WHERE p.owner = ?1
            OR EXISTS (SELECT 1 FROM user_stickers us
                       JOIN stickers s ON s.id = us.sticker_id
                       WHERE us.user_id = ?1 AND s.pack_id = p.id)
         ORDER BY p.created_at DESC",
    )?;
    let it = stmt.query_map(params![user], |r| r.get::<_, String>(0))?;
    it.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn pack_exists(c: &StorageConnection<'_>, pack_id: &str) -> Result<bool> {
    let exists: Option<i64> = c
        .query_row(
            "SELECT 1 FROM sticker_packs WHERE id = ?1",
            params![pack_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

/// 包内 (sticker_id, sort_order)，安装时按它补行。
pub(in crate::domain) fn pack_stickers(
    c: &StorageConnection<'_>,
    pack_id: &str,
) -> Result<Vec<(String, i64)>> {
    let stmt =
        c.prepare("SELECT id, sort_order FROM stickers WHERE pack_id = ?1 ORDER BY sort_order")?;
    let rows = stmt.query_map(params![pack_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn install_sticker(
    c: &StorageConnection<'_>,
    user: &str,
    sticker_id: &str,
    sort_order: i64,
    now: i64,
) -> Result<usize> {
    c.execute(
        "INSERT INTO user_stickers(user_id, sticker_id, sort_order, installed_at)
         VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(user_id, sticker_id) DO NOTHING",
        params![user, sticker_id, sort_order, now],
    )
}

pub(in crate::domain) fn uninstall_pack(
    c: &StorageConnection<'_>,
    user: &str,
    pack_id: &str,
) -> Result<usize> {
    c.execute(
        "DELETE FROM user_stickers
         WHERE user_id = ?1 AND sticker_id IN (SELECT id FROM stickers WHERE pack_id = ?2)",
        params![user, pack_id],
    )
}

pub(in crate::domain) fn installed_for(
    c: &StorageConnection<'_>,
    user: &str,
) -> Result<Vec<Sticker>> {
    let stmt = c.prepare(
        "SELECT s.id, s.pack_id, s.media_id, s.name, us.sort_order
         FROM user_stickers us JOIN stickers s ON s.id = us.sticker_id
         WHERE us.user_id = ?1
         ORDER BY us.sort_order, us.installed_at, s.id",
    )?;
    let rows = stmt.query_map(params![user], |r| {
        Ok(Sticker {
            id: r.get(0)?,
            pack_id: r.get(1)?,
            media_id: r.get(2)?,
            name: r.get(3)?,
            sort_order: r.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
}

pub(in crate::domain) fn set_sticker_order(
    c: &StorageConnection<'_>,
    user: &str,
    sticker_id: &str,
    sort_order: i64,
) -> Result<usize> {
    c.execute(
        "UPDATE user_stickers SET sort_order = ?1
         WHERE user_id = ?2 AND sticker_id = ?3",
        params![sort_order, user, sticker_id],
    )
}

pub(in crate::domain) fn delete_user_sticker(
    c: &StorageConnection<'_>,
    user: &str,
    sticker_id: &str,
) -> Result<usize> {
    c.execute(
        "DELETE FROM user_stickers WHERE user_id = ?1 AND sticker_id = ?2",
        params![user, sticker_id],
    )
}

pub(in crate::domain) fn delete_pack(
    c: &StorageConnection<'_>,
    owner: &str,
    pack_id: &str,
) -> Result<usize> {
    c.execute(
        "DELETE FROM sticker_packs WHERE id = ?1 AND owner = ?2",
        params![pack_id, owner],
    )
}
