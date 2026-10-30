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

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::Serialize;

use crate::auth::Authed;
use crate::db::offload;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
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
        // 读成员列表：P3 命令面（`group.members`）与客户端成员页都要它
        .route("/api/groups/{group_id}/members", get(list_members))
        .route("/api/groups/{group_id}/leave", post(leave))
        .route(
            "/api/groups/{group_id}/members/{user_id}",
            delete(remove_member),
        )
        .route("/api/groups/{group_id}/remark-mine", put(remark_mine))
        .route("/api/groups/{group_id}/visibility", put(set_visibility))
        .route("/api/groups/{group_id}/dissolve", post(dissolve))
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

/// `GET /api/groups/{id}/members` 的翻页参数（#331 第 3 条，游标下推）。
/// 两个都不给 = 老行为：整份名单。
#[derive(serde::Deserialize)]
pub struct ListMembersQuery {
    pub after_user: Option<String>,
    pub limit: Option<i64>,
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
    /// 可见性（G02-02）：private 的群不进任何"发现"面。
    pub visibility: String,
    /// 已解散：软删，历史消息仍在，但不能再加人 / 发消息。
    pub dissolved: bool,
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
    /// 幂等重放：同一个 key 已经执行过就返回**当时**的结果。
    ///
    /// 为什么放在平台层而不是命令层：重试可能来自 CLI、MCP、UI 任意一个入口，
    /// 只有服务端记住才有效。键由调用方给（`Idempotency-Key` 头）。
    pub fn replay_idempotent(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let c = self.db().lock();
        let row: Option<String> = c
            .query_row(
                "SELECT response FROM idempotency WHERE key = ?1",
                rusqlite::params![key],
                |r| r.get(0),
            )
            .optional()?;
        drop(c);
        match row {
            Some(text) => Ok(Some(
                serde_json::from_str(&text).map_err(PlatformError::Codec)?,
            )),
            None => Ok(None),
        }
    }

    pub fn remember_idempotent(&self, key: &str, response: &serde_json::Value) -> Result<()> {
        self.db().lock().execute(
            "INSERT INTO idempotency(key, response, created_at) VALUES(?1, ?2, ?3)
             ON CONFLICT(key) DO NOTHING",
            rusqlite::params![
                key,
                serde_json::to_string(response).map_err(PlatformError::Codec)?,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        Ok(())
    }

    /// 把群成员快照同步进信箱（P3：信箱成为成员与顺序的唯一真相）。
    ///
    /// 成员变更是低频操作，直接**全量重置**而不是增量：漏写一条增量会让某个
    /// 人永远收不到消息，而全量重置最多多花几毫秒。宁可慢一点，不要漏。
    ///
    /// 没有信箱（纯控制面测试/演示）时静默跳过——那些场景没有投递。
    pub(crate) fn sync_mailbox_members(&self, group_id: &str) {
        let Some(store) = self.message_store.as_ref() else {
            return;
        };
        let Ok(ids) = self.group_member_ids(group_id) else {
            return;
        };
        if let Err(err) =
            store.reset_member_snapshot((wxlike_message::OwnerType::Group, group_id), &ids)
        {
            // 同步失败不该让建群/拉人失败：下次成员变更会再重置一次
            eprintln!("[platform] 信箱成员快照同步失败（{group_id}）: {err}");
        }
    }

    /// 群成员 id 列表（不含角色等展示字段）。
    fn group_member_ids(&self, group_id: &str) -> Result<Vec<String>> {
        let c = self.db().lock();
        let mut stmt =
            c.prepare("SELECT user_id FROM group_members WHERE group_id = ?1 ORDER BY joined_at")?;
        let rows = stmt
            .query_map(rusqlite::params![group_id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

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
        self.set_owner_kind(&id, wxlike_message::OwnerType::Group)?;
        self.sync_mailbox_members(&id);
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

    /// 游标页：`group_members` 的下推版（#331 第 3 条）。
    ///
    /// 排序与全量版一致（`joined_at, user_id`），游标是上一页最后一行的
    /// `user_id`：先把游标行的 `joined_at` 查出来，再用键集条件
    /// `(joined_at, user_id) > 游标行` 一次取回这一页——大群不再整表取回再切。
    /// 语义与执行器旧的内存切片（#315）逐条一致：
    /// * `after_user` 缺省/为空 = 从第一页开始；
    /// * 游标已不在群里（被移出）= **空页**，不是从头再给一遍
    ///   （从头给会让翻页游标永远打转）；
    /// * `limit = None` = 游标之后全给，`Some(0)` = 空页，负数按 0 算。
    pub fn group_members_page(
        &self,
        group_id: &str,
        after_user: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<MemberRow>> {
        let sql_limit: i64 = match limit {
            None => -1,
            Some(n) => n.max(0),
        };
        let map_row = |r: &rusqlite::Row<'_>| {
            Ok(MemberRow {
                user_id: r.get(0)?,
                role: r.get(1)?,
                my_remark: r.get(2)?,
                joined_at: r.get(3)?,
            })
        };
        let c = self.db().lock();
        let rows = match after_user.filter(|u| !u.is_empty()) {
            Some(after) => {
                let joined: Option<i64> = c
                    .query_row(
                        "SELECT joined_at FROM group_members
                         WHERE group_id = ?1 AND user_id = ?2",
                        params![group_id, after],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(joined) = joined else {
                    return Ok(Vec::new());
                };
                let mut stmt = c.prepare(
                    "SELECT user_id, role, my_remark, joined_at FROM group_members
                     WHERE group_id = ?1
                       AND (joined_at > ?2 OR (joined_at = ?2 AND user_id > ?3))
                     ORDER BY joined_at, user_id LIMIT ?4",
                )?;
                let mapped =
                    stmt.query_map(params![group_id, joined, after, sql_limit], map_row)?;
                mapped.collect::<std::result::Result<Vec<_>, _>>()?
            }
            None => {
                let mut stmt = c.prepare(
                    "SELECT user_id, role, my_remark, joined_at FROM group_members
                     WHERE group_id = ?1 ORDER BY joined_at, user_id LIMIT ?2",
                )?;
                let mapped = stmt.query_map(params![group_id, sql_limit], map_row)?;
                mapped.collect::<std::result::Result<Vec<_>, _>>()?
            }
        };
        Ok(rows)
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
        if added > 0 {
            self.sync_mailbox_members(&group_id);
        }
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
        self.sync_mailbox_members(&group_id);
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
        self.sync_mailbox_members(&group_id);
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

/// 读 `Idempotency-Key` 头（建群与加人共用）：缺失/空/非法都当没带。
fn idempotency_key(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(str::to_string)
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
    headers: axum::http::HeaderMap,
    ApiJson(body): ApiJson<CreateGroup>,
) -> std::result::Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // agent 会重试：带同一个 Idempotency-Key 时返回第一次的结果，不新建群
    let idem = idempotency_key(&headers);
    // ADR-28 样板 ③「群管理写」：每一次同步 SQL（幂等重放 / 建群 + 成员行 /
    // 审计与幂等回执）都整段进阻塞池，handler 只在两次之间 `await` 扇出。
    let replay_key = idem.clone();
    if let Some(prev) = offload(&platform, move |p| match &replay_key {
        Some(key) => p.replay_idempotent(key),
        None => Ok(None),
    })
    .await?
    {
        return Ok((StatusCode::OK, Json(prev)));
    }

    let owner = authed.user_id.clone();
    let name = body.name.clone();
    let member_ids = body.member_ids.clone();
    let group_name = name.clone();
    let (id, creator_name) = offload(&platform, move |p| {
        let id = p.create_group(&owner, &group_name, &member_ids)?;
        Ok((id, p.profile_of(&owner).map(|x| x.name).unwrap_or_default()))
    })
    .await?;
    let others: Vec<String> = body.member_ids;
    if !others.is_empty() {
        let env = group_notice(
            &id,
            &authed.user_id,
            &others,
            &format!("{creator_name} 创建了群聊「{}」", name),
        );
        platform.emit(env).await;
    }
    let reply = serde_json::json!({ "group_id": id, "members": others.len() + 1 });
    if let Some(key) = idem {
        let stored = reply.clone();
        offload(&platform, move |p| p.remember_idempotent(&key, &stored)).await?;
    }
    Ok((StatusCode::OK, Json(reply)))
}

async fn add_members(
    State(platform): State<PlatformState>,
    authed: Authed,
    headers: axum::http::HeaderMap,
    Path(group_id): Path<String>,
    ApiJson(body): ApiJson<MemberIds>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    // #331 第 2 条：读 `Idempotency-Key` 做重放。加人本身靠主键 +
    // ON CONFLICT 天然幂等，但裸重试第二次拿到的是 `added=0`，调用方
    // 分不清「我上次加过了」和「这人本来就在」——同键重试返回**第一次**
    // 的结果。存储键带操作/群/操作者前缀：`idempotency` 是全域一张表，
    // 裸键会和建群、转账的键撞车。重放在权限校验之前（与建群同形状），
    // 只是把当时的回执再给一遍，不再发通知。
    let idem = idempotency_key(&headers)
        .map(|k| format!("group.add_members:{group_id}:{}:{k}", authed.user_id));
    let replay_key = idem.clone();
    if let Some(prev) = offload(&platform, move |p| match &replay_key {
        Some(key) => p.replay_idempotent(key),
        None => Ok(None),
    })
    .await?
    {
        return Ok(Json(prev));
    }

    // ADR-28 样板 ③「群管理写」：权限 → 人数上限 → 逐行 INSERT → 缓存失效 +
    // 信箱成员快照，全链在一个阻塞任务里跑完（顺序与改前一致）。
    let actor = authed.user_id.clone();
    let user_ids = body.user_ids.clone();
    let conv = group_id.clone();
    let (added, members, name) = offload(&platform, move |p| {
        let added = p.add_members(&conv, &actor, &user_ids)?;
        let members = p.group_members(&conv)?;
        let name = p.profile_of(&actor).map(|m| m.name).unwrap_or_default();
        Ok((added, members, name))
    })
    .await?;
    if added > 0 {
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
    let reply = serde_json::json!({ "added": added });
    if let Some(key) = idem {
        let stored = reply.clone();
        offload(&platform, move |p| p.remember_idempotent(&key, &stored)).await?;
    }
    Ok(Json(reply))
}

async fn remove_member(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path((group_id, user_id)): Path<(String, String)>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ③「群管理写」：删成员行 + 名单读取 + 显示名一次阻塞完成。
    let actor = authed.user_id.clone();
    let conv = group_id.clone();
    let target = user_id.clone();
    let (members, name) = offload(&platform, move |p| {
        p.remove_member(&conv, &actor, &target)?;
        let members = p.group_members(&conv)?;
        Ok((
            members,
            p.profile_of(&actor).map(|x| x.name).unwrap_or_default(),
        ))
    })
    .await?;
    let recipients: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
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
    // ADR-28 样板 ③「群管理写」：退群 = 校验角色 + 删行 + 缓存失效 + 快照同步。
    let actor = authed.user_id.clone();
    let conv = group_id.clone();
    let (members, name) = offload(&platform, move |p| {
        p.leave_group(&conv, &actor)?;
        let members = p.group_members(&conv)?;
        Ok((
            members,
            p.profile_of(&actor).map(|x| x.name).unwrap_or_default(),
        ))
    })
    .await?;
    let recipients: Vec<String> = members.iter().map(|m| m.user_id.clone()).collect();
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
    ApiJson(body): ApiJson<RemarkMine>,
) -> std::result::Result<StatusCode, ApiError> {
    // ADR-28 样板 ③「群管理写」：改自己在那一行的备注（单条 UPDATE）。
    let actor = authed.user_id.clone();
    let remark = body.remark.clone();
    offload(&platform, move |p| {
        p.set_my_remark(&group_id, &actor, &remark)
    })
    .await?;
    Ok(StatusCode::OK)
}

impl Platform {
    /// 群可见性：仅群主。写入 `group_meta`（关系域的群元数据表）。
    ///
    /// `private` 的含义是"不出现在发现 / 搜索里"，不是"成员看不到"——
    /// 非成员读群信息本来就是 403（见 `group_info`）。
    pub fn set_group_visibility(
        &self,
        actor: &str,
        group_id: &str,
        visibility: &str,
    ) -> Result<()> {
        let vis = match visibility {
            "public" | "private" => visibility,
            _ => return Err(PlatformError::BadInput("visibility")),
        };
        let c = self.db().lock();
        let owner: Option<String> = c
            .query_row(
                "SELECT owner_uid FROM groups WHERE id = ?1",
                params![group_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(owner) = owner else {
            return Err(PlatformError::NotFound);
        };
        if owner != actor {
            return Err(PlatformError::Forbidden);
        }
        c.execute(
            "INSERT INTO group_meta(group_id, visibility) VALUES(?1, ?2) \
             ON CONFLICT(group_id) DO UPDATE SET visibility = excluded.visibility",
            params![group_id, vis],
        )?;
        Ok(())
    }

    /// 解散群：仅群主。软删（`group_meta.dissolved_at`），返回需要通知的成员。
    ///
    /// 不删 `groups` / `group_members` / 消息：解散是可追溯事件，历史消息仍要能读；
    /// "不能再写"由 `dissolved` 判定，不靠删数据。
    pub fn dissolve_group(&self, actor: &str, group_id: &str) -> Result<Vec<String>> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        let owner: Option<String> = c
            .query_row(
                "SELECT owner_uid FROM groups WHERE id = ?1",
                params![group_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(owner) = owner else {
            return Err(PlatformError::NotFound);
        };
        if owner != actor {
            return Err(PlatformError::Forbidden);
        }
        c.execute(
            "INSERT INTO group_meta(group_id, visibility, dissolved_at, dissolved_by) \
             VALUES(?1, 'public', ?2, ?3) \
             ON CONFLICT(group_id) DO UPDATE SET dissolved_at = excluded.dissolved_at, \
               dissolved_by = excluded.dissolved_by",
            params![group_id, now, actor],
        )?;
        let mut stmt = c.prepare("SELECT user_id FROM group_members WHERE group_id = ?1")?;
        let rows = stmt
            .query_map(params![group_id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 群元数据（可见性 / 解散）—— 缺行时按 public、未解散处理。
    pub fn group_meta_of(&self, group_id: &str) -> Result<(String, bool)> {
        let c = self.db().lock();
        let row: Option<(String, Option<i64>)> = c
            .query_row(
                "SELECT visibility, dissolved_at FROM group_meta WHERE group_id = ?1",
                params![group_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(match row {
            Some((vis, dissolved)) => (vis, dissolved.is_some()),
            None => ("public".to_string(), false),
        })
    }
}

#[derive(serde::Deserialize)]
pub struct VisibilityBody {
    pub visibility: String,
}

async fn set_visibility(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
    ApiJson(body): ApiJson<VisibilityBody>,
) -> std::result::Result<StatusCode, ApiError> {
    let group_id = validate::id(&group_id).map_err(ApiError::from)?.to_string();
    // ADR-28 样板 ③「群管理写」：归属校验 + 元数据 upsert + 审计写，一次阻塞。
    let actor = authed.user_id.clone();
    let visibility = body.visibility.clone();
    let target = group_id.clone();
    offload(&platform, move |p| {
        p.set_group_visibility(&actor, &target, &visibility)?;
        p.audit(&actor, "group.visibility", &target, &visibility);
        Ok(())
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 解散群：群主专属。给每个成员发一条 `group.dissolved` 通知帧。
async fn dissolve(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let group_id = validate::id(&group_id).map_err(ApiError::from)?.to_string();
    // ADR-28 样板 ③「群管理写」：解散写 + 审计写在同一段阻塞里；逐成员扇出仍
    // 留在 await 侧（那是网络投递，不是 SQL）。
    let actor = authed.user_id.clone();
    let target = group_id.clone();
    let members = offload(&platform, move |p| {
        let members = p.dissolve_group(&actor, &target)?;
        p.audit(&actor, "group.dissolve", &target, "");
        Ok(members)
    })
    .await?;
    for member in &members {
        let mut env =
            crate::system_envelope(&format!("sys-{member}"), "system", vec![member.clone()], "");
        env.body = wxlike_core::MsgBody::Custom {
            kind: "group.dissolved".into(),
            payload: serde_json::json!({
                "group_id": group_id,
                "by": authed.user_id,
                "at_ms": wxlike_core::types::now_ms(),
            }),
        };
        platform.emit(env).await;
    }
    Ok(Json(
        serde_json::json!({ "dissolved": true, "notified": members.len() }),
    ))
}

// 群管理的三个读接口（group_info / list_members / my_groups）没在本卡三条样板热点
// 里，逐个点位下面的 `TODO(ADR-28)` 各自记一笔，改造方式与 `dissolve` 相同。
async fn group_info(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
) -> std::result::Result<Json<GroupInfo>, ApiError> {
    // TODO(ADR-28): 一次请求里 4 次上锁（header/role/meta/members）全在 await
    // 上下文里同步跑；改法：整段读包进一次 `offload`。
    let group_id = validate::id(&group_id).map_err(ApiError::from)?.to_string();
    // existence first: "no such group" is a 404, "not yours to read" is a 403
    let (id, name, owner_uid, created_at) = platform
        .group_header(&group_id)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such group"))?;
    let my_role = platform
        .role_of(&group_id, &authed.user_id)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "not a member of this group"))?;
    let (visibility, dissolved) = platform.group_meta_of(&group_id)?;
    Ok(Json(GroupInfo {
        id,
        name,
        owner_uid,
        created_at,
        my_role,
        visibility,
        dissolved,
        members: platform.group_members(&group_id)?,
    }))
}

/// 群成员列表。只对群成员开放——非成员拿到 403 而不是空列表，
/// 否则"这个群有多少人"就成了公开信息。
///
/// 带 `after_user` / `limit` 时走 `group_members_page` 的 SQL 下推
/// （#331 第 3 条）；两个参数都不给时与旧行为逐字一致：整份名单。
async fn list_members(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(group_id): Path<String>,
    Query(q): Query<ListMembersQuery>,
) -> std::result::Result<Json<Vec<MemberRow>>, ApiError> {
    // TODO(ADR-28): 成员名单这条读（含 `is_member` 的存在性校验）在 await 上下文
    // 里上锁同步跑；与 `group_info` 同一类点位。
    if !platform.is_member(&group_id, &authed.user_id)? {
        return Err(PlatformError::Forbidden.into());
    }
    if q.after_user.is_none() && q.limit.is_none() {
        return Ok(Json(platform.group_members(&group_id)?));
    }
    Ok(Json(platform.group_members_page(
        &group_id,
        q.after_user.as_deref(),
        q.limit,
    )?))
}

async fn my_groups(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Vec<GroupSummary>>, ApiError> {
    // TODO(ADR-28): "我的群"这条带子查询的读在 await 上下文里同步上锁；
    // 与 `group_info` / `list_members` 同一类点位。
    Ok(Json(platform.group_summary(&authed.user_id)?))
}
