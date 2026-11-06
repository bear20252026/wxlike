//! 好友与新朋友：双向申请状态机。
//!
//! ```text
//! (无) --send--> pending --accept--> accepted --> 互为好友（两行）
//!                  |  \--reject--> rejected
//!                  \-----cancel--> cancelled
//! pending(A→B) + send(B→A)  ==> 直接 accepted（双方都表达过意愿）
//! ```
//!
//! 判定顺序是刻意的：**拉黑 → 已是好友 → 反向申请 → 同向去重 → 新建**。
//! 把拉黑放在最前面，是为了让被拉黑的一方拿不到"对方已经是你好友/已经申请过"
//! 这类线索。

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::{audit, check_text, random_hex, RelError, Result, MAX_MESSAGE_CHARS, REQUEST_ID_BYTES};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Accepted,
    Rejected,
    Cancelled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
        }
    }

    /// 原文 → 状态。**没有 `_ => Pending` 这种兜底**。
    ///
    /// 待处理列表本身是安全的（`incoming`/`outgoing` 的 SQL 写死了
    /// `status = 'pending'`，坏值选不出来）。会撒谎的是**按 id 取整行**的
    /// `load`/`find_between`：折成 `pending` 之后，`send()` 直接把一条读不懂的
    /// 行当作"你已经申请过了"返回 `Duplicate`，而这行既不是 pending、也不会
    /// 再被任何流程推进——用户卡死在一个永远发不出去的申请上。
    /// 坏行必须让调用方看见"这行坏了"，而不是看见一条假的好消息。
    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(RelError::Corrupt("friend_requests.status")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: String,
    pub from_uid: String,
    pub to_uid: String,
    pub message: String,
    pub status: Status,
    pub created_at: i64,
}

/// 发申请的四种结局。全部是 `Ok`：**"已经是好友""已申请过"不是错误**，
/// 否则调用方会把它当失败重试，反而放大重复提交。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Created(Request),
    Duplicate(Request),
    AutoAccepted(Request),
    AlreadyFriends,
}

const COLS: &str = "id, from_uid, to_uid, message, status, created_at";

/// `SQLITE_CONSTRAINT_UNIQUE`（2067）。只有这一种约束冲突意味着"已经有一行了"，
/// 其它约束问题（非空、检查、外键）仍然要照原样报错，不许一起吞掉。
const CONSTRAINT_UNIQUE: i32 = rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE;

/// 一行的**原文**读法：`status` 先按字符串取，判定留到 [`decode`]。
///
/// 分两步是因为行闭包里只能返回 `rusqlite::Result`——在那里折默认值太顺手了，
/// 坏数据就此在类型层面假装成功（与 identity 的 `RawJob`/`decode_job` 同一套）。
struct RawRequest {
    id: String,
    from_uid: String,
    to_uid: String,
    message: String,
    status: String,
    created_at: i64,
}

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRequest> {
    Ok(RawRequest {
        id: r.get(0)?,
        from_uid: r.get(1)?,
        to_uid: r.get(2)?,
        message: r.get(3)?,
        status: r.get(4)?,
        created_at: r.get(5)?,
    })
}

/// 原文 → 领域对象。任何一种"看不懂"都是一条 [`RelError::Corrupt`]。
fn decode(raw: RawRequest) -> Result<Request> {
    Ok(Request {
        id: raw.id,
        from_uid: raw.from_uid,
        to_uid: raw.to_uid,
        message: raw.message,
        status: Status::parse(&raw.status)?,
        created_at: raw.created_at,
    })
}

fn load(conn: &Connection, id: &str) -> Result<Option<Request>> {
    let sql = format!("SELECT {COLS} FROM friend_requests WHERE id = ?1");
    conn.query_row(&sql, params![id], row_of)
        .optional()?
        .map(decode)
        .transpose()
}

fn find_between(conn: &Connection, from: &str, to: &str) -> Result<Option<Request>> {
    let sql = format!("SELECT {COLS} FROM friend_requests WHERE from_uid = ?1 AND to_uid = ?2");
    conn.query_row(&sql, params![from, to], row_of)
        .optional()?
        .map(decode)
        .transpose()
}

/// 是否有一方拉黑了另一方（任一方向都算）。
pub fn blocked_either(conn: &Connection, a: &str, b: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM blocks WHERE (user_id = ?1 AND blocked_id = ?2) \
         OR (user_id = ?2 AND blocked_id = ?1)",
        params![a, b],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// 单方向拉黑判定（`by` 拉黑了 `target` 吗）。
pub fn is_blocked(conn: &Connection, by: &str, target: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM blocks WHERE user_id = ?1 AND blocked_id = ?2",
        params![by, target],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// 是否互为好友（两行都在才算）。
pub fn are_friends(conn: &Connection, a: &str, b: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM friends WHERE (user_id = ?1 AND friend_id = ?2) \
         OR (user_id = ?2 AND friend_id = ?1)",
        params![a, b],
        |r| r.get(0),
    )?;
    Ok(n == 2)
}

/// 发好友申请。
pub fn send(
    conn: &Connection,
    from: &str,
    to: &str,
    message: &str,
    now: i64,
) -> Result<SendOutcome> {
    if from == to {
        return Err(RelError::BadInput("self request"));
    }
    let msg = check_text(message, MAX_MESSAGE_CHARS, "message")?;
    if blocked_either(conn, from, to)? {
        return Err(RelError::Forbidden("blocked"));
    }
    if are_friends(conn, from, to)? {
        return Ok(SendOutcome::AlreadyFriends);
    }
    if let Some(reverse) = find_between(conn, to, from)? {
        if reverse.status == Status::Pending {
            // 反向申请是 `to → from`，所以收件人是 `from`（本次发送者）。
            let accepted = accept(conn, from, &reverse.id, now)?;
            return Ok(SendOutcome::AutoAccepted(accepted));
        }
    }
    match find_between(conn, from, to)? {
        Some(existing) if existing.status == Status::Pending => {
            Ok(SendOutcome::Duplicate(existing))
        }
        Some(existing) => reopen(conn, &existing, &msg, now),
        None => create(conn, from, to, &msg, now),
    }
}

/// 落一条新申请。
///
/// `UNIQUE(from_uid, to_uid)` 撞车**不是失败**：那是同一对账号的两个请求同时
/// 走到这里（前端重试、双端同点、弱网重发），库里已经有一行了。先查再插在两个
/// 连接之间必然有窗口，所以这里把约束冲突解释成"已存在"，重读那一行按状态交出
/// `Duplicate`。原样抛 `Db(ConstraintViolation)` 会把一次无害的重复提交变成 500。
fn create(conn: &Connection, from: &str, to: &str, msg: &str, now: i64) -> Result<SendOutcome> {
    let id = random_hex(REQUEST_ID_BYTES);
    match conn.execute(
        "INSERT INTO friend_requests(id, from_uid, to_uid, message, status, created_at) \
         VALUES(?1, ?2, ?3, ?4, 'pending', ?5)",
        params![id, from, to, msg, now],
    ) {
        Ok(_) => {
            audit(conn, from, to, "friend.request", now)?;
            let req = load(conn, &id)?.ok_or(RelError::NotFound("request"))?;
            Ok(SendOutcome::Created(req))
        }
        Err(rusqlite::Error::SqliteFailure(code, _)) if code.extended_code == CONSTRAINT_UNIQUE => {
            let existing = find_between(conn, from, to)?.ok_or(RelError::NotFound("request"))?;
            if existing.status == Status::Pending {
                return Ok(SendOutcome::Duplicate(existing));
            }
            // 输掉插入的一方紧接着对方已经把它接受/拒掉了：按真实终态回答。
            Err(RelError::Conflict("request decided concurrently"))
        }
        Err(e) => Err(e.into()),
    }
}

/// 被拒/取消之后重新申请：复用同一行（`UNIQUE(from,to)` 不允许第二行）。
///
/// 条件 UPDATE 命中 0 行**必须报错**。旧写法忽略受影响行数，于是一条
/// `status='accepted'` 但好友两行残缺的行会走成"重新申请成功"——函数返回
/// `Created`，行里却仍是 `accepted`，收件人的待处理列表里永远不会出现这条申请，
/// 用户看到"已发送"而服务端什么都没写。这是最该拒绝的那类假装成功。
fn reopen(conn: &Connection, existing: &Request, msg: &str, now: i64) -> Result<SendOutcome> {
    let n = conn.execute(
        "UPDATE friend_requests SET status = 'pending', message = ?2, created_at = ?3 \
         WHERE id = ?1 AND status IN ('rejected','cancelled')",
        params![existing.id, msg, now],
    )?;
    if n != 1 {
        return Err(RelError::Conflict("request is not re-openable"));
    }
    audit(
        conn,
        &existing.from_uid,
        &existing.to_uid,
        "friend.request",
        now,
    )?;
    let req = load(conn, &existing.id)?.ok_or(RelError::NotFound("request"))?;
    if req.status != Status::Pending {
        return Err(RelError::Corrupt("friend_requests.status"));
    }
    Ok(SendOutcome::Created(req))
}

/// 接受申请：条件 UPDATE 抢占 + 两行关系（`INSERT OR IGNORE`）。
pub fn accept(conn: &Connection, me: &str, req_id: &str, now: i64) -> Result<Request> {
    let tx = conn.unchecked_transaction()?;
    let req = load(&tx, req_id)?.ok_or(RelError::NotFound("request"))?;
    if req.to_uid != me {
        return Err(RelError::Forbidden("not the addressee"));
    }
    if req.status != Status::Pending {
        return Err(RelError::Conflict("request already decided"));
    }
    if blocked_either(&tx, &req.from_uid, &req.to_uid)? {
        return Err(RelError::Forbidden("blocked"));
    }
    let n = tx.execute(
        "UPDATE friend_requests SET status = 'accepted' WHERE id = ?1 AND status = 'pending'",
        params![req.id],
    )?;
    if n != 1 {
        return Err(RelError::Conflict("request already decided"));
    }
    link(&tx, &req.from_uid, &req.to_uid, now)?;
    audit(&tx, &req.from_uid, &req.to_uid, "friend.accept", now)?;
    tx.commit()?;
    load(conn, req_id)?.ok_or(RelError::NotFound("request"))
}

/// 双向落两行；已存在就跳过（并发接受不会产生重复关系）。
fn link(conn: &Connection, a: &str, b: &str, now: i64) -> Result<()> {
    for (owner, peer) in [(a, b), (b, a)] {
        conn.execute(
            "INSERT OR IGNORE INTO friends(user_id, friend_id, remark, source, starred, created_at) \
             VALUES(?1, ?2, '', 'request', 0, ?3)",
            params![owner, peer, now],
        )?;
    }
    Ok(())
}

/// 拒绝申请（只有收件人能拒）。
pub fn reject(conn: &Connection, me: &str, req_id: &str, now: i64) -> Result<Request> {
    decide(conn, me, req_id, Status::Rejected, "friend.reject", now)
}

/// 撤回自己发出的申请。
///
/// 与 `accept`/`decide` 一样必须核对条件 UPDATE 的命中行数：撤回和接受是**真的
/// 会并发**的两个动作（发申请的人点"撤回"的同时，对方正在点"接受"）。忽略行数的
/// 话，`cancel` 会在对方已经接受之后返回"撤回成功"，前端就此显示"已撤回"，
/// 而库里两人其实已是好友。
pub fn cancel(conn: &Connection, me: &str, req_id: &str, now: i64) -> Result<Request> {
    let tx = conn.unchecked_transaction()?;
    let req = load(&tx, req_id)?.ok_or(RelError::NotFound("request"))?;
    if req.from_uid != me {
        return Err(RelError::Forbidden("not the sender"));
    }
    if req.status != Status::Pending {
        return Err(RelError::Conflict("request already decided"));
    }
    let n = tx.execute(
        "UPDATE friend_requests SET status = 'cancelled' WHERE id = ?1 AND status = 'pending'",
        params![req.id],
    )?;
    if n != 1 {
        return Err(RelError::Conflict("request already decided"));
    }
    audit(&tx, me, &req.to_uid, "friend.cancel", now)?;
    tx.commit()?;
    load(conn, req_id)?.ok_or(RelError::NotFound("request"))
}

fn decide(
    conn: &Connection,
    me: &str,
    req_id: &str,
    target: Status,
    kind: &str,
    now: i64,
) -> Result<Request> {
    let tx = conn.unchecked_transaction()?;
    let req = load(&tx, req_id)?.ok_or(RelError::NotFound("request"))?;
    if req.to_uid != me {
        return Err(RelError::Forbidden("not the addressee"));
    }
    let n = tx.execute(
        "UPDATE friend_requests SET status = ?2 WHERE id = ?1 AND status = 'pending'",
        params![req.id, target.as_str()],
    )?;
    if n != 1 {
        return Err(RelError::Conflict("request already decided"));
    }
    audit(&tx, me, &req.from_uid, kind, now)?;
    tx.commit()?;
    load(conn, req_id)?.ok_or(RelError::NotFound("request"))
}

/// 拉黑：落黑名单 + 拆掉双向好友 + 清掉双向待处理申请。
pub fn block(conn: &Connection, me: &str, target: &str, now: i64) -> Result<()> {
    if me == target {
        return Err(RelError::BadInput("self block"));
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT OR IGNORE INTO blocks(user_id, blocked_id, created_at) VALUES(?1, ?2, ?3)",
        params![me, target, now],
    )?;
    tx.execute(
        "DELETE FROM friends WHERE (user_id = ?1 AND friend_id = ?2) \
         OR (user_id = ?2 AND friend_id = ?1)",
        params![me, target],
    )?;
    tx.execute(
        "DELETE FROM friend_requests WHERE (from_uid = ?1 AND to_uid = ?2) \
         OR (from_uid = ?2 AND to_uid = ?1)",
        params![me, target],
    )?;
    audit(&tx, me, target, "block", now)?;
    tx.commit()?;
    Ok(())
}

/// 解除拉黑。**不恢复**好友关系（用户要重新申请）。
pub fn unblock(conn: &Connection, me: &str, target: &str, now: i64) -> Result<()> {
    let n = conn.execute(
        "DELETE FROM blocks WHERE user_id = ?1 AND blocked_id = ?2",
        params![me, target],
    )?;
    if n == 0 {
        return Err(RelError::NotFound("block"));
    }
    audit(conn, me, target, "unblock", now)?;
    Ok(())
}

/// 删除好友（单向发起，双向生效）。
pub fn remove(conn: &Connection, me: &str, other: &str, now: i64) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "DELETE FROM friends WHERE (user_id = ?1 AND friend_id = ?2) \
         OR (user_id = ?2 AND friend_id = ?1)",
        params![me, other],
    )?;
    if n == 0 {
        return Err(RelError::NotFound("friendship"));
    }
    audit(&tx, me, other, "friend.remove", now)?;
    tx.commit()?;
    Ok(())
}

fn list(conn: &Connection, column: &str, me: &str, limit: i64) -> Result<Vec<Request>> {
    let sql = format!(
        "SELECT {COLS} FROM friend_requests WHERE {column} = ?1 AND status = 'pending' \
         ORDER BY created_at DESC LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![me, limit], row_of)?;
    let raw = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    raw.into_iter().map(decode).collect()
}

/// 待我处理的新朋友，最近在前。
pub fn incoming(conn: &Connection, me: &str, limit: i64) -> Result<Vec<Request>> {
    list(conn, "to_uid", me, limit)
}

/// 我发出且仍在等对方处理的申请。
pub fn outgoing(conn: &Connection, me: &str, limit: i64) -> Result<Vec<Request>> {
    list(conn, "from_uid", me, limit)
}

/// 好友 id 列表（备注/标签在 [`crate::prefs`]）。
pub fn friend_ids(conn: &Connection, me: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT friend_id FROM friends WHERE user_id = ?1 ORDER BY created_at ASC")?;
    let rows = stmt.query_map(params![me], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    #[test]
    fn duplicate_send_returns_the_same_row() {
        let conn = db();
        let a = send(&conn, "u1", "u2", "hi", 1_000).unwrap();
        let b = send(&conn, "u1", "u2", "hi again", 2_000).unwrap();
        let (SendOutcome::Created(first), SendOutcome::Duplicate(second)) = (a, b) else {
            panic!("第二次必须是 Duplicate");
        };
        assert_eq!(first.id, second.id);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM friend_requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "去重：同向重复申请不产生第二行");
    }

    #[test]
    fn reverse_request_auto_accepts() {
        let conn = db();
        send(&conn, "u1", "u2", "", 1_000).unwrap();
        let out = send(&conn, "u2", "u1", "", 2_000).unwrap();
        assert!(matches!(out, SendOutcome::AutoAccepted(_)));
        assert!(are_friends(&conn, "u1", "u2").unwrap());
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM friends", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 2, "好友关系是两行，各持备注/星标");
    }

    #[test]
    fn concurrent_accept_creates_one_pair() {
        let conn = db();
        let SendOutcome::Created(req) = send(&conn, "u1", "u2", "", 1_000).unwrap() else {
            panic!("created");
        };
        accept(&conn, "u2", &req.id, 1_100).unwrap();
        assert!(matches!(
            accept(&conn, "u2", &req.id, 1_200),
            Err(RelError::Conflict(_))
        ));
        assert!(are_friends(&conn, "u1", "u2").unwrap());
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM friends", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 2);
    }

    #[test]
    fn only_addressee_can_accept() {
        let conn = db();
        let SendOutcome::Created(req) = send(&conn, "u1", "u2", "", 1_000).unwrap() else {
            panic!("created");
        };
        assert!(matches!(
            accept(&conn, "u3", &req.id, 1_100),
            Err(RelError::Forbidden(_))
        ));
        assert!(matches!(
            reject(&conn, "u1", &req.id, 1_100),
            Err(RelError::Forbidden(_))
        ));
    }

    #[test]
    fn block_wins_over_everything() {
        let conn = db();
        let SendOutcome::Created(req) = send(&conn, "u1", "u2", "", 1_000).unwrap() else {
            panic!("created");
        };
        block(&conn, "u2", "u1", 1_100).unwrap();
        // 申请被清掉，再发直接被拒。
        assert!(load(&conn, &req.id).unwrap().is_none());
        assert!(matches!(
            send(&conn, "u1", "u2", "", 1_200),
            Err(RelError::Forbidden("blocked"))
        ));
        assert!(
            matches!(
                send(&conn, "u2", "u1", "", 1_200),
                Err(RelError::Forbidden("blocked"))
            ),
            "拉黑是双向闸门：拉黑方也不能再发起"
        );
    }

    #[test]
    fn block_removes_friendship_and_unblock_does_not_restore() {
        let conn = db();
        send(&conn, "u1", "u2", "", 1_000).unwrap();
        send(&conn, "u2", "u1", "", 1_100).unwrap();
        assert!(are_friends(&conn, "u1", "u2").unwrap());
        block(&conn, "u1", "u2", 1_200).unwrap();
        assert!(!are_friends(&conn, "u1", "u2").unwrap());
        unblock(&conn, "u1", "u2", 1_300).unwrap();
        assert!(!are_friends(&conn, "u1", "u2").unwrap());
        assert!(!is_blocked(&conn, "u1", "u2").unwrap());
    }

    #[test]
    fn already_friends_is_not_an_error() {
        let conn = db();
        send(&conn, "u1", "u2", "", 1_000).unwrap();
        send(&conn, "u2", "u1", "", 1_100).unwrap();
        assert_eq!(
            send(&conn, "u1", "u2", "", 1_200).unwrap(),
            SendOutcome::AlreadyFriends
        );
    }

    #[test]
    fn reapply_after_reject_reuses_the_row() {
        let conn = db();
        let SendOutcome::Created(first) = send(&conn, "u1", "u2", "one", 1_000).unwrap() else {
            panic!("created");
        };
        reject(&conn, "u2", &first.id, 1_100).unwrap();
        let SendOutcome::Created(again) = send(&conn, "u1", "u2", "two", 1_200).unwrap() else {
            panic!("重新申请");
        };
        assert_eq!(again.id, first.id, "UNIQUE(from,to) 只允许一行，复用即可");
        assert_eq!(again.status, Status::Pending);
        assert_eq!(again.message, "two");
    }

    #[test]
    fn cancel_and_remove_and_lists() {
        let conn = db();
        assert!(incoming(&conn, "u2", 50).unwrap().is_empty());
        let SendOutcome::Created(req) = send(&conn, "u1", "u2", "", 1_000).unwrap() else {
            panic!("created");
        };
        assert_eq!(incoming(&conn, "u2", 50).unwrap().len(), 1);
        assert_eq!(outgoing(&conn, "u1", 50).unwrap().len(), 1);
        cancel(&conn, "u1", &req.id, 1_100).unwrap();
        assert!(incoming(&conn, "u2", 50).unwrap().is_empty());
        assert!(matches!(
            remove(&conn, "u1", "u2", 1_200),
            Err(RelError::NotFound(_))
        ));
    }

    #[test]
    fn rejects_bad_input() {
        let conn = db();
        assert!(matches!(
            send(&conn, "u1", "u1", "", 1_000),
            Err(RelError::BadInput(_))
        ));
        let long = "x".repeat(MAX_MESSAGE_CHARS + 1);
        assert!(matches!(
            send(&conn, "u1", "u2", &long, 1_000),
            Err(RelError::BadInput(_))
        ));
        assert!(matches!(
            block(&conn, "u1", "u1", 1_000),
            Err(RelError::BadInput(_))
        ));
        assert!(matches!(
            unblock(&conn, "u1", "u2", 1_000),
            Err(RelError::NotFound(_))
        ));
    }
}
