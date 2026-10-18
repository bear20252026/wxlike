//! WP-02 friend relations: request -> accept/decline, address book, remarks,
//! incremental sync cursor, and the system envelopes pushed through the hub.
//!
//! Model: `friends` stores **two rows per friendship** (a->b and b->a) written
//! in one transaction, so each side can hold its own remark. `contact_log` is
//! the append-only source for `/api/friends/changes?since=<seq>`.
//!
//! Authorization rule: a request is always attributed to the authenticated
//! caller — the body cannot name another `from_uid` (WP-02 A3).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::Serialize;

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::validate;
use crate::{direct_conv_id, system_envelope, Platform, PlatformState};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/friends/requests", post(create_request))
        .route("/api/friends/requests/{req_id}/accept", post(accept))
        .route("/api/friends/requests/{req_id}/decline", post(decline))
        .route("/api/friends/requests/incoming", get(incoming))
        .route("/api/friends/requests/outgoing", get(outgoing))
        .route("/api/friends", get(list_friends))
        .route("/api/friends/changes", get(changes))
        .route("/api/friends/{user_id}/remark", put(set_remark))
        .route("/api/friends/{user_id}", delete(remove_friend))
}

#[derive(serde::Deserialize)]
pub struct RequestBody {
    pub to_user_id: String,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Serialize)]
pub struct RequestRow {
    pub id: String,
    pub from_uid: String,
    pub to_uid: String,
    pub message: String,
    pub status: String,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct FriendRow {
    pub friend_id: String,
    pub remark: String,
    pub source: String,
    pub created_at: i64,
    /// P3 关系元数据（成员私有）：星标 / 标签(数组) / 分组名。
    pub starred: bool,
    pub tags: serde_json::Value,
    pub friend_group: String,
}

#[derive(Serialize)]
pub struct ChangeRow {
    pub seq: i64,
    pub subject_id: String,
    pub kind: String,
    pub changed_at: i64,
}

#[derive(serde::Deserialize)]
pub struct RemarkBody {
    pub remark: String,
}

#[derive(serde::Deserialize)]
pub struct ChangesQuery {
    #[serde(default)]
    since: Option<i64>,
}

fn display_name(p: &Platform, user_id: &str) -> String {
    p.profile_of(user_id).map(|x| x.name).unwrap_or_default()
}

/// Appends one address-book change for `user` about `subject`.
fn log(c: &rusqlite::Connection, user: &str, subject: &str, kind: &str, now: i64) -> Result<()> {
    c.execute(
        "INSERT INTO contact_log(user_id, subject_id, kind, changed_at) VALUES(?1, ?2, ?3, ?4)",
        params![user, subject, kind, now],
    )?;
    Ok(())
}

fn log_tx(
    tx: &rusqlite::Transaction<'_>,
    user: &str,
    subject: &str,
    kind: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO contact_log(user_id, subject_id, kind, changed_at) VALUES(?1, ?2, ?3, ?4)",
        params![user, subject, kind, now],
    )?;
    Ok(())
}

impl Platform {
    /// Creates (or reuses) the pending request from `from_uid` to `to_uid`.
    /// Returns `(request_id, recipient_id)`.
    pub fn send_friend_request(
        &self,
        from_uid: &str,
        to_uid: &str,
        message: &str,
    ) -> Result<(String, String)> {
        // P3 ADR-24: 拉黑是好友申请的前置闸门
        if self.is_blocked(from_uid, to_uid)? {
            return Err(PlatformError::Forbidden);
        }
        let from_uid = validate::id(from_uid)?.to_string();
        let to_uid = validate::id(to_uid)?.to_string();
        let message = validate::note(message)?.to_string();
        if from_uid == to_uid {
            return Err(PlatformError::BadInput("cannot add yourself"));
        }
        let id = crate::auth::random_hex(8);
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        let target: Option<String> = c
            .query_row("SELECT id FROM users WHERE id = ?1", params![to_uid], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        if target.is_none() {
            return Err(PlatformError::NotFound);
        }
        let existing: Option<(String, String)> = c
            .query_row(
                "SELECT id, status FROM friend_requests WHERE from_uid = ?1 AND to_uid = ?2",
                params![from_uid, to_uid],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((existing_id, status)) = existing {
            // Re-adding someone you already added: idempotent for the caller but
            // never a second inbox entry.
            if status == "pending" || status == "accepted" {
                return Err(PlatformError::Conflict("request".into()));
            }
            c.execute(
                "UPDATE friend_requests SET status = 'pending', message = ?2, created_at = ?3 WHERE id = ?1",
                params![existing_id, message, now],
            )?;
            log(&c, &to_uid, &from_uid, "request", now)?;
            log(&c, &from_uid, &to_uid, "request", now)?;
            return Ok((existing_id, to_uid));
        }
        c.execute(
            "INSERT INTO friend_requests(id, from_uid, to_uid, message, status, created_at)
             VALUES(?1, ?2, ?3, ?4, 'pending', ?5)",
            params![id, from_uid, to_uid, message, now],
        )?;
        // Both sides log it: the sender's other devices need to learn that a
        // request exists, not just the recipient.
        log(&c, &to_uid, &from_uid, "request", now)?;
        log(&c, &from_uid, &to_uid, "request", now)?;
        Ok((id, to_uid))
    }

    fn load_request(&self, req_id: &str) -> Result<RequestRow> {
        let c = self.db().lock();
        c.query_row(
            "SELECT id, from_uid, to_uid, message, status, created_at FROM friend_requests WHERE id = ?1",
            params![req_id],
            |r| {
                Ok(RequestRow {
                    id: r.get(0)?,
                    from_uid: r.get(1)?,
                    to_uid: r.get(2)?,
                    message: r.get(3)?,
                    status: r.get(4)?,
                    created_at: r.get(5)?,
                })
            },
        )
        .optional()?
        .ok_or(PlatformError::NotFound)
    }

    /// Only the recipient of a pending request may accept it, and the
    /// friendship rows + both contact-log entries commit atomically.
    pub fn accept_request(&self, me: &str, req_id: &str) -> Result<RequestRow> {
        let req = self.load_request(req_id)?;
        if req.to_uid != me {
            return Err(PlatformError::Forbidden);
        }
        if req.status != "pending" {
            return Err(PlatformError::Conflict("request".into()));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        let changed = tx.execute(
            "UPDATE friend_requests SET status = 'accepted' WHERE id = ?1 AND status = 'pending'",
            params![req_id],
        )?;
        if changed == 0 {
            // lost a race with a concurrent accept/decline
            return Err(PlatformError::Conflict("request".into()));
        }
        for (a, b) in [
            (req.from_uid.as_str(), req.to_uid.as_str()),
            (req.to_uid.as_str(), req.from_uid.as_str()),
        ] {
            tx.execute(
                "INSERT INTO friends(user_id, friend_id, remark, source, created_at)
                 VALUES(?1, ?2, '', ?3, ?4)
                 ON CONFLICT(user_id, friend_id) DO NOTHING",
                params![a, b, req_id, now],
            )?;
            log_tx(&tx, a, b, "accepted", now)?;
        }
        tx.commit()?;
        Ok(req)
    }

    pub fn decline_request(&self, me: &str, req_id: &str) -> Result<RequestRow> {
        let req = self.load_request(req_id)?;
        if req.to_uid != me {
            return Err(PlatformError::Forbidden);
        }
        if req.status != "pending" {
            return Err(PlatformError::Conflict("request".into()));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        let tx = c.transaction()?;
        tx.execute(
            "UPDATE friend_requests SET status = 'declined' WHERE id = ?1",
            params![req_id],
        )?;
        log_tx(&tx, &req.from_uid, &req.to_uid, "declined", now)?;
        log_tx(&tx, &req.to_uid, &req.from_uid, "declined", now)?;
        tx.commit()?;
        Ok(req)
    }

    pub fn incoming_requests(&self, me: &str) -> Result<Vec<RequestRow>> {
        // The inbox is the pending queue: answered requests (accepted/declined)
        // leave it, which is what makes "decline empties the inbox" observable.
        let sql = "SELECT id, from_uid, to_uid, message, status, created_at FROM friend_requests WHERE to_uid = ?1 AND status = 'pending' ORDER BY created_at DESC";
        let c = self.db().lock();
        let mut stmt = c.prepare(sql)?;
        let rows = stmt.query_map(params![me], |r| {
            Ok(RequestRow {
                id: r.get(0)?,
                from_uid: r.get(1)?,
                to_uid: r.get(2)?,
                message: r.get(3)?,
                status: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn outgoing_requests(&self, me: &str) -> Result<Vec<RequestRow>> {
        // Outgoing keeps every status so the sender can see who declined.
        self.list_requests("from_uid", me)
    }

    fn list_requests(&self, col: &str, me: &str) -> Result<Vec<RequestRow>> {
        // `col` is a closed set chosen by the caller, never user input.
        let sql = match col {
            "to_uid" => "SELECT id, from_uid, to_uid, message, status, created_at FROM friend_requests WHERE to_uid = ?1 ORDER BY created_at DESC",
            _ => "SELECT id, from_uid, to_uid, message, status, created_at FROM friend_requests WHERE from_uid = ?1 ORDER BY created_at DESC",
        };
        let c = self.db().lock();
        let mut stmt = c.prepare(sql)?;
        let rows = stmt.query_map(params![me], |r| {
            Ok(RequestRow {
                id: r.get(0)?,
                from_uid: r.get(1)?,
                to_uid: r.get(2)?,
                message: r.get(3)?,
                status: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn friends(&self, me: &str) -> Result<Vec<FriendRow>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT friend_id, remark, source, created_at, starred, tags, friend_group
             FROM friends WHERE user_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![me], |r| {
            Ok(FriendRow {
                friend_id: r.get(0)?,
                remark: r.get(1)?,
                source: r.get(2)?,
                created_at: r.get(3)?,
                starred: r.get::<_, i64>(4)? != 0,
                tags: serde_json::from_str(&r.get::<_, String>(5)?)
                    .unwrap_or(serde_json::json!([])),
                friend_group: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn are_friends(&self, a: &str, b: &str) -> Result<bool> {
        let c = self.db().lock();
        let hit: Option<i64> = c
            .query_row(
                "SELECT 1 FROM friends WHERE user_id = ?1 AND friend_id = ?2",
                params![a, b],
                |r| r.get::<_, i64>(0),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    pub fn set_remark(&self, me: &str, friend: &str, remark: &str) -> Result<()> {
        let friend = validate::id(friend)?.to_string();
        let remark = validate::remark(remark)?.to_string();
        let c = self.db().lock();
        let changed = c.execute(
            "UPDATE friends SET remark = ?1 WHERE user_id = ?2 AND friend_id = ?3",
            params![remark, me, friend],
        )?;
        if changed == 0 {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    /// Mutual removal: both rows and both log entries in one transaction.
    pub fn remove_friend(&self, me: &str, other: &str) -> Result<()> {
        let other = validate::id(other)?.to_string();
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        if !self.are_friends_inner(&mut c, me, &other)? {
            return Err(PlatformError::NotFound);
        }
        let tx = c.transaction()?;
        tx.execute(
            "DELETE FROM friends WHERE user_id = ?1 AND friend_id = ?2",
            params![me, other],
        )?;
        tx.execute(
            "DELETE FROM friends WHERE user_id = ?1 AND friend_id = ?2",
            params![other, me],
        )?;
        for (a, b) in [(me, other.as_str()), (other.as_str(), me)] {
            log_tx(&tx, a, b, "removed", now)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn are_friends_inner(&self, c: &mut rusqlite::Connection, a: &str, b: &str) -> Result<bool> {
        Ok(c.query_row(
            "SELECT 1 FROM friends WHERE user_id = ?1 AND friend_id = ?2",
            params![a, b],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
    }

    /// Incremental address-book sync: everything for `me` after `since`.
    pub fn changes(&self, me: &str, since: i64, limit: i64) -> Result<Vec<ChangeRow>> {
        let c = self.db().lock();
        let mut stmt = c.prepare(
            "SELECT seq, subject_id, kind, changed_at FROM contact_log
             WHERE user_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![me, since, limit], |r| {
            Ok(ChangeRow {
                seq: r.get(0)?,
                subject_id: r.get(1)?,
                kind: r.get(2)?,
                changed_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Highest log sequence, for the client to store as its next cursor.
    pub fn changes_head(&self, me: &str) -> Result<i64> {
        let c = self.db().lock();
        Ok(c.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM contact_log WHERE user_id = ?1",
            params![me],
            |r| r.get::<_, i64>(0),
        )?)
    }
}

async fn create_request(
    State(platform): State<PlatformState>,
    authed: Authed,
    Json(body): Json<RequestBody>,
) -> std::result::Result<(StatusCode, Json<RequestRow>), ApiError> {
    let message = body.message.clone().unwrap_or_default();
    let (req_id, to_uid) =
        platform.send_friend_request(&authed.user_id, &body.to_user_id, &message)?;
    let from_name = display_name(&platform, &authed.user_id);
    let note = if message.is_empty() {
        format!("{from_name} 请求添加你为好友")
    } else {
        format!("{from_name} 请求添加你为好友：{message}")
    };
    let env = system_envelope(
        &direct_conv_id(&authed.user_id, &to_uid),
        &authed.user_id,
        vec![to_uid.clone()],
        &note,
    );
    platform.emit(env).await;
    let row = platform.load_request(&req_id)?;
    Ok((StatusCode::CREATED, Json(row)))
}

async fn accept(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(req_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let req = platform.accept_request(&authed.user_id, &req_id)?;
    let my_name = display_name(&platform, &authed.user_id);
    let env = system_envelope(
        &direct_conv_id(&req.from_uid, &req.to_uid),
        &authed.user_id,
        vec![req.from_uid.clone()],
        &format!("{my_name} 已同意你的好友请求"),
    );
    platform.emit(env).await;
    Ok(Json(serde_json::json!({
        "friend_id": req.from_uid,
        "status": "accepted",
    })))
}

async fn decline(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(req_id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let req = platform.decline_request(&authed.user_id, &req_id)?;
    let my_name = display_name(&platform, &authed.user_id);
    let env = system_envelope(
        &direct_conv_id(&req.from_uid, &req.to_uid),
        &authed.user_id,
        vec![req.from_uid.clone()],
        &format!("{my_name} 拒绝了你的好友请求"),
    );
    platform.emit(env).await;
    Ok(Json(serde_json::json!({ "status": "declined" })))
}

async fn incoming(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Vec<RequestRow>>, ApiError> {
    Ok(Json(platform.incoming_requests(&authed.user_id)?))
}

async fn outgoing(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Vec<RequestRow>>, ApiError> {
    Ok(Json(platform.outgoing_requests(&authed.user_id)?))
}

async fn list_friends(
    State(platform): State<PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Vec<FriendRow>>, ApiError> {
    Ok(Json(platform.friends(&authed.user_id)?))
}

async fn set_remark(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(user_id): Path<String>,
    Json(body): Json<RemarkBody>,
) -> std::result::Result<StatusCode, ApiError> {
    platform.set_remark(&authed.user_id, &user_id, &body.remark)?;
    Ok(StatusCode::OK)
}

async fn remove_friend(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(user_id): Path<String>,
) -> std::result::Result<StatusCode, ApiError> {
    let other = validate::id(&user_id).map_err(ApiError::from)?;
    let was_friend = platform.are_friends(&authed.user_id, other)?;
    platform.remove_friend(&authed.user_id, other)?;
    if was_friend {
        let my_name = display_name(&platform, &authed.user_id);
        let env = system_envelope(
            &direct_conv_id(&authed.user_id, other),
            &authed.user_id,
            vec![other.to_string()],
            &format!("{my_name} 已将你从通讯录移除"),
        );
        platform.emit(env).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn changes(
    State(platform): State<PlatformState>,
    authed: Authed,
    Query(q): Query<ChangesQuery>,
) -> std::result::Result<Json<serde_json::Value>, ApiError> {
    let since = q.since.unwrap_or(0).max(0);
    let rows = platform.changes(&authed.user_id, since, 200)?;
    let head = platform.changes_head(&authed.user_id)?;
    Ok(Json(serde_json::json!({
        "changes": rows,
        "cursor": head,
        "has_more": head > rows.last().map(|r| r.seq).unwrap_or(since),
    })))
}
