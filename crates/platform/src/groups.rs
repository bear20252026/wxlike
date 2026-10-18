//! WP-04 groups: creation, membership, roles, and the member table the hub
//! routes group messages by.
//!
//! Delivery model (write fan-out, decided by the task book): the hub asks
//! [`Membership::members_of`] for the conversation, drops anything from a
//! non-member before it is stored, and then delivers to each member except the
//! sender — parking frames for members who are offline. Group ids are used
//! directly as `Envelope.conv_id`.
//!
//! The membership list is cached in-process and invalidated on every
//! membership change, which is what A1's "缓存失效正确" test pins down.
//!
//! Note on `my_remark`: it is a column of `group_members`, so it belongs to the
//! member's **own** row and is that member's nickname for the group — not a
//! per-observer alias of another user. `PUT /api/groups/{id}/remark-mine`
//! therefore writes the caller's row, and the roster returned by
//! `GET /api/groups/{id}` shows each member's own chosen nickname.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::Serialize;

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::validate;
use crate::{Platform, PlatformState};

/// Task book A4: adding more members than this is refused.
pub const MAX_GROUP_MEMBERS: usize = 1_000;

pub const ROLE_OWNER: &str = "owner";
pub const ROLE_ADMIN: &str = "admin";
pub const ROLE_MEMBER: &str = "member";

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/groups", post(create_group))
        .route("/api/groups/mine", get(my_groups))
        .route("/api/groups/{group_id}", get(group_info))
        .route("/api/groups/{group_id}/members", post(add_members))
        .route("/api/groups/{group_id}/leave", post(leave))
        .route(
            "/api/groups/{group_id}/members/{user_id}",
            delete(remove_member),
        )
        .route("/api/groups/{group_id}/remark-mine", put(remark_mine))
}

#[derive(serde::Deserialize)]
pub struct CreateGroup {
    pub name: String,
    #[serde(default)]
    pub member_ids: Vec<String>,
}

#[derive(serde::Deserialize)]
pub struct MemberIds {
    pub user_ids: Vec<String>,
}

#[derive(serde::Deserialize)]
pub struct RemarkMine {
    pub remark: String,
}

#[derive(Serialize)]
pub struct GroupInfo {
    pub id: String,
    pub name: String,
    pub owner_uid: String,
    pub created_at: i64,
    pub my_role: String,
    pub members: Vec<MemberRow>,
}

#[derive(Serialize, Clone, PartialEq)]
pub struct MemberRow {
    pub user_id: String,
    pub role: String,
    pub my_remark: String,
    pub joined_at: i64,
}

#[derive(Serialize)]
pub struct GroupSummary {
    pub id: String,
    pub name: String,
    pub owner_uid: String,
    pub member_count: i64,
    pub my_role: String,
}

impl Platform {
    pub fn create_group(&self, owner: &str, name: &str, member_ids: &[String]) -> Result<String> {
        let name = validate::name(name)?;
        let owner = validate::id(owner)?.to_string();
        let id = crate::auth::random_hex(8);
        let now = wxlike_core::types::now_ms() as i64;

        // de-duplicate requested members, keep caller order for the log
        let mut requested: Vec<String> = Vec::with_capacity(member_ids.len());
        for m in member_ids {
            let m = validate::id(m)?.to_string();
            if m != owner && !requested.contains(&m) {
                requested.push(m);
            }
        }
        if requested.len() + 1 > MAX_GROUP_MEMBERS {
            return Err(PlatformError::BadInput("group member limit exceeded"));
        }

        let c = self.db().lock();
        for m in &requested {
            let exists: Option<String> = c
                .query_row("SELECT id FROM users WHERE id = ?1", params![m], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?;
            if exists.is_none() {
                return Err(PlatformError::NotFound);
            }
        }
        c.execute(
            "INSERT INTO groups(id, name, owner_uid, created_at) VALUES(?1, ?2, ?3, ?4)",
            params![id, name, owner, now],
        )?;
        c.execute(
            "INSERT INTO group_members(group_id, user_id, role, my_remark, joined_at)
             VALUES(?1, ?2, ?3, '', ?4)",
            params![id, owner, ROLE_OWNER, now],
        )?;
        for m in &requested {
            c.execute(
                "INSERT INTO group_members(group_id, user_id, role, my_remark, joined_at)
                 VALUES(?1, ?2, ?3, '', ?4)",
                params![id, m, ROLE_MEMBER, now],
            )?;
        }
        drop(c);
        self.invalidate_group(&id);
        Ok(id)
    }

    /// Header row of a group: `(id, name, owner_uid, created_at)`.
    pub fn group_header(&self, group_id: &str) -> Result<Option<(String, String, String, i64)>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT id, name, owner_uid, created_at FROM groups WHERE id = ?1",
            params![group_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?)
    }

    pub fn group_exists(&self, group_id: &str) -> Result<bool> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT 1 FROM groups WHERE id = ?1",
            params![group_id],
            |_| Ok(1i64),
        )
        .optional()?
        .is_some())
    }

    fn role_of_row(&self, group_id: &str, user_id: &str) -> Result<Option<String>> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT role FROM group_members WHERE group_id = ?1 AND user_id = ?2",
            params![group_id, user_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
    }

    pub fn is_member(&self, group_id: &str, user_id: &str) -> Result<bool> {
        Ok(self.role_of_row(group_id, user_id)?.is_some())
    }

    pub fn role_of(&self, group_id: &str, user_id: &str) -> Result<String> {
        self.role_of_row(group_id, user_id)?
            .ok_or(PlatformError::Forbidden)
    }

    pub(crate) fn assert_manager(&self, group_id: &str, user_id: &str) -> Result<String> {
        let role = self.role_of(group_id, user_id)?;
        if role == ROLE_OWNER || role == ROLE_ADMIN {
            Ok(role)
        } else {
            Err(PlatformError::Forbidden)
        }
    }

    pub fn group_members(&self, group_id: &str) -> Result<Vec<MemberRow>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT user_id, role, my_remark, joined_at FROM group_members
             WHERE group_id = ?1 ORDER BY joined_at, user_id",
        )?;
        let rows = stmt.query_map(params![group_id], |r| {
            Ok(MemberRow {
                user_id: r.get(0)?,
                role: r.get(1)?,
                my_remark: r.get(2)?,
                joined_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn group_summary(&self, me: &str) -> Result<Vec<GroupSummary>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT g.id, g.name, g.owner_uid, m.role,
                    (SELECT COUNT(*) FROM group_members gm WHERE gm.group_id = g.id)
             FROM groups g JOIN group_members m ON m.group_id = g.id
             WHERE m.user_id = ?1 ORDER BY g.created_at DESC",
        )?;
        let rows = stmt.query_map(params![me], |r| {
            Ok(GroupSummary {
                id: r.get(0)?,
                name: r.get(1)?,
                owner_uid: r.get(2)?,
                my_role: r.get(3)?,
                member_count: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Returns how many members were actually added (duplicates are idempotent).
    pub fn add_members(&self, group_id: &str, actor: &str, user_ids: &[String]) -> Result<usize> {
        let group_id = validate::id(group_id)?.to_string();
        self.assert_manager(&group_id, actor)?;
        if !self.group_exists(&group_id)? {
            return Err(PlatformError::NotFound);
        }
        let mut cleaned: Vec<String> = Vec::with_capacity(user_ids.len());
        for u in user_ids {
            let u = validate::id(u)?.to_string();
            if !cleaned.contains(&u) {
                cleaned.push(u);
            }
        }

        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        let current: i64 = c.query_row(
            "SELECT COUNT(*) FROM group_members WHERE group_id = ?1",
            params![&group_id],
            |r| r.get(0),
        )?;
        let new_count: usize = (current as usize)
            + cleaned
                .iter()
                .filter(|u| {
                    c.query_row(
                        "SELECT 1 FROM group_members WHERE group_id = ?1 AND user_id = ?2",
                        params![&group_id, u],
                        |_| Ok(1i64),
                    )
                    .optional()
                    .map(|r| r.map(|_| false).unwrap_or(true))
                    .unwrap_or(true)
                })
                .count();
        if new_count > MAX_GROUP_MEMBERS {
            return Err(PlatformError::BadInput("group member limit exceeded"));
        }
        let mut added = 0usize;
        for u in &cleaned {
            let exists: Option<String> = c
                .query_row("SELECT id FROM users WHERE id = ?1", params![u], |r| {
                    r.get::<_, String>(0)
                })
                .optional()?;
            if exists.is_none() {
                return Err(PlatformError::NotFound);
            }
            added += c.execute(
                "INSERT INTO group_members(group_id, user_id, role, my_remark, joined_at)
                 VALUES(?1, ?2, ?3, '', ?4)
                 ON CONFLICT(group_id, user_id) DO NOTHING",
                params![&group_id, u, ROLE_MEMBER, now],
            )?;
        }
        drop(c);
        self.invalidate_group(&group_id);
        Ok(added)
    }

    /// Owner only, per the contract. Removing the owner is refused: the group
    /// would otherwise lose its authority row.
    pub fn remove_member(&self, group_id: &str, actor: &str, target: &str) -> Result<()> {
        let group_id = validate::id(group_id)?.to_string();
        let target = validate::id(target)?.to_string();
        self.assert_owner(&group_id, actor)?;
        let target_role = self.role_of_row(&group_id, &target)?;
        match target_role.as_deref() {
            None => return Err(PlatformError::NotFound),
            Some(ROLE_OWNER) => {
                return Err(PlatformError::BadInput("cannot remove the group owner"))
            }
            _ => {}
        }
        let c = self.db().lock();
        c.execute(
            "DELETE FROM group_members WHERE group_id = ?1 AND user_id = ?2",
            params![&group_id, target],
        )?;
        drop(c);
        self.invalidate_group(&group_id);
        Ok(())
    }

    fn assert_owner(&self, group_id: &str, actor: &str) -> Result<()> {
        if self.role_of(group_id, actor)? == ROLE_OWNER {
            Ok(())
        } else {
            Err(PlatformError::Forbidden)
        }
    }

    /// Self-service exit. The owner cannot simply leave: ownership must move
    /// first, otherwise the group is left unmanageable.
    pub fn leave_group(&self, group_id: &str, actor: &str) -> Result<()> {
        let group_id = validate::id(group_id)?.to_string();
        let role = self.role_of(&group_id, actor)?;
        if role == ROLE_OWNER {
            return Err(PlatformError::BadInput(
                "owner must transfer the group before leaving",
            ));
        }
        let c = self.db().lock();
        c.execute(
            "DELETE FROM group_members WHERE group_id = ?1 AND user_id = ?2",
            params![&group_id, actor],
        )?;
        drop(c);
        self.invalidate_group(&group_id);
        Ok(())
    }

    pub fn set_my_remark(&self, group_id: &str, actor: &str, remark: &str) -> Result<()> {
        let group_id = validate::id(group_id)?.to_string();
        let remark = validate::remark(remark)?.to_string();
        let c = self.db().lock();
        let changed = c.execute(
            "UPDATE group_members SET my_remark = ?1 WHERE group_id = ?2 AND user_id = ?3",
            params![remark, &group_id, actor],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    /// Every group `user_id` belongs to (P1 search authorization: the set of
    /// conversations whose history and search results this user may see).
    pub fn groups_of_user(&self, user_id: &str) -> Result<Vec<String>> {
        let c = self.db().lock();
        let mut stmt =
            c.prepare("SELECT group_id FROM group_members WHERE user_id = ?1 ORDER BY group_id")?;
        let rows = stmt.query_map(params![user_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---- membership cache (used by the hub through Membership) ----

    pub fn cached_members(&self, group_id: &str) -> Result<Option<Vec<String>>> {
        if !self.group_exists(group_id)? {
            return Ok(None);
        }
        let mut guard = self
            .group_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(hit) = guard.get(group_id) {
            return Ok(Some(hit.clone()));
        }
        let members: Vec<String> = self
            .group_members(group_id)?
            .into_iter()
            .map(|m| m.user_id)
            .collect();
        guard.insert(group_id.to_string(), members.clone());
        Ok(Some(members))
    }

    /// Drops the cached roster after any membership change, so the hub's next
    /// lookup cannot serve a stale member list.
    pub fn invalidate_group(&self, group_id: &str) {
        let mut guard = self
            .group_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.remove(group_id);
    }

    /// Entries currently cached (tests assert invalidation dropped them).
    pub fn group_cache_len(&self) -> usize {
        self.group_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

/// Group ops are announced inside the group as a `MsgKind::System` envelope.
pub(crate) fn group_notice(
    group_id: &str,
    actor: &str,
    recipients: &[String],
    text: &str,
) -> wxlike_core::Envelope {
    crate::system_envelope(group_id, actor, recipients.to_vec(), text)
}

async fn create_group(
    State(platform): State<PlatformState>,
    authed: Authed,
    Json(body): Json<CreateGroup>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let id = platform.create_group(&authed.user_id, &body.name, &body.member_ids)?;
    let others: Vec<String> = body.member_ids.to_vec();
    if !others.is_empty() {
        let name = platform
            .profile_of(&authed.user_id)
            .map(|p| p.name)
            .unwrap_or_default();
        let env = group_notice(
            &id,
            &authed.user_id,
            &others,
            &format!("{name} 创建了群聊「{}」", body.name),
        );
        platform.emit(env).await;
    }
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({ "group_id": id, "members": others.len() + 1 })),
    ))
}

async fn add_members(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
    Json(body): Json<MemberIds>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let added = platform.add_members(&group_id, &authed.user_id, &body.user_ids)?;
    if added > 0 {
        let members = platform.group_members(&group_id)?;
        let name = platform
            .profile_of(&authed.user_id)
            .map(|p| p.name)
            .unwrap_or_default();
        let recipients: Vec<String> = members
            .iter()
            .map(|m| m.user_id.clone())
            .filter(|u| *u != authed.user_id)
            .collect();
        if !recipients.is_empty() {
            let env = group_notice(
                &group_id,
                &authed.user_id,
                &recipients,
                &format!("{name} 加入了群聊"),
            );
            platform.emit(env).await;
        }
    }
    Ok(Json(serde_json::json!({ "added": added })))
}

async fn remove_member(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((group_id, user_id)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.remove_member(&group_id, &authed.user_id, &user_id)?;
    let members = platform.group_members(&group_id)?;
    let recipients: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
    let name = platform
        .profile_of(&authed.user_id)
        .map(|p| p.name)
        .unwrap_or_default();
    if !recipients.is_empty() {
        let env = group_notice(
            &group_id,
            &authed.user_id,
            &recipients,
            &format!("{name} 将用户移出群聊"),
        );
        platform.emit(env).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn leave(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.leave_group(&group_id, &authed.user_id)?;
    let members = platform.group_members(&group_id)?;
    let recipients: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
    let name = platform
        .profile_of(&authed.user_id)
        .map(|p| p.name)
        .unwrap_or_default();
    if !recipients.is_empty() {
        let env = group_notice(
            &group_id,
            &authed.user_id,
            &recipients,
            &format!("{name} 退出了群聊"),
        );
        platform.emit(env).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn remark_mine(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
    Json(body): Json<RemarkMine>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.set_my_remark(&group_id, &authed.user_id, &body.remark)?;
    Ok(StatusCode::OK)
}

async fn group_info(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
) -> std::result::Result<Json<GroupInfo>, ApiError> {
    let group_id = validate::id(&group_id).map_err(ApiError::from)?.to_string();
    // existence first: "no such group" is a 404, "not yours to read" is a 403
    let (id, name, owner_uid, created_at) = platform
        .group_header(&group_id)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such group"))?;
    let my_role = platform
        .role_of(&group_id, &authed.user_id)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "not a member of this group"))?;
    Ok(Json(GroupInfo {
        id,
        name,
        owner_uid,
        created_at,
        my_role,
        members: platform.group_members(&group_id)?,
    }))
}

async fn my_groups(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Vec<GroupSummary>>, ApiError> {
    Ok(Json(platform.group_summary(&authed.user_id)?))
}
