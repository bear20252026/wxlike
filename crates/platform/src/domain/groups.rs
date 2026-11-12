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
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::groups`]。

use crate::error::{PlatformError, Result};
use crate::validate;
use crate::Platform;
use rusqlite::{params, OptionalExtension as _};
use serde::Serialize;

/// Task book A4: adding more members than this is refused.
pub const MAX_GROUP_MEMBERS: usize = 1_000;
pub const ROLE_OWNER: &str = "owner";
pub const ROLE_ADMIN: &str = "admin";
pub const ROLE_MEMBER: &str = "member";

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
        crate::db::ensure_updated(changed)?;
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
pub(crate) fn idempotency_key(headers: &axum::http::HeaderMap) -> Option<String> {
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
