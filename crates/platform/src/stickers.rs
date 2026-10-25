//! G05-03 表情包（`/api/stickers`）。
//!
//! 三个真实持久化的集合：包（`sticker_packs`）、包内表情（`stickers`）、
//! 用户安装表（`user_stickers`）。安装状态存服务端，所以换端登录后能看到
//! 同一套表情与排序；卸载只删本人安装行，不影响包和其他人。
//!
//! 边界：
//!
//! * 许可必须是白名单里的开放许可。无法确认再分发权利的素材不能被别人
//!   安装——这是"版权限制"落地的地方，而不是文案提示。
//! * 单张 ≤ 5 MB、必须是图片（按服务端嗅探的 content_type，不信声明）、
//!   必须是创建者上传的媒体。
//! * 表情发送走普通图片消息（引用 `media_id`）；这里保证 media 仍存在，
//!   因此"可实际发送且可重下"都能被真实断言。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::validate;
use crate::{Platform, PlatformState};

pub const MAX_STICKER_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_STICKERS_PER_PACK: usize = 100;
pub const MAX_PACK_NAME_CHARS: usize = 64;
pub const MAX_STICKER_NAME_CHARS: usize = 32;
pub const MAX_LICENSE_CHARS: usize = 64;
const LICENSES: [&str; 4] = ["cc0-1.0", "cc-by-4.0", "cc-by-sa-4.0", "agpl-3.0-only"];

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/stickers/packs", post(create_pack))
        .route("/api/stickers/packs", get(list_packs))
        .route("/api/stickers/packs/{id}", get(one_pack))
        .route("/api/stickers/packs/{id}", delete(remove_pack))
        .route("/api/stickers/packs/{id}/install", post(install))
        .route("/api/stickers/packs/{id}/install", delete(uninstall))
        .route("/api/stickers", get(installed))
        .route("/api/stickers/{id}", put(order))
        .route("/api/stickers/{id}", delete(remove))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Sticker {
    pub id: String,
    pub pack_id: String,
    pub media_id: String,
    pub name: String,
    pub sort_order: i64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct StickerPack {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub license: String,
    pub created_at: i64,
    pub sticker_count: i64,
    pub installed: bool,
    pub cover_media_id: String,
    pub stickers: Vec<Sticker>,
}

#[derive(Deserialize)]
pub struct StickerInput {
    pub media_id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Deserialize)]
pub struct PackBody {
    pub name: String,
    pub license: String,
    pub stickers: Vec<StickerInput>,
}

#[derive(Deserialize)]
pub struct OrderBody {
    pub sort_order: i64,
}

impl Platform {
    pub fn create_sticker_pack(&self, owner: &str, body: &PackBody) -> Result<StickerPack> {
        let name = check_text("pack name", &body.name, MAX_PACK_NAME_CHARS)?;
        if name.trim().is_empty() {
            return Err(PlatformError::BadInput("pack name is required"));
        }
        let license = body.license.trim().to_ascii_lowercase();
        if license.chars().count() > MAX_LICENSE_CHARS || !LICENSES.contains(&license.as_str()) {
            return Err(PlatformError::BadInput(
                "sticker pack license must be a known open license",
            ));
        }
        if body.stickers.is_empty() || body.stickers.len() > MAX_STICKERS_PER_PACK {
            return Err(PlatformError::BadInput("sticker count out of range"));
        }
        let mut media_ids = Vec::with_capacity(body.stickers.len());
        for sticker in &body.stickers {
            let media_id = validate::id(&sticker.media_id)?.to_string();
            check_text("sticker name", &sticker.name, MAX_STICKER_NAME_CHARS)?;
            media_ids.push(media_id);
        }
        media_ids.sort();
        let before = media_ids.len();
        media_ids.dedup();
        if media_ids.len() != before {
            return Err(PlatformError::BadInput("duplicate sticker media"));
        }
        self.check_sticker_media(owner, &media_ids)?;
        let now = wxlike_core::types::now_ms() as i64;
        let pack_id = crate::auth::random_hex(8);
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO sticker_packs(id, owner, name, license, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![pack_id, owner, name, license, now],
        )?;
        for (i, sticker) in body.stickers.iter().enumerate() {
            let media_id = validate::id(&sticker.media_id)?;
            let id = crate::auth::random_hex(8);
            tx.execute(
                "INSERT INTO stickers(id, pack_id, media_id, name, sort_order, created_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, pack_id, media_id, sticker.name, i as i64, now],
            )?;
        }
        let pack = pack_row(&tx, &pack_id, owner)?.ok_or(PlatformError::NotFound)?;
        tx.commit()?;
        Ok(pack)
    }

    fn check_sticker_media(&self, owner: &str, media_ids: &[String]) -> Result<()> {
        let c = self.db().lock();
        for media_id in media_ids {
            let row: Option<(String, i64, String)> = c
                .query_row(
                    "SELECT uploader, size, content_type FROM media WHERE id = ?1",
                    params![media_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some((uploader, size, content_type)) = row else {
                return Err(PlatformError::BadInput("unknown sticker media"));
            };
            if uploader != owner {
                return Err(PlatformError::Forbidden);
            }
            if !content_type.starts_with("image/") {
                return Err(PlatformError::BadInput("sticker must be an image"));
            }
            if size as u64 > MAX_STICKER_BYTES {
                return Err(PlatformError::PayloadTooLarge);
            }
        }
        Ok(())
    }

    pub fn sticker_packs(&self, user: &str) -> Result<Vec<StickerPack>> {
        let ids: Vec<String> = {
            let c = self.db().lock();
            let mut stmt = c.prepare(
                "SELECT p.id FROM sticker_packs p
                 WHERE p.owner = ?1
                    OR EXISTS (SELECT 1 FROM user_stickers us
                               JOIN stickers s ON s.id = us.sticker_id
                               WHERE us.user_id = ?1 AND s.pack_id = p.id)
                 ORDER BY p.created_at DESC",
            )?;
            let it = stmt.query_map(params![user], |r| r.get::<_, String>(0))?;
            it.collect::<std::result::Result<Vec<_>, _>>()?
        };
        ids.iter().map(|id| self.sticker_pack(user, id)).collect()
    }

    pub fn sticker_pack(&self, viewer: &str, id: &str) -> Result<StickerPack> {
        let id = validate::id(id)?;
        let c = self.db().lock();
        pack_row(&c, id, viewer)?.ok_or(PlatformError::NotFound)
    }

    /// 安装（可重复执行）：只为缺失的行补记录，不重排用户已有的自定义顺序。
    pub fn install_sticker_pack(&self, user: &str, pack_id: &str) -> Result<usize> {
        let pack_id = validate::id(pack_id)?;
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM sticker_packs WHERE id = ?1",
                params![pack_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(PlatformError::NotFound);
        }
        let mut stmt = tx.prepare(
            "SELECT id, sort_order FROM stickers WHERE pack_id = ?1 ORDER BY sort_order",
        )?;
        let stickers = stmt
            .query_map(params![pack_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut added = 0usize;
        for (sticker_id, sort_order) in stickers {
            added += tx.execute(
                "INSERT INTO user_stickers(user_id, sticker_id, sort_order, installed_at)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(user_id, sticker_id) DO NOTHING",
                params![user, sticker_id, sort_order, now],
            )?;
        }
        tx.commit()?;
        Ok(added)
    }

    pub fn uninstall_sticker_pack(&self, user: &str, pack_id: &str) -> Result<usize> {
        let pack_id = validate::id(pack_id)?;
        let c = self.db().lock();
        Ok(c.execute(
            "DELETE FROM user_stickers
             WHERE user_id = ?1 AND sticker_id IN (SELECT id FROM stickers WHERE pack_id = ?2)",
            params![user, pack_id],
        )?)
    }

    /// 已安装表情（发送面板的数据源），按用户自己的排序返回。
    pub fn installed_stickers(&self, user: &str) -> Result<Vec<Sticker>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
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
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn order_sticker(&self, user: &str, sticker_id: &str, sort_order: i64) -> Result<()> {
        let sticker_id = validate::id(sticker_id)?;
        if !(0..=1_000_000).contains(&sort_order) {
            return Err(PlatformError::BadInput("sort_order out of range"));
        }
        let changed = self.db().lock().execute(
            "UPDATE user_stickers SET sort_order = ?1
             WHERE user_id = ?2 AND sticker_id = ?3",
            params![sort_order, user, sticker_id],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    pub fn remove_sticker(&self, user: &str, sticker_id: &str) -> Result<()> {
        let sticker_id = validate::id(sticker_id)?;
        let changed = self.db().lock().execute(
            "DELETE FROM user_stickers WHERE user_id = ?1 AND sticker_id = ?2",
            params![user, sticker_id],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    /// 只允许包的所有者删除；安装行随 `stickers` 的外键级联清掉。
    pub fn delete_sticker_pack(&self, owner: &str, pack_id: &str) -> Result<()> {
        let pack_id = validate::id(pack_id)?;
        let changed = self.db().lock().execute(
            "DELETE FROM sticker_packs WHERE id = ?1 AND owner = ?2",
            params![pack_id, owner],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }
}

fn pack_row(c: &Connection, id: &str, viewer: &str) -> Result<Option<StickerPack>> {
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

fn check_text(field: &'static str, s: &str, max: usize) -> Result<String> {
    if s.chars().count() > max {
        return Err(PlatformError::BadInput(field));
    }
    if s.chars().any(char::is_control) {
        return Err(PlatformError::BadInput(field));
    }
    Ok(s.to_string())
}

async fn create_pack(
    State(platform): State<PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<PackBody>,
) -> std::result::Result<(StatusCode, Json<StickerPack>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(platform.create_sticker_pack(&authed.user_id, &body)?),
    ))
}

async fn list_packs(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        serde_json::json!({ "packs": platform.sticker_packs(&authed.user_id)? }),
    ))
}

async fn one_pack(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<StickerPack>, ApiError> {
    Ok(Json(platform.sticker_pack(&authed.user_id, &id)?))
}

async fn remove_pack(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.delete_sticker_pack(&authed.user_id, &id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn install(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let added = platform.install_sticker_pack(&authed.user_id, &id)?;
    let pack = platform.sticker_pack(&authed.user_id, &id)?;
    Ok(Json(serde_json::json!({ "added": added, "pack": pack })))
}

async fn uninstall(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let removed = platform.uninstall_sticker_pack(&authed.user_id, &id)?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

async fn installed(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        serde_json::json!({ "stickers": platform.installed_stickers(&authed.user_id)? }),
    ))
}

async fn order(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<OrderBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.order_sticker(&authed.user_id, &id, body.sort_order)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.remove_sticker(&authed.user_id, &id)?;
    Ok(StatusCode::NO_CONTENT)
}
