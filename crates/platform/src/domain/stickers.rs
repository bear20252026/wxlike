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
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::stickers`]。

use crate::error::{PlatformError, Result};
use crate::validate;
use crate::Platform;
use serde::{Deserialize, Serialize};

use super::repo::stickers::{
    delete_pack, delete_user_sticker, insert_pack, insert_sticker, install_sticker, installed_for,
    media_info, pack_exists, pack_row, pack_stickers, packs_for, set_sticker_order, uninstall_pack,
};

pub const MAX_STICKER_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_STICKERS_PER_PACK: usize = 100;
pub const MAX_PACK_NAME_CHARS: usize = 64;
pub const MAX_STICKER_NAME_CHARS: usize = 32;
pub const MAX_LICENSE_CHARS: usize = 64;
const LICENSES: [&str; 4] = ["cc0-1.0", "cc-by-4.0", "cc-by-sa-4.0", "agpl-3.0-only"];

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

impl Platform {
    pub(crate) fn create_sticker_pack(&self, owner: &str, body: &PackBody) -> Result<StickerPack> {
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
        self.db().with_tx(|conn| -> Result<StickerPack> {
            insert_pack(conn, &pack_id, owner, &name, &license, now)?;
            for (i, sticker) in body.stickers.iter().enumerate() {
                let media_id = validate::id(&sticker.media_id)?;
                let id = crate::auth::random_hex(8);
                insert_sticker(conn, &id, &pack_id, media_id, &sticker.name, i as i64, now)?;
            }
            pack_row(conn, &pack_id, owner)?.ok_or(PlatformError::NotFound)
        })
    }

    fn check_sticker_media(&self, owner: &str, media_ids: &[String]) -> Result<()> {
        let c = self.db().acquire();
        for media_id in media_ids {
            let Some(info) = media_info(&c, media_id)? else {
                return Err(PlatformError::BadInput("unknown sticker media"));
            };
            if info.uploader != owner {
                return Err(PlatformError::Forbidden);
            }
            if !info.content_type.starts_with("image/") {
                return Err(PlatformError::BadInput("sticker must be an image"));
            }
            if info.size as u64 > MAX_STICKER_BYTES {
                return Err(PlatformError::PayloadTooLarge);
            }
        }
        Ok(())
    }

    pub(crate) fn sticker_packs(&self, user: &str) -> Result<Vec<StickerPack>> {
        let ids: Vec<String> = {
            let c = self.db().acquire();
            packs_for(&c, user)?
        };
        ids.iter().map(|id| self.sticker_pack(user, id)).collect()
    }

    pub(crate) fn sticker_pack(&self, viewer: &str, id: &str) -> Result<StickerPack> {
        let id = validate::id(id)?;
        let c = self.db().acquire();
        pack_row(&c, id, viewer)?.ok_or(PlatformError::NotFound)
    }

    /// 安装（可重复执行）：只为缺失的行补记录，不重排用户已有的自定义顺序。
    pub(crate) fn install_sticker_pack(&self, user: &str, pack_id: &str) -> Result<usize> {
        let pack_id = validate::id(pack_id)?;
        let now = wxlike_core::types::now_ms() as i64;
        self.db().with_tx(|conn| -> Result<usize> {
            if !pack_exists(conn, pack_id)? {
                return Err(PlatformError::NotFound);
            }
            let stickers = pack_stickers(conn, pack_id)?;
            let mut added = 0usize;
            for (sticker_id, sort_order) in stickers {
                added += install_sticker(conn, user, &sticker_id, sort_order, now)?;
            }
            Ok(added)
        })
    }

    pub(crate) fn uninstall_sticker_pack(&self, user: &str, pack_id: &str) -> Result<usize> {
        let pack_id = validate::id(pack_id)?;
        let c = self.db().acquire();
        uninstall_pack(&c, user, pack_id)
    }

    /// 已安装表情（发送面板的数据源），按用户自己的排序返回。
    pub(crate) fn installed_stickers(&self, user: &str) -> Result<Vec<Sticker>> {
        let c = self.db().acquire();
        installed_for(&c, user)
    }

    pub(crate) fn order_sticker(
        &self,
        user: &str,
        sticker_id: &str,
        sort_order: i64,
    ) -> Result<()> {
        let sticker_id = validate::id(sticker_id)?;
        if !(0..=1_000_000).contains(&sort_order) {
            return Err(PlatformError::BadInput("sort_order out of range"));
        }
        let changed = {
            let c = self.db().acquire();
            set_sticker_order(&c, user, sticker_id, sort_order)?
        };
        crate::db::ensure_updated(changed)?;
        Ok(())
    }

    pub(crate) fn remove_sticker(&self, user: &str, sticker_id: &str) -> Result<()> {
        let sticker_id = validate::id(sticker_id)?;
        let changed = {
            let c = self.db().acquire();
            delete_user_sticker(&c, user, sticker_id)?
        };
        crate::db::ensure_updated(changed)?;
        Ok(())
    }

    /// 只允许包的所有者删除；安装行随 `stickers` 的外键级联清掉。
    pub(crate) fn delete_sticker_pack(&self, owner: &str, pack_id: &str) -> Result<()> {
        let pack_id = validate::id(pack_id)?;
        let changed = {
            let c = self.db().acquire();
            delete_pack(&c, owner, pack_id)?
        };
        crate::db::ensure_updated(changed)?;
        Ok(())
    }
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

#[derive(Deserialize)]
pub struct PackBody {
    pub name: String,
    pub license: String,
    pub stickers: Vec<StickerInput>,
}

#[derive(Deserialize)]
pub struct StickerInput {
    pub media_id: String,
    #[serde(default)]
    pub name: String,
}
