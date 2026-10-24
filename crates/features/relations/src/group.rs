//! 群：角色、成员、解散与可见性。
//!
//! 角色只有三级（owner / admin / member），规则全部在服务端：
//!
//! | 动作 | owner | admin | member |
//! |---|---|---|---|
//! | 拉人入群 | ✓ | ✓ | ✗ |
//! | 授予/收回 admin | ✓ | ✗ | ✗ |
//! | 踢人 | 任何人（除自己） | 只能踢 member | ✗ |
//! | 退群 | ✗（须先转让或解散） | ✓ | ✓ |
//! | 解散 | ✓ | ✗ | ✗ |
//!
//! 解散是**软删**：`group_meta.dissolved_at` 落时间戳，成员行保留供审计与
//! 对账；所有写命令都先过 [`require_active`]，解散后的群不能再改。

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::{
    audit, check_text, random_hex, RelError, Result, GROUP_ID_BYTES, MAX_GROUP_MEMBERS,
    MAX_TEXT_CHARS,
};

/// 角色取值（封闭集合，与平台 `group_members.role` 同口径）。
pub const ROLES: [&str; 3] = ["owner", "admin", "member"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Owner,
    Admin,
    Member,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "member" => Some(Self::Member),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "public" => Some(Self::Public),
            "private" => Some(Self::Private),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub owner_uid: String,
    pub visibility: Visibility,
    pub created_at: i64,
    pub dissolved: bool,
}

const SELECT_GROUP: &str = "SELECT g.id, g.name, g.owner_uid, g.created_at, \
     COALESCE(m.visibility, 'public'), m.dissolved_at FROM groups g \
     LEFT JOIN group_meta m ON m.group_id = g.id";

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<Group> {
    let visibility = Visibility::parse(&r.get::<_, String>(4)?).unwrap_or(Visibility::Public);
    let dissolved: Option<i64> = r.get(5)?;
    Ok(Group {
        id: r.get(0)?,
        name: r.get(1)?,
        owner_uid: r.get(2)?,
        created_at: r.get(3)?,
        visibility,
        dissolved: dissolved.is_some(),
    })
}

fn load(conn: &Connection, group_id: &str) -> Result<Group> {
    let sql = format!("{SELECT_GROUP} WHERE g.id = ?1");
    conn.query_row(&sql, params![group_id], row_of)
        .optional()?
        .ok_or(RelError::NotFound("group"))
}

/// 群存在且未解散，否则写入一律失败。
fn require_active(conn: &Connection, group_id: &str) -> Result<Group> {
    let g = load(conn, group_id)?;
    if g.dissolved {
        return Err(RelError::Conflict("group dissolved"));
    }
    Ok(g)
}

/// 建群：owner 自动入群，`members` 去重后一并加入（被拉黑的人仍可同群，
/// 群内关系由消息层按黑名单过滤，不在这里拒绝建群）。
pub fn create(
    conn: &Connection,
    owner: &str,
    name: &str,
    members: &[String],
    visibility: Visibility,
    now: i64,
) -> Result<Group> {
    let name = check_text(name, MAX_TEXT_CHARS, "group name")?;
    if name.is_empty() {
        return Err(RelError::BadInput("group name"));
    }
    let mut unique: Vec<&String> = Vec::new();
    for m in members {
        if m != owner && !unique.contains(&m) {
            unique.push(m);
        }
    }
    if unique.len() + 1 > MAX_GROUP_MEMBERS {
        return Err(RelError::BadInput("too many members"));
    }
    let id = random_hex(GROUP_ID_BYTES);
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO groups(id, name, owner_uid, created_at) VALUES(?1, ?2, ?3, ?4)",
        params![id, name, owner, now],
    )?;
    tx.execute(
        "INSERT INTO group_meta(group_id, visibility) VALUES(?1, ?2)",
        params![id, visibility.as_str()],
    )?;
    join(&tx, &id, owner, Role::Owner, now)?;
    for m in unique {
        join(&tx, &id, m, Role::Member, now)?;
    }
    audit(&tx, owner, &id, "group.create", now)?;
    tx.commit()?;
    load(conn, &id)
}

fn join(conn: &Connection, group_id: &str, user: &str, role: Role, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "INSERT OR IGNORE INTO group_members(group_id, user_id, role, my_remark, joined_at) \
         VALUES(?1, ?2, ?3, '', ?4)",
        params![group_id, user, role.as_str(), now],
    )?)
}

/// 群内角色（非成员返回 `None`）。
pub fn role_of(conn: &Connection, group_id: &str, user: &str) -> Result<Option<Role>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT role FROM group_members WHERE group_id = ?1 AND user_id = ?2",
            params![group_id, user],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw.as_deref().and_then(Role::parse))
}

/// 成员 id 列表（owner 在前，其次按入群时间）。
pub fn members(conn: &Connection, group_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT user_id FROM group_members WHERE group_id = ?1 \
         ORDER BY CASE role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 ELSE 2 END, joined_at ASC",
    )?;
    let rows = stmt.query_map(params![group_id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// 拉人入群（owner/admin）。已在群里的人被静默跳过（幂等）。
pub fn add_members(
    conn: &Connection,
    actor: &str,
    group_id: &str,
    targets: &[String],
    now: i64,
) -> Result<usize> {
    require_active(conn, group_id)?;
    let role = role_of(conn, group_id, actor)?.ok_or(RelError::Forbidden("not a member"))?;
    if role == Role::Member {
        return Err(RelError::Forbidden("requires admin"));
    }
    let current = members(conn, group_id)?.len();
    let fresh = targets.iter().filter(|t| !t.is_empty()).count();
    if current + fresh > MAX_GROUP_MEMBERS {
        return Err(RelError::BadInput("too many members"));
    }
    let tx = conn.unchecked_transaction()?;
    let mut added = 0;
    for target in targets {
        if target.is_empty() {
            continue;
        }
        added += join(&tx, group_id, target, Role::Member, now)?;
    }
    audit(&tx, actor, group_id, "group.add", now)?;
    tx.commit()?;
    Ok(added)
}

/// 授予/收回角色。只有 owner 能改；把某人设为 owner 即**转让**，原 owner 降为 admin。
pub fn set_role(
    conn: &Connection,
    actor: &str,
    group_id: &str,
    target: &str,
    role: Role,
    now: i64,
) -> Result<()> {
    require_active(conn, group_id)?;
    let actor_role = role_of(conn, group_id, actor)?.ok_or(RelError::Forbidden("not a member"))?;
    if actor_role != Role::Owner {
        return Err(RelError::Forbidden("owner only"));
    }
    if target == actor && role != Role::Owner {
        return Err(RelError::Conflict("owner must transfer or dissolve"));
    }
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "UPDATE group_members SET role = ?3 WHERE group_id = ?1 AND user_id = ?2",
        params![group_id, target, role.as_str()],
    )?;
    if n == 0 {
        return Err(RelError::NotFound("member"));
    }
    if role == Role::Owner {
        tx.execute(
            "UPDATE groups SET owner_uid = ?2 WHERE id = ?1",
            params![group_id, target],
        )?;
        tx.execute(
            "UPDATE group_members SET role = 'admin' WHERE group_id = ?1 AND user_id = ?2",
            params![group_id, actor],
        )?;
    }
    audit(&tx, actor, target, "group.role", now)?;
    tx.commit()?;
    Ok(())
}

/// 踢人。owner 可踢任何人（除自己）；admin 只能踢 member。
pub fn kick(conn: &Connection, actor: &str, group_id: &str, target: &str, now: i64) -> Result<()> {
    require_active(conn, group_id)?;
    let actor_role = role_of(conn, group_id, actor)?.ok_or(RelError::Forbidden("not a member"))?;
    let target_role = role_of(conn, group_id, target)?.ok_or(RelError::NotFound("member"))?;
    let allowed = match actor_role {
        Role::Owner => target != actor,
        Role::Admin => target_role == Role::Member,
        Role::Member => false,
    };
    if !allowed {
        return Err(RelError::Forbidden("insufficient role"));
    }
    conn.execute(
        "DELETE FROM group_members WHERE group_id = ?1 AND user_id = ?2",
        params![group_id, target],
    )?;
    audit(conn, actor, target, "group.kick", now)?;
    Ok(())
}

/// 退群。owner 不能直接退（否则群无人负责），必须先转让或解散。
pub fn leave(conn: &Connection, user: &str, group_id: &str, now: i64) -> Result<()> {
    require_active(conn, group_id)?;
    match role_of(conn, group_id, user)? {
        None => return Err(RelError::NotFound("member")),
        Some(Role::Owner) => return Err(RelError::Conflict("owner must transfer or dissolve")),
        Some(_) => {}
    }
    conn.execute(
        "DELETE FROM group_members WHERE group_id = ?1 AND user_id = ?2",
        params![group_id, user],
    )?;
    audit(conn, user, group_id, "group.leave", now)?;
    Ok(())
}

/// 解散（owner）。返回需要通知的成员 id（含 owner）。
pub fn dissolve(conn: &Connection, actor: &str, group_id: &str, now: i64) -> Result<Vec<String>> {
    let group = require_active(conn, group_id)?;
    if group.owner_uid != actor {
        return Err(RelError::Forbidden("owner only"));
    }
    let list = members(conn, group_id)?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO group_meta(group_id, visibility, dissolved_at, dissolved_by) \
         VALUES(?1, ?2, ?3, ?4) \
         ON CONFLICT(group_id) DO UPDATE SET dissolved_at = ?3, dissolved_by = ?4",
        params![group_id, group.visibility.as_str(), now, actor],
    )?;
    audit(&tx, actor, group_id, "group.dissolve", now)?;
    tx.commit()?;
    Ok(list)
}

/// 我参与的群（不含已解散）。
pub fn groups_of(conn: &Connection, user: &str) -> Result<Vec<Group>> {
    let sql = format!(
        "{SELECT_GROUP} JOIN group_members gm ON gm.group_id = g.id AND gm.user_id = ?1 \
         WHERE m.dissolved_at IS NULL ORDER BY g.created_at ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![user], row_of)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// 共同群：**交集**查询，两边都必须是成员。
///
/// 非成员看不到任何行——所以"共同群"不会泄漏私有群的存在；群名是否可见另由
/// [`may_see`] 判定（非成员 + 私有群 = 不可见）。
pub fn common_groups(conn: &Connection, a: &str, b: &str) -> Result<Vec<Group>> {
    let sql = format!(
        "{SELECT_GROUP} \
         JOIN group_members ma ON ma.group_id = g.id AND ma.user_id = ?1 \
         JOIN group_members mb ON mb.group_id = g.id AND mb.user_id = ?2 \
         WHERE m.dissolved_at IS NULL ORDER BY g.created_at ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![a, b], row_of)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// 某人能否看到这个群（成员恒可见；非成员只见公开且未解散的群）。
pub fn may_see(conn: &Connection, viewer: &str, group_id: &str) -> Result<bool> {
    let group = load(conn, group_id)?;
    if role_of(conn, group_id, viewer)?.is_some() {
        return Ok(true);
    }
    Ok(group.visibility == Visibility::Public && !group.dissolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    fn team(conn: &Connection) -> Group {
        create(
            conn,
            "u1",
            "研发群",
            &["u2".to_string(), "u3".to_string()],
            Visibility::Public,
            1_000,
        )
        .unwrap()
    }

    #[test]
    fn create_dedupes_and_sets_owner() {
        let conn = db();
        let g = create(
            &conn,
            "u1",
            "群",
            &["u2".into(), "u2".into(), "u1".into()],
            Visibility::Public,
            1_000,
        )
        .unwrap();
        assert_eq!(members(&conn, &g.id).unwrap(), vec!["u1", "u2"]);
        assert_eq!(role_of(&conn, &g.id, "u1").unwrap(), Some(Role::Owner));
        assert_eq!(role_of(&conn, &g.id, "u9").unwrap(), None);
    }

    #[test]
    fn member_cannot_invite_or_kick() {
        let conn = db();
        let g = team(&conn);
        assert!(matches!(
            add_members(&conn, "u2", &g.id, &["u9".into()], 1_100),
            Err(RelError::Forbidden(_))
        ));
        assert!(matches!(
            kick(&conn, "u2", &g.id, "u3", 1_100),
            Err(RelError::Forbidden(_))
        ));
        assert!(matches!(
            set_role(&conn, "u2", &g.id, "u3", Role::Admin, 1_100),
            Err(RelError::Forbidden(_))
        ));
    }

    #[test]
    fn admin_can_kick_member_but_not_admin() {
        let conn = db();
        let g = team(&conn);
        set_role(&conn, "u1", &g.id, "u2", Role::Admin, 1_100).unwrap();
        kick(&conn, "u2", &g.id, "u3", 1_200).unwrap();
        set_role(&conn, "u1", &g.id, "u4", Role::Admin, 1_200).ok();
        add_members(&conn, "u1", &g.id, &["u4".into()], 1_250).unwrap();
        set_role(&conn, "u1", &g.id, "u4", Role::Admin, 1_260).unwrap();
        assert!(
            matches!(
                kick(&conn, "u2", &g.id, "u4", 1_300),
                Err(RelError::Forbidden(_))
            ),
            "admin 不能踢同级"
        );
        assert!(
            matches!(
                kick(&conn, "u2", &g.id, "u2", 1_300),
                Err(RelError::Forbidden(_))
            ),
            "admin 不能踢自己"
        );
    }

    #[test]
    fn owner_transfer_demotes_old_owner() {
        let conn = db();
        let g = team(&conn);
        set_role(&conn, "u1", &g.id, "u2", Role::Owner, 1_100).unwrap();
        assert_eq!(role_of(&conn, &g.id, "u2").unwrap(), Some(Role::Owner));
        assert_eq!(role_of(&conn, &g.id, "u1").unwrap(), Some(Role::Admin));
        let owner: String = conn
            .query_row(
                "SELECT owner_uid FROM groups WHERE id = ?1",
                params![g.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(owner, "u2");
        assert!(
            matches!(
                set_role(&conn, "u1", &g.id, "u3", Role::Admin, 1_200),
                Err(RelError::Forbidden(_))
            ),
            "降级后不再有改角色的权力"
        );
    }

    #[test]
    fn owner_cannot_leave_but_can_dissolve() {
        let conn = db();
        let g = team(&conn);
        assert!(matches!(
            leave(&conn, "u1", &g.id, 1_100),
            Err(RelError::Conflict(_))
        ));
        leave(&conn, "u2", &g.id, 1_100).unwrap();
        assert_eq!(members(&conn, &g.id).unwrap().len(), 2);
        assert!(matches!(
            dissolve(&conn, "u2", &g.id, 1_200),
            Err(RelError::Forbidden(_))
        ));
        let notified = dissolve(&conn, "u1", &g.id, 1_300).unwrap();
        assert_eq!(notified.len(), 2);
        assert!(matches!(
            add_members(&conn, "u1", &g.id, &["u9".into()], 1_400),
            Err(RelError::Conflict(_))
        ));
        assert!(groups_of(&conn, "u1").unwrap().is_empty());
    }

    #[test]
    fn common_groups_is_an_intersection() {
        let conn = db();
        let g1 = team(&conn);
        create(
            &conn,
            "u1",
            "另一个群",
            &["u3".into()],
            Visibility::Private,
            2_000,
        )
        .unwrap();
        let shared = common_groups(&conn, "u1", "u3").unwrap();
        assert_eq!(shared.len(), 2);
        assert!(common_groups(&conn, "u1", "u9").unwrap().is_empty());
        assert_eq!(shared[0].id, g1.id);
    }

    #[test]
    fn private_group_name_is_hidden_from_outsiders() {
        let conn = db();
        let g = create(
            &conn,
            "u1",
            "私密",
            &["u2".into()],
            Visibility::Private,
            1_000,
        )
        .unwrap();
        assert!(may_see(&conn, "u2", &g.id).unwrap());
        assert!(!may_see(&conn, "u9", &g.id).unwrap());
        assert!(common_groups(&conn, "u2", "u9").unwrap().is_empty());
        let public = team(&conn);
        assert!(may_see(&conn, "u9", &public.id).unwrap());
    }

    #[test]
    fn add_is_idempotent_and_limited() {
        let conn = db();
        let g = team(&conn);
        assert_eq!(
            add_members(&conn, "u1", &g.id, &["u2".into(), "u9".into()], 1_100).unwrap(),
            1,
            "已在群里的人不重复加"
        );
        assert_eq!(members(&conn, &g.id).unwrap().len(), 4);
    }

    #[test]
    fn bad_input() {
        let conn = db();
        assert!(matches!(
            create(&conn, "u1", "  ", &[], Visibility::Public, 1_000),
            Err(RelError::BadInput(_))
        ));
        let long = "x".repeat(MAX_TEXT_CHARS + 1);
        assert!(matches!(
            create(&conn, "u1", &long, &[], Visibility::Public, 1_000),
            Err(RelError::BadInput(_))
        ));
        assert!(role_of(&conn, "missing", "u1").unwrap().is_none());
        assert!(matches!(
            kick(&conn, "u1", "missing", "u2", 1_000),
            Err(RelError::NotFound(_))
        ));
    }
}
