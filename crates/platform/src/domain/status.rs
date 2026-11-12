//! G05-06 我的状态 / 好友状态（`/api/statuses`）。
//!
//! **到期由服务端判定**：`expires_at` 在写入时由服务端时钟算出，读路径与
//! 清理作业都用服务端 `now`；客户端改系统时间不会延长状态。清理保留
//! `cleared_at` 行与审计记录，不物理删除（审计与举报追溯需要）。
//!
//! 可见性每次读时计算：owner 自己永远可见；好友关系、双向拉黑、`audience`
//! 与到期状态共同决定别人能不能看到。设置/清除都在事务里完成，并在成功后
//! 通过 `StatusChanged` 系统事件推给好友（在线端立即刷新，离线端拉列表）。
//!
//! 背景图必须先是本人上传、经媒体表校验过的图片；草稿单独存一行，设置
//! 失败时草稿保留，客户端不必重传。
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::status`]。

use crate::error::{PlatformError, Result};
use crate::validate;
use crate::{Platform, PlatformState};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use wxlike_core::tx::ConnectionTx as _;

use super::repo::status::{check_media, clear_owner, insert_status_indexed, status_row};

/// 微信语义：24 小时。客户端不能把它改长。
pub const STATUS_TTL_MS: i64 = 24 * 60 * 60 * 1000;
pub const MAX_STATUS_TEXT_CHARS: usize = 500;
const KINDS: [&str; 3] = ["text", "image", "video"];
const AUDIENCES: [&str; 3] = ["friends", "public", "private"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StatusView {
    pub id: String,
    pub owner: String,
    pub owner_name: String,
    pub kind: String,
    pub text: String,
    pub media_id: String,
    pub background_media_id: String,
    pub audience: String,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
    /// 服务端时间下的剩余时长（秒）；已到期为 0。
    pub seconds_left: i64,
    pub reactions: i64,
    pub my_reaction: bool,
}

fn default_audience() -> String {
    "friends".to_string()
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct BackgroundDraft {
    pub owner: String,
    pub media_id: String,
    pub updated_at: i64,
}

impl Platform {
    /// 好友 id 列表（状态变更事件的收件人集合）。
    pub fn friend_ids(&self, user: &str) -> Result<Vec<String>> {
        let c = self.db().lock();
        let mut stmt = c.prepare("SELECT friend_id FROM friends WHERE user_id = ?1")?;
        let it = stmt.query_map(params![user], |r| r.get::<_, String>(0))?;
        Ok(it.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 设置状态。同一用户旧的活跃状态在**同一事务**里被清掉（含索引与
    /// 收藏撤权），因此两端不会同时看到两条"当前状态"。
    pub fn set_status(&self, owner: &str, body: &StatusBody) -> Result<StatusView> {
        let owner = validate::id(owner)?.to_string();
        let kind = body.kind.as_str();
        if !KINDS.contains(&kind) {
            return Err(PlatformError::BadInput("unknown status kind"));
        }
        let audience = body.audience.trim().to_ascii_lowercase();
        if !AUDIENCES.contains(&audience.as_str()) {
            return Err(PlatformError::BadInput("unknown status audience"));
        }
        check_status_text(&body.text, kind)?;
        let (media_id, background) = resolve_status_media(self, &owner, kind, body)?;
        let now = wxlike_core::types::now_ms() as i64;
        let expires_at = now + STATUS_TTL_MS;
        let id = crate::auth::random_hex(8);
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<StatusView> {
            clear_owner(tx, &owner, now)?;
            if !background.is_empty() {
                // 背景草稿被用掉了；清掉引用，不留下一个“未使用”的草稿。
                tx.execute(
                    "DELETE FROM status_background_drafts WHERE owner = ?1 AND media_id = ?2",
                    params![owner, background],
                )?;
            }
            insert_status_indexed(
                tx,
                StatusInsert {
                    id: &id,
                    owner: &owner,
                    kind,
                    text: &body.text,
                    media_id: &media_id,
                    background: &background,
                    audience: &audience,
                    now,
                    expires_at,
                },
            )?;
            status_row(tx, &owner, &id)?.ok_or(PlatformError::NotFound)
        })
    }

    pub fn clear_status(&self, owner: &str, id: &str) -> Result<()> {
        let id = validate::id(id)?;
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<()> {
            let changed = tx.execute(
                "UPDATE statuses SET cleared_at = ?1, updated_at = ?1
                 WHERE id = ?2 AND owner = ?3 AND cleared_at IS NULL",
                params![now, id, owner],
            )?;
            crate::db::ensure_updated(changed)?;
            crate::domain::repo::content_index::remove(tx, id)?;
            crate::domain::repo::content_index::remove_media_refs(tx, "status", id)?;
            crate::domain::repo::favorites::revoke_source(tx, "status", id)?;
            Ok(())
        })
    }

    pub fn clear_all_statuses(&self, owner: &str) -> Result<usize> {
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<usize> { clear_owner(tx, owner, now) })
    }

    /// 好友 + 自己的可见状态（每人只返回最新一条活跃状态）。
    pub fn statuses(&self, viewer: &str) -> Result<Vec<StatusView>> {
        let now = wxlike_core::types::now_ms() as i64;
        let ids: Vec<String> = {
            let c = self.db().lock();
            let mut stmt = c.prepare(
                "SELECT s.id FROM statuses s
                 WHERE s.cleared_at IS NULL AND s.expires_at > ?2
                   AND (s.owner = ?1
                        OR (s.owner IN (SELECT friend_id FROM friends WHERE user_id = ?1)
                            AND s.audience <> 'private'
                            AND NOT EXISTS (SELECT 1 FROM blocks b
                                 WHERE (b.user_id = ?1 AND b.blocked_id = s.owner)
                                    OR (b.user_id = s.owner AND b.blocked_id = ?1))))
                   AND s.rowid = (SELECT MAX(s2.rowid) FROM statuses s2
                                  WHERE s2.owner = s.owner AND s2.cleared_at IS NULL
                                    AND s2.expires_at > ?2)
                 ORDER BY s.updated_at DESC",
            )?;
            let it = stmt.query_map(params![viewer, now], |r| r.get::<_, String>(0))?;
            it.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if self.status_visible(viewer, &id)? {
                if let Some(view) = self.status(viewer, &id)? {
                    out.push(view);
                }
            }
        }
        Ok(out)
    }

    pub fn status_visible(&self, viewer: &str, id: &str) -> Result<bool> {
        let row: Option<(String, String, i64, Option<i64>)> = {
            let c = self.db().lock();
            c.query_row(
                "SELECT owner, audience, expires_at, cleared_at FROM statuses WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
        };
        let Some((owner, audience, expires_at, cleared_at)) = row else {
            return Ok(false);
        };
        if cleared_at.is_some() || expires_at <= wxlike_core::types::now_ms() as i64 {
            return Ok(false);
        }
        if owner == viewer {
            return Ok(true);
        }
        if self.is_blocked(viewer, &owner)? || self.is_blocked(&owner, viewer)? {
            return Ok(false);
        }
        match audience.as_str() {
            "public" => Ok(true),
            "friends" => self.are_friends(&owner, viewer),
            _ => Ok(false),
        }
    }

    pub fn status(&self, viewer: &str, id: &str) -> Result<Option<StatusView>> {
        let id = validate::id(id)?;
        if !self.status_visible(viewer, id)? {
            return Ok(None);
        }
        let c = self.db().lock();
        status_row(&c, viewer, id)
    }

    pub fn react_status(&self, user: &str, id: &str) -> Result<(bool, i64)> {
        let id = validate::id(id)?;
        if !self.status_visible(user, id)? {
            return Err(PlatformError::NotFound);
        }
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<(bool, i64)> {
            let removed = tx.execute(
                "DELETE FROM status_reactions WHERE status_id = ?1 AND user_id = ?2",
                params![id, user],
            )?;
            let active = if removed == 0 {
                tx.execute(
                    "INSERT INTO status_reactions(status_id, user_id, created_at) VALUES(?1, ?2, ?3)",
                    params![id, user, now],
                )?;
                true
            } else {
                false
            };
            if active {
                if let Some(owner) = tx
                    .query_row(
                        "SELECT owner FROM statuses WHERE id = ?1",
                        params![id],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                {
                    crate::domain::repo::inbox::notify(
                        tx,
                        &owner,
                        "status_reaction",
                        user,
                        "status",
                        id,
                        "回应了你的状态",
                        &format!("heart:status:{id}:{user}"),
                        now,
                    )?;
                }
            }
            let count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM status_reactions WHERE status_id = ?1",
                params![id],
                |r| r.get(0),
            )?;
            Ok((active, count))
        })
    }

    /// 背景草稿：先校验再落行；校验失败不会覆盖已存在的草稿。
    pub fn save_status_background(&self, owner: &str, media_id: &str) -> Result<BackgroundDraft> {
        let media_id = validate::id(media_id)?.to_string();
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        check_media(&c, owner, &media_id, "image")?;
        c.execute(
            "INSERT INTO status_background_drafts(owner, media_id, updated_at)
             VALUES(?1, ?2, ?3)
             ON CONFLICT(owner) DO UPDATE SET media_id = excluded.media_id,
                                              updated_at = excluded.updated_at",
            params![owner, media_id, now],
        )?;
        Ok(BackgroundDraft {
            owner: owner.to_string(),
            media_id,
            updated_at: now,
        })
    }

    pub fn status_background(&self, owner: &str) -> Result<Option<BackgroundDraft>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT owner, media_id, updated_at FROM status_background_drafts WHERE owner = ?1",
            params![owner],
            |r| {
                Ok(BackgroundDraft {
                    owner: r.get(0)?,
                    media_id: r.get(1)?,
                    updated_at: r.get(2)?,
                })
            },
        )
        .optional()?)
    }

    pub fn remove_status_background(&self, owner: &str) -> Result<()> {
        self.db().lock().execute(
            "DELETE FROM status_background_drafts WHERE owner = ?1",
            params![owner],
        )?;
        Ok(())
    }

    /// 服务端到期清理：清可见性、删索引、撤收藏、留审计。返回清理条数。
    /// 由 `spawn_status_sweeper` 定时调用；测试直接调用以获得确定性。
    pub fn sweep_expired_statuses(&self) -> Result<usize> {
        let now = wxlike_core::types::now_ms() as i64;
        let expired: Vec<(String, String)> = {
            let mut c = self.db().lock();
            c.with_tx(|tx| -> Result<Vec<(String, String)>> {
                let mut stmt = tx.prepare(
                    "SELECT id, owner FROM statuses
                     WHERE cleared_at IS NULL AND expires_at <= ?1",
                )?;
                let rows = stmt
                    .query_map(params![now], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                drop(stmt);
                for (id, _) in &rows {
                    tx.execute(
                        "UPDATE statuses SET cleared_at = ?1 WHERE id = ?2 AND cleared_at IS NULL",
                        params![now, id],
                    )?;
                    crate::domain::repo::content_index::remove(tx, id)?;
                    crate::domain::repo::content_index::remove_media_refs(tx, "status", id)?;
                    crate::domain::repo::favorites::revoke_source(tx, "status", id)?;
                }
                Ok(rows)
            })?
        };
        for (id, owner) in &expired {
            self.audit(owner, "status_expire", id, "expires_at reached");
        }
        Ok(expired.len())
    }
}

/// 状态正文闸：限长 + 无控制字符；文字状态还要求非空。
fn check_status_text(text: &str, kind: &str) -> Result<()> {
    if text.chars().count() > MAX_STATUS_TEXT_CHARS
        || text.contains(|c: char| c.is_control() && c != '\n')
    {
        return Err(PlatformError::BadInput("status text malformed"));
    }
    if kind == "text" && text.trim().is_empty() {
        return Err(PlatformError::BadInput("status text is required"));
    }
    Ok(())
}

/// 背景是"草稿消费"：给定了 media 就先验证归属/图片类型，设置成功后由
/// 事务删草稿行。空串 = 不换背景。
fn consume_background(
    platform: &Platform,
    owner: &str,
    background_media_id: &str,
) -> Result<String> {
    if background_media_id.trim().is_empty() {
        return Ok(String::new());
    }
    let id = validate::id(background_media_id)?.to_string();
    {
        let c = platform.db().lock();
        check_media(&c, owner, &id, "image")?;
    }
    Ok(id)
}

/// 状态媒体前置校验：非文本状态验证媒体归属，背景走"草稿消费"。
/// 返回 (media_id, background_media_id)，文本状态媒体为空串。
fn resolve_status_media(
    platform: &Platform,
    owner: &str,
    kind: &str,
    body: &StatusBody,
) -> Result<(String, String)> {
    let media_id = match kind {
        "text" => String::new(),
        _ => {
            let media_id = validate::id(&body.media_id)?.to_string();
            {
                let c = platform.db().lock();
                check_media(&c, owner, &media_id, kind)?;
            }
            media_id
        }
    };
    let background = consume_background(platform, owner, &body.background_media_id)?;
    Ok((media_id, background))
}

/// 新状态行的写入载荷（状态正文 + 索引/引用同步所需字段）。
pub(in crate::domain) struct StatusInsert<'a> {
    pub(crate) id: &'a str,
    pub(crate) owner: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) text: &'a str,
    pub(crate) media_id: &'a str,
    pub(crate) background: &'a str,
    pub(crate) audience: &'a str,
    pub(crate) now: i64,
    pub(crate) expires_at: i64,
}

/// `status_row` 查出的裸行；字段名与 SELECT 列序一一对应。
pub(in crate::domain) struct StatusRow {
    pub(crate) id: String,
    pub(crate) owner: String,
    pub(crate) owner_name: String,
    pub(crate) kind: String,
    pub(crate) text: String,
    pub(crate) media_id: String,
    pub(crate) background_media_id: String,
    pub(crate) audience: String,
    pub(crate) version: i64,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) expires_at: i64,
    pub(crate) reactions: i64,
}

/// `status_row` 里尾部两列的下标（与 SELECT 列序一致）。
pub(in crate::domain) const COL_STATUS_EXPIRES_AT: usize = 11;
pub(in crate::domain) const COL_STATUS_REACTIONS: usize = 12;

/// 定时清理（启动时由装配层调用，与红包退款 sweeper 同一形状）。
fn sweep_and_log(state: &PlatformState) {
    match state.sweep_expired_statuses() {
        Ok(0) => {}
        Ok(n) => println!("[platform] cleared {n} expired status(es)"),
        Err(err) => eprintln!("[platform] status sweep failed: {err}"),
    }
}

pub fn spawn_status_sweeper(
    state: PlatformState,
    every: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            sweep_and_log(&state);
        }
    })
}

// ---- HTTP handlers ----

#[derive(Deserialize)]
pub struct StatusBody {
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub media_id: String,
    #[serde(default)]
    pub background_media_id: String,
    #[serde(default = "default_audience")]
    pub audience: String,
}
