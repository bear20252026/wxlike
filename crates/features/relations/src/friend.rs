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

    fn parse(raw: &str) -> Self {
        match raw {
            "accepted" => Self::Accepted,
            "rejected" => Self::Rejected,
            "cancelled" => Self::Cancelled,
            _ => Self::Pending,
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

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<Request> {
    Ok(Request {
        id: r.get(0)?,
        from_uid: r.get(1)?,
        to_uid: r.get(2)?,
        message: r.get(3)?,
        status: Status::parse(&r.get::<_, String>(4)?),
        created_at: r.get(5)?,
    })
}

fn load(conn: &Connection, id: &str) -> Result<Option<Request>> {
    let sql = format!("SELECT {COLS} FROM friend_requests WHERE id = ?1");
    Ok(conn.query_row(&sql, params![id], row_of).optional()?)
}

fn find_between(conn: &Connection, from: &str, to: &str) -> Result<Option<Request>> {
    let sql = format!("SELECT {COLS} FROM friend_requests WHERE from_uid = ?1 AND to_uid = ?2");
    Ok(conn.query_row(&sql, params![from, to], row_of).optional()?)
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

fn create(conn: &Connection, from: &str, to: &str, msg: &str, now: i64) -> Result<SendOutcome> {
    let id = random_hex(REQUEST_ID_BYTES);
    conn.execute(
        "INSERT INTO friend_requests(id, from_uid, to_uid, message, status, created_at) \
         VALUES(?1, ?2, ?3, ?4, 'pending', ?5)",
        params![id, from, to, msg, now],
    )?;
    audit(conn, from, to, "friend.request", now)?;
    let req = load(conn, &id)?.ok_or(RelError::NotFound("request"))?;
    Ok(SendOutcome::Created(req))
}

/// 被拒/取消之后重新申请：复用同一行（`UNIQUE(from,to)` 不允许第二行）。
fn reopen(conn: &Connection, existing: &Request, msg: &str, now: i64) -> Result<SendOutcome> {
    conn.execute(
        "UPDATE friend_requests SET status = 'pending', message = ?2, created_at = ?3 \
         WHERE id = ?1 AND status IN ('rejected','cancelled')",
        params![existing.id, msg, now],
    )?;
    audit(
        conn,
        &existing.from_uid,
        &existing.to_uid,
        "friend.request",
        now,
    )?;
    let req = load(conn, &existing.id)?.ok_or(RelError::NotFound("request"))?;
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
pub fn cancel(conn: &Connection, me: &str, req_id: &str, now: i64) -> Result<Request> {
    let tx = conn.unchecked_transaction()?;
    let req = load(&tx, req_id)?.ok_or(RelError::NotFound("request"))?;
    if req.from_uid != me {
        return Err(RelError::Forbidden("not the sender"));
    }
    if req.status != Status::Pending {
        return Err(RelError::Conflict("request already decided"));
    }
    tx.execute(
        "UPDATE friend_requests SET status = 'cancelled' WHERE id = ?1 AND status = 'pending'",
        params![req.id],
    )?;
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
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
