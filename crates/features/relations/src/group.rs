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
//! 对账；所有写命令都先过内部的 `require_active` 闸门，解散后的群不能再改。

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::port::Acs;
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
     m.visibility, m.dissolved_at FROM groups g \
     LEFT JOIN group_meta m ON m.group_id = g.id";

/// 原文读法：`visibility` 取 `Option<String>`，判定留到 [`decode_group`]。
struct RawGroup {
    id: String,
    name: String,
    owner_uid: String,
    created_at: i64,
    visibility: Option<String>,
    dissolved_at: Option<i64>,
}

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawGroup> {
    Ok(RawGroup {
        id: r.get(0)?,
        name: r.get(1)?,
        owner_uid: r.get(2)?,
        created_at: r.get(3)?,
        visibility: r.get(4)?,
        dissolved_at: r.get(5)?,
    })
}

/// 原文 → 领域对象。
///
/// **`visibility` 读不懂时绝不能折成 `Public`**：`may_see` 对非成员的判定就是
/// "公开且未解散"，一个坏掉的私有群一旦折成公开，非成员立刻能看见它——这是
/// 向外敞开的 fail-open，比报错严重得多。
///
/// 只有一种情况容忍缺失：**整行 `group_meta` 不存在**（`NULL`）。那是本平台
/// 引入 `group_meta` 之前建出来的存量群，形状单一且可枚举，按公开处理是
/// 有意的向后兼容；而 `'privat'` 这种**存在但解析不出**的值是新写入的损坏，
/// 必须报错。这两者的区别正是本函数存在的理由。
fn decode_group(raw: RawGroup) -> Result<Group> {
    let visibility = match raw.visibility.as_deref() {
        None => Visibility::Public,
        Some(v) => Visibility::parse(v).ok_or(RelError::Corrupt("group_meta.visibility"))?,
    };
    Ok(Group {
        id: raw.id,
        name: raw.name,
        owner_uid: raw.owner_uid,
        created_at: raw.created_at,
        visibility,
        dissolved: raw.dissolved_at.is_some(),
    })
}

fn load(conn: &Connection, group_id: &str) -> Result<Group> {
    let sql = format!("{SELECT_GROUP} WHERE g.id = ?1");
    conn.query_row(&sql, params![group_id], row_of)
        .optional()?
        .map(decode_group)
        .transpose()?
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
///
/// "这一行存在但 `role` 读不懂"和"这一行不存在"必须分开：把损坏折成 `None`
/// 会让成员被告知"你不是成员"，而排查方向就此指向了错误的地方。
pub fn role_of(conn: &Connection, group_id: &str, user: &str) -> Result<Option<Role>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT role FROM group_members WHERE group_id = ?1 AND user_id = ?2",
            params![group_id, user],
            |r| r.get(0),
        )
        .optional()?;
    match raw {
        None => Ok(None),
        Some(v) => Role::parse(&v)
            .map(Some)
            .ok_or(RelError::Corrupt("group_members.role")),
    }
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

/// 拉人入群。判定全部走位（见 [`may_invite`]）：
/// 发起方要有 `S`，被拉方要允许这个人加 ta（ta 的隐私规则 + 拉黑）。
/// 已在群里的人静默跳过（幂等）。
pub fn add_members(
    conn: &Connection,
    actor: &str,
    group_id: &str,
    targets: &[String],
    now: i64,
) -> Result<usize> {
    require_active(conn, group_id)?;
    if !may_invite(conn, actor, group_id, actor)? {
        // 空列表也要先问发起方有没有 `S`：否则"零个目标"成了一条免检通道。
        return Err(RelError::Forbidden("cannot invite"));
    }
    let fresh: Vec<&String> = targets.iter().filter(|t| !t.is_empty()).collect();
    for target in &fresh {
        if !may_invite(conn, actor, group_id, target)? {
            // 不区分"我没权力拉人"与"对方不接受入群"：两条路只回同一个
            // Forbidden，都不透露目标账号是否存在（防枚举）。
            return Err(RelError::Forbidden("cannot invite"));
        }
    }
    let current = members(conn, group_id)?.len();
    if current + fresh.len() > MAX_GROUP_MEMBERS {
        return Err(RelError::BadInput("too many members"));
    }
    let tx = conn.unchecked_transaction()?;
    let mut added = 0;
    for target in &fresh {
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
    // 角色一改，那一行的 `mode_given` 就按新角色重写成缺省位。
    //
    // 为什么必须重写而不是留着：位是比角色更细的真相，但**角色变更是用户对
    // "这个人是什么"的显式裁决**，留着旧的显式行会出现"降级成 member 却仍带 A"
    // 这种两套真相打架的行（`role_cap` 的读时夹紧只是兜底，不是同步机制）。
    // 副作用是随角色变更一起清掉单独授予的 S——要恢复就再 `grant_member("+S")`。
    let (target_want, _) = stored_member_acs(&tx, group_id, target)?;
    upsert_member_acs(&tx, group_id, target, target_want, acs_of_role(role), now)?;
    if role == Role::Owner {
        let (actor_want, _) = stored_member_acs(&tx, group_id, actor)?;
        upsert_member_acs(&tx, group_id, actor, actor_want, acs_of_role(Role::Admin), now)?;
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
///
/// 判定权交给位模型（[`can_kick`]），这里只保留写入。成员行与位行**一起删**：
/// 残留的 `mode_given` 行会在他被重新拉进群时复活旧的 S/A 特权，
/// 而上游删除订阅时是把两个 mode 一起带走的。
pub fn kick(conn: &Connection, actor: &str, group_id: &str, target: &str, now: i64) -> Result<()> {
    require_active(conn, group_id)?;
    // 读取顺序是钉死的：**先发起方、后目标**。
    // * 发起方那一行存在但 `role` 读不懂 → `Corrupt` 必须先冒出来
    //   （`tests/corrupt_rows.rs`），不能被"目标不在群里"的 NotFound 抢掉；
    // * 目标根本不是成员 → `NotFound`（`tests/group_rbac.rs`）：approver 已经
    //   知道群里有谁，"没这个人"与"你没权力"必须是两种答案。
    role_of(conn, group_id, actor)?.ok_or(RelError::Forbidden("not a member"))?;
    role_of(conn, group_id, target)?.ok_or(RelError::NotFound("member"))?;
    if !can_kick(conn, group_id, actor, target)? {
        return Err(RelError::Forbidden("insufficient role"));
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM group_members WHERE group_id = ?1 AND user_id = ?2",
        params![group_id, target],
    )?;
    forget_member_acs(&tx, group_id, target)?;
    tx.commit()?;
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
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM group_members WHERE group_id = ?1 AND user_id = ?2",
        params![group_id, user],
    )?;
    forget_member_acs(&tx, group_id, user)?;
    tx.commit()?;
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
    rows.collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(decode_group)
        .collect()
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
    rows.collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(decode_group)
        .collect()
}

/// 设置群可见性（只有 owner 能改；`require_active` 之外的唯一入口）。
///
/// 规则放在本 crate 而不是各调用方各写一条 `UPDATE group_meta`，是因为
/// `may_see` 判定的就是这一列：任何"顺手改一下可见性"的写入都必须走同一条
/// 权限判定，否则就会出现成员名单没变、群却对陌生人敞开的路径。
pub fn set_visibility(
    conn: &Connection,
    actor: &str,
    group_id: &str,
    visibility: Visibility,
    now: i64,
) -> Result<Visibility> {
    let group = require_active(conn, group_id)?;
    if group.owner_uid != actor {
        return Err(RelError::Forbidden("owner only"));
    }
    conn.execute(
        "INSERT INTO group_meta(group_id, visibility) VALUES(?1, ?2) \
         ON CONFLICT(group_id) DO UPDATE SET visibility = excluded.visibility",
        params![group_id, visibility.as_str()],
    )?;
    audit(conn, actor, group_id, "group.visibility", now)?;
    Ok(visibility)
}

/// 某人能否看到这个群（累积权限的 `R` 位）。
///
/// 三种"看不到"是三条不同的路径，判定只有一处：非成员 + 私有群（`given` 里
/// 就没有 R）、成员自己把 R 关了（`want` 收窄，见 [`set_member_want`]）、
/// 群不存在（`NotFound`）。前两种都只回 `false`，形状相同——
/// 调用方无法用它试探"这个私有群到底存不存在"。
pub fn may_see(conn: &Connection, viewer: &str, group_id: &str) -> Result<bool> {
    let group = load(conn, group_id)?;
    Ok(member_acs(conn, group_id, viewer, &group)?.is_reader())
}

// ───────────────────────────────────────────────────────────────────────────
// 权限位视图（借 Tinode `AccessMode`，定义见 [`crate::port::Acs`]）
//
// `role` 那一列还得留着——平台其它模块与 `group_members.role` 同口径，改它等于
// 改合同。这一层做的是 role 表达不了的那部分：**逐个成员开/关某些位**
// （只读旁观、免打扰、允许某个人拉人、把删除权单独给出去），
// 以及把"能不能拉人/踢人/转让"从三段 `match role` 升级成上游那种**位判定**。
//
// | 上游判定 | 本 crate 的位判定 |
// |---|---|
// | `modeGiven & modeWant` | `member_acs` |
// | `IsSharer()` → 能不能邀人 | `may_invite` |
// | `IsAdmin()` → 能不能给出非默认权限 | `grant_member` |
// | `IsOwner()` → 能不能转让 | `grant_member` 的 owner 分支 |
// | P2P `modeGiven & ModeCP2P 再补 A` | `friend::grant` 的夹紧 |
// ───────────────────────────────────────────────────────────────────────────

/// 角色 → 缺省授予的位。
///
/// | 角色 | 位 | 串 |
/// |---|---|---|
/// | owner | `JRWPASDO` | 上游 `ModeCFull`（255） |
/// | admin | `JRWPAS` | `ModeCPublic \| ModeApprove`（63） |
/// | member | `JRWP` | `ModeCPublic` **去掉 S**（15） |
///
/// **member 为什么比上游少一个 `S`**：上游 `ModeCPublic` 里普通成员可以邀人，
/// 本平台的产品口径（模块头的权限矩阵）是"member 拉人 = ✗"。缺省位因此收紧一格，
/// 需要放开时由 owner/admin 用 [`grant_member`] 单独给某个人 `+S`——
/// 差异登记在 `third_party/reviews/patches/UP-TINODE-relations-acs.md`。
pub fn acs_of_role(role: Role) -> Acs {
    match role {
        Role::Owner => Acs::FULL,
        Role::Admin => Acs::PUBLIC.with(Acs::APPROVE),
        Role::Member => Acs::PUBLIC.without(Acs::SHARE),
    }
}

/// 某个角色**能被授予**的上限位。
///
/// 显式授予的位串（[`grant_member`]）与读入库存的 `mode_given` 都要过这一层夹：
///
/// * `owner` → 全部（只有 ta 能把 `O` 给出去，见上游 `attempt to transfer
///   ownership by non-owner`）；
/// * `admin` → 除 `O` 之外全开；
/// * `member` → `JRWPS`——比 [`acs_of_role`] 的缺省多一格 `S`。
///
/// `S` 是本平台刻意留出的**比角色更细的那一格**："允许这个成员拉人进群，
/// 但不给他审批/踢人权"。角色枚举表达不了，位能。缺省不开，开了也不会改
/// `role` 标签（`role` 仍是平台其它模块读的兼容列）。
/// `O` 永远不能凭空出现在 member/admin 的行里：库里真出现了就是坏数据，
/// 读的时候直接抹掉——宁可少权限，也不让一行坏数据造出第二个群主。
fn role_cap(role: Role) -> Acs {
    match role {
        Role::Owner => Acs::FULL,
        Role::Admin => Acs::PUBLIC.with(Acs::APPROVE).with(Acs::DELETE),
        Role::Member => Acs::PUBLIC,
    }
}

/// 读 `group_member_acs` 一行；没有行 = `(UNSET, UNSET)`。
///
/// 与 `friend::stored_acs` 同一条纪律：存在但读不懂 → [`RelError::Corrupt`]，
/// 绝不折成"没权限"（会把好数据判成屏蔽）也绝不折成"全权限"。
fn stored_member_acs(conn: &Connection, group_id: &str, user: &str) -> Result<(Acs, Acs)> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT mode_want, mode_given FROM group_member_acs WHERE group_id = ?1 AND user_id = ?2",
            params![group_id, user],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((want, given)) = row else {
        return Ok((Acs::UNSET, Acs::UNSET));
    };
    Ok((
        member_parse(&want, "group_member_acs.mode_want")?,
        member_parse(&given, "group_member_acs.mode_given")?,
    ))
}

fn member_parse(raw: &str, what: &'static str) -> Result<Acs> {
    if raw.is_empty() {
        return Ok(Acs::UNSET);
    }
    Acs::parse(raw).map_err(|_| RelError::Corrupt(what))
}

fn upsert_member_acs(
    conn: &Connection,
    group_id: &str,
    user: &str,
    want: Acs,
    given: Acs,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO group_member_acs(group_id, user_id, mode_want, mode_given, updated_at) \
         VALUES(?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(group_id, user_id) DO UPDATE SET mode_want = ?3, mode_given = ?4, updated_at = ?5",
        params![group_id, user, want.to_flags(), given.to_flags(), now],
    )?;
    Ok(())
}

/// 群授予 `user` 的位（还没算 ta 自己要的）。
///
/// 三档，与上游 `accessFor` + `anotherUserSub` 一致：
/// 1. 显式行里的 `mode_given`；
/// 2. 成员：按角色缺省（[`acs_of_role`]）；解散的群整群降为只读
///    （上游 `isReadOnly()` 之后任何写操作都 `ErrPermissionDenied`）；
/// 3. 非成员：公开且未解散 → `JR`（[`Acs::READ_ONLY`]，看得见进不来），
///    私有 → `N`。
fn given_of_member(conn: &Connection, group: &Group, user: &str) -> Result<Acs> {
    let (_, stored) = stored_member_acs(conn, &group.id, user)?;
    if stored.is_defined() {
        // 显式授予也要被角色上限夹住：member 行里写着 `O` 是坏数据，不是特权。
        let cap = role_cap(role_of(conn, &group.id, user)?.unwrap_or(Role::Member));
        let given = stored.only(cap.bits()).masked();
        return Ok(if group.dissolved {
            given.only(Acs::JOIN | Acs::READ)
        } else {
            given
        });
    }
    match role_of(conn, &group.id, user)? {
        Some(role) => Ok(if group.dissolved {
            acs_of_role(role).only(Acs::JOIN | Acs::READ)
        } else {
            acs_of_role(role)
        }),
        None if group.dissolved => Ok(Acs::NONE),
        None => Ok(if group.visibility == Visibility::Public {
            Acs::READ_ONLY
        } else {
            Acs::NONE
        }),
    }
}

/// 我自己要进来的位（订阅方）。缺省 = 全都要（见 `friend::want_of` 的同款理由）。
pub fn member_want(conn: &Connection, group_id: &str, user: &str) -> Result<Acs> {
    let (want, _) = stored_member_acs(conn, group_id, user)?;
    Ok(if want.is_defined() {
        want.masked()
    } else {
        Acs::FULL
    })
}

/// 累积权限：`given & want`。**"谁能看到谁"在群里的唯一算法**。
pub fn member_acs(conn: &Connection, group_id: &str, user: &str, group: &Group) -> Result<Acs> {
    let given = given_of_member(conn, group, user)?;
    let want = member_want(conn, group_id, user)?;
    Ok(Acs::cumulative(given, want))
}

/// `actor` 能不能把 `target` 拉进群。两个闸门，缺一不可：
///
/// 1. **actor 手里有 `S`**（上游 `IsSharer()`：owner/approver/sharer）；
/// 2. **target 允许 actor 加 ta**（`friend::given_to(target, actor)` 的 `S` 位，
///    即 ta 的隐私规则 + 拉黑）。
///
/// 第 2 条是这次移植真正新增的能力：上游只在群这一侧判 `IsSharer`，
/// 被邀方的意愿走的是 `modeWant`（先加进来再由对方退订）。wxlike 的产品口径
/// 是"入群需要被邀方确认"，所以把判定提前到写入之前——少一次"被莫名拉群"。
pub fn may_invite(conn: &Connection, actor: &str, group_id: &str, target: &str) -> Result<bool> {
    let group = load(conn, group_id)?;
    if group.dissolved {
        return Ok(false);
    }
    let mine = member_acs(conn, group_id, actor, &group)?;
    if !mine.is_sharer() {
        return Ok(false);
    }
    crate::friend::may_add(conn, actor, target)
}

/// `actor` 能不能把 `target` 踢出去（上游的 A/O 判定，替代三段 `match role`）。
///
/// 规则：`actor` 必须有 `A`（approver）；`actor` 不是 owner 时，
/// **不能动同样有 `A` 或 `O` 的人**（admin 只能踢 member）；owner 不能踢自己。
pub fn can_kick(conn: &Connection, group_id: &str, actor: &str, target: &str) -> Result<bool> {
    let group = load(conn, group_id)?;
    // 两边都必须在成员表里。公开群的非成员手里有 `JR`（[`given_of_member`] 的
    // 第三档），那是"能被看见"，不是"在群里"——不先卡成员身份，owner 就能
    // "踢"一个从没进来过的陌生人。
    if role_of(conn, group_id, actor)?.is_none() || role_of(conn, group_id, target)?.is_none() {
        return Ok(false);
    }
    let mine = member_acs(conn, group_id, actor, &group)?;
    let theirs = member_acs(conn, group_id, target, &group)?;
    Ok(actor != target
        && mine.is_approver()
        && theirs.is_joiner()
        && (mine.is_owner() || !(theirs.is_approver() || theirs.is_owner())))
}

/// 逐个成员授予/收窄位。
///
/// 三条上游纪律一条不落（`anotherUserSub`）：
/// * 没有 `S` 的人连邀人都做不了，更别提交改权限；
/// * **只有 O/A 能写明确的位串**（`sharer cannot set explicit modeGiven`）：
///   增量（带 `+`/`-`）谁都能表达"我想要"，整串赋值是特权操作；
/// * **只有 owner 能给出 `O`**（`attempt to transfer ownership by non-owner`），
///   给出 `O` 即转让，原 owner 的 `O` 位连同 `A` 一起收回（上游 `ownerChange`
///   是把 `ModeOwner` 从 want/given 两边 `&^` 掉，本平台让 `role` 同步降级，
///   免得两套真相并存）。
pub fn grant_member(
    conn: &Connection,
    actor: &str,
    group_id: &str,
    target: &str,
    mutation: &str,
    now: i64,
) -> Result<Acs> {
    let group = require_active(conn, group_id)?;
    let mine = member_acs(conn, group_id, actor, &group)?;
    if !mine.is_sharer() {
        return Err(RelError::Forbidden("approver has no permission"));
    }
    let explicit = !mutation.is_empty() && !mutation.contains(['+', '-']);
    if explicit && !mine.is_admin() {
        return Err(RelError::Forbidden("sharer cannot set explicit modeGiven"));
    }
    let (target_want, target_given) = stored_member_acs(conn, group_id, target)?;
    let base = if target_given.is_defined() {
        target_given
    } else {
        given_of_member(conn, &group, target)?
    };
    let mut next = base;
    next.apply_mutation(mutation)?;
    let next = next.masked();
    if !next.is_joiner() && !explicit {
        // `-J` 由 approver 表达"把人踢出去"，走 kick；这里拒绝静默 ban。
        return Err(RelError::BadInput("use kick to revoke join"));
    }
    if next.is_owner() && !mine.is_owner() {
        return Err(RelError::Forbidden("attempt to transfer ownership"));
    }
    // 转让路径唯一允许把 `O` 顶到角色上限之上的情形，而且只有 owner 走得到。
    let transferring = next.is_owner() && group.owner_uid != target;
    let ceiling = role_cap(role_of(conn, group_id, target)?.unwrap_or(Role::Member));
    let ceiling = if transferring {
        ceiling.with(Acs::OWNER)
    } else {
        ceiling
    };
    if next.better_than(ceiling) {
        return Err(RelError::Forbidden("grant exceeds role ceiling"));
    }
    let tx = conn.unchecked_transaction()?;
    upsert_member_acs(&tx, group_id, target, target_want, next, now)?;
    if next.is_owner() && group.owner_uid != target {
        // 转让：老 owner 的位与角色一起降回 admin（见函数头的说明）。
        let (old_want, old_given) = stored_member_acs(&tx, group_id, &group.owner_uid)?;
        let demote = |a: Acs| -> Acs {
            if a.is_defined() {
                a.without(Acs::OWNER).masked()
            } else {
                a
            }
        };
        upsert_member_acs(
            &tx,
            group_id,
            &group.owner_uid,
            demote(old_want),
            demote(old_given),
            now,
        )?;
        tx.execute(
            "UPDATE group_members SET role = 'admin' WHERE group_id = ?1 AND user_id = ?2",
            params![group_id, group.owner_uid],
        )?;
        tx.execute(
            "UPDATE group_members SET role = 'owner' WHERE group_id = ?1 AND user_id = ?2",
            params![group_id, target],
        )?;
        tx.execute(
            "UPDATE groups SET owner_uid = ?2 WHERE id = ?1",
            params![group_id, target],
        )?;
    }
    audit(&tx, actor, target, "group.grant", now)?;
    tx.commit()?;
    Ok(next)
}

/// 自己这一侧改位：退订提示（`-P` = 免打扰）、看不见（`-R`）、自禁（`-J`）。
///
/// 上游在 `ownSub` 里对 `-P` 会先发一条 `off+dis` 在场通知、对丢 J 直接
/// `evictUser`。本平台把这两件事交回调用方（通知走 G03，成员行留着才有审计），
/// 这里只保证**位改对了**并且改完的累积权限如实返回。
pub fn set_member_want(
    conn: &Connection,
    user: &str,
    group_id: &str,
    mutation: &str,
    now: i64,
) -> Result<Acs> {
    let group = require_active(conn, group_id)?;
    if role_of(conn, group_id, user)?.is_none() {
        return Err(RelError::NotFound("member"));
    }
    let (want, given) = stored_member_acs(conn, group_id, user)?;
    let mut next = if want.is_defined() { want } else { Acs::FULL };
    next.apply_mutation(mutation)?;
    upsert_member_acs(conn, group_id, user, next.masked(), given, now)?;
    audit(conn, user, group_id, "group.want", now)?;
    Ok(Acs::cumulative(given_of_member(conn, &group, user)?, next.masked()))
}

/// 清掉一位成员的两个位（踢人/退群时调用，避免残留行在重新入群后复活旧特权）。
fn forget_member_acs(conn: &Connection, group_id: &str, user: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM group_member_acs WHERE group_id = ?1 AND user_id = ?2",
        params![group_id, user],
    )?;
    Ok(())
}

#[cfg(test)]
mod acs_tests {
    use super::*;
    use crate::port::Acs;
    use crate::testing::db;

    fn seed(conn: &Connection) -> Group {
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
    fn role_defaults_map_to_documented_bits() {
        assert_eq!(acs_of_role(Role::Owner), Acs::FULL);
        assert_eq!(acs_of_role(Role::Admin).bits(), 63, "JRWPAS");
        assert_eq!(acs_of_role(Role::Member).bits(), 15, "JRWP：member 没有 S");
        // 权限矩阵的三行必须与位一致，否则表与代码会各自演化。
        assert!(!acs_of_role(Role::Member).is_sharer());
        assert!(acs_of_role(Role::Admin).is_sharer());
        assert!(acs_of_role(Role::Admin).is_approver());
        assert!(!acs_of_role(Role::Admin).is_owner());
    }

    #[test]
    fn outsider_of_public_group_is_reader_only() {
        let conn = db();
        let g = seed(&conn);
        let acs = member_acs(&conn, &g.id, "u9", &g).unwrap();
        assert_eq!(acs, Acs::READ_ONLY, "看得见、进不来、发不了");
        assert!(!acs.is_writer(), "READ_ONLY 里的 J 只表示能被看见，发不了言");
        let secret = create(&conn, "u1", "私密", &[], Visibility::Private, 2_000).unwrap();
        assert_eq!(member_acs(&conn, &secret.id, "u9", &secret).unwrap(), Acs::NONE);
        assert!(!may_see(&conn, "u9", &secret.id).unwrap());
        assert!(may_see(&conn, "u2", &g.id).unwrap());
    }

    #[test]
    fn invite_needs_both_shares() {
        let conn = db();
        let g = seed(&conn);
        // member 没有 S：拉不动人（与权限矩阵、与旧的角色判定同结果）。
        assert!(!may_invite(&conn, "u2", &g.id, "u8").unwrap());
        assert!(may_invite(&conn, "u1", &g.id, "u8").unwrap());
        // 给 u2 单开 S 位之后就能拉人——这是角色枚举做不到的那一格。
        grant_member(&conn, "u1", &g.id, "u2", "+S", 1_100).unwrap();
        assert!(may_invite(&conn, "u2", &g.id, "u8").unwrap());
        // 被邀方自己关掉了"允许别人加我"：拉人方有 S 也不行。
        crate::friend::set_policy(
            &conn,
            "u8",
            crate::port::RelationLevel::Anon,
            "-S",
            1_200,
        )
        .unwrap();
        assert!(!may_invite(&conn, "u1", &g.id, "u8").unwrap());
        assert!(!crate::friend::may_add(&conn, "u1", "u8").unwrap());
    }

    #[test]
    fn only_admin_writes_explicit_mode_and_only_owner_transfers() {
        let conn = db();
        let g = seed(&conn);
        grant_member(&conn, "u1", &g.id, "u2", "+S", 1_100).unwrap();
        let e = grant_member(&conn, "u2", &g.id, "u3", "JR", 1_200);
        assert!(
            matches!(e, Err(RelError::Forbidden(_))),
            "sharer 不能写明确位串（上游同一条）"
        );
        let e = grant_member(&conn, "u2", &g.id, "u3", "+D", 1_200);
        assert!(
            matches!(e, Err(RelError::Forbidden(_))),
            "u2 有 S 能拉人，但 D 超出 member 的角色上限"
        );
        let e = grant_member(&conn, "u1", &g.id, "u3", "JRWPASDO", 1_300);
        assert!(
            matches!(e, Err(RelError::Forbidden(_))),
            "转让只能给 O 这一格，整串塞满 A/D 一样越权"
        );
        let e = grant_member(&conn, "u1", &g.id, "u3", "-J", 1_400);
        assert!(
            matches!(e, Err(RelError::BadInput(_))),
            "静默 ban 走 kick，不走增量"
        );
    }

    #[test]
    fn ownership_transfer_strips_old_owner_bits() {
        let conn = db();
        let g = seed(&conn);
        let granted = grant_member(&conn, "u1", &g.id, "u2", "+O", 1_100).unwrap();
        assert!(granted.is_owner());
        assert_eq!(role_of(&conn, &g.id, "u2").unwrap(), Some(Role::Owner));
        assert_eq!(role_of(&conn, &g.id, "u1").unwrap(), Some(Role::Admin));
        let old = member_acs(&conn, &g.id, "u1", &load(&conn, &g.id).unwrap()).unwrap();
        assert!(!old.is_owner(), "老 owner 的 O 必须收回，不能两个 owner");
        assert!(old.is_approver(), "降级为 admin：仍保留 A/S");
    }

    #[test]
    fn kick_is_decided_by_bits() {
        let conn = db();
        let g = seed(&conn);
        assert!(can_kick(&conn, &g.id, "u1", "u2").unwrap());
        assert!(!can_kick(&conn, &g.id, "u1", "u1").unwrap(), "owner 不能踢自己");
        assert!(!can_kick(&conn, &g.id, "u2", "u3").unwrap(), "member 没有 A");
        set_role(&conn, "u1", &g.id, "u2", Role::Admin, 1_100).unwrap();
        assert!(can_kick(&conn, &g.id, "u2", "u3").unwrap());
        assert!(!can_kick(&conn, &g.id, "u2", "u1").unwrap(), "不能动 owner");
        assert!(!can_kick(&conn, &g.id, "u2", "u2").unwrap());
    }

    #[test]
    fn want_can_narrow_given_but_never_widen() {
        let conn = db();
        let g = seed(&conn);
        // 关掉提示（免打扰）：还在群里、还能读写，只是不再收 presence。
        let acc = set_member_want(&conn, "u2", &g.id, "-P", 1_100).unwrap();
        assert!(!acc.is_presencer() && acc.is_reader() && acc.is_writer());
        // 关掉 R：等同"看不见这个群"，但成员行还在（may_see 走累积权限）。
        set_member_want(&conn, "u3", &g.id, "-R", 1_200).unwrap();
        assert!(!may_see(&conn, "u3", &g.id).unwrap());
        // 反向：want 里写 S 不会凭空得到 S——given 才是权力来源。
        set_member_want(&conn, "u3", &g.id, "+S", 1_300).unwrap();
        let group = load(&conn, &g.id).unwrap();
        assert!(!member_acs(&conn, &g.id, "u3", &group).unwrap().is_sharer());
        assert!(matches!(
            set_member_want(&conn, "u9", &g.id, "-P", 1_400),
            Err(RelError::NotFound(_))
        ));
    }

    #[test]
    fn explicit_grant_is_capped_by_role() {
        let conn = db();
        let g = seed(&conn);
        // 直接往 member 行里写一个 member 本不该有的位：被角色夹住。
        // 一行里既有合法位（JRW）又有越权位（O）：越权的那格抹掉，合法的那几格留。
        let poisoned = Acs::parse("JRW").unwrap().with(Acs::OWNER);
        upsert_member_acs(&conn, &g.id, "u2", Acs::UNSET, poisoned, 1_100).unwrap();
        let group = load(&conn, &g.id).unwrap();
        let acs = member_acs(&conn, &g.id, "u2", &group).unwrap();
        assert!(!acs.is_owner() && !acs.is_approver(), "越权位不产生特权");
        assert!(acs.is_reader() && acs.is_writer(), "合法位不该被一行坏数据连坐");
        // 纯越权的一行（只有 O）夹完就是零位：宁可不给，也不凭空造出第二个 owner。
        upsert_member_acs(&conn, &g.id, "u3", Acs::UNSET, Acs::parse("O").unwrap(), 1_100).unwrap();
        let acs = member_acs(&conn, &g.id, "u3", &group).unwrap();
        assert!(acs.is_zero(), "只写着 O 的 member 行 → 无任何权限");
    }

    #[test]
    fn corrupt_acs_string_is_not_folded() {
        let conn = db();
        let g = seed(&conn);
        conn.execute(
            "INSERT INTO friend_acs(user_id, peer_id, mode_want, mode_given, updated_at) \
             VALUES('u2','u1','','Z',1_300)",
            [],
        )
        .unwrap();
        assert!(matches!(
            crate::friend::acs_toward(&conn, "u1", "u2"),
            Err(RelError::Corrupt(_))
        ));
        conn.execute(
            "INSERT INTO group_member_acs(group_id, user_id, mode_want, mode_given, updated_at) \
             VALUES(?1,'u2','ZQ','',1_300)",
            params![g.id],
        )
        .unwrap();
        assert!(matches!(
            set_member_want(&conn, "u2", &g.id, "-P", 1_200),
            Err(RelError::Corrupt(_))
        ));
    }

    #[test]
    fn dissolved_group_drops_to_read_only_for_members() {
        let conn = db();
        let g = seed(&conn);
        dissolve(&conn, "u1", &g.id, 1_100).unwrap();
        let group = load(&conn, &g.id).unwrap();
        let acs = member_acs(&conn, &g.id, "u2", &group).unwrap();
        assert!(acs.is_reader() && !acs.is_writer(), "解散后只读");
        assert!(!may_invite(&conn, "u1", &g.id, "u8").unwrap());
    }

    #[test]
    fn kick_and_leave_clear_acs_rows() {
        let conn = db();
        let g = seed(&conn);
        grant_member(&conn, "u1", &g.id, "u2", "+S", 1_100).unwrap();
        kick(&conn, "u1", &g.id, "u2", 1_200).unwrap();
        let left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM group_member_acs WHERE group_id = ?1 AND user_id = 'u2'",
                params![g.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "残留位行会在重新入群时复活旧特权");
    }
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
