//! WP-02 friend relations: request -> accept/decline, address book, remarks,
//! incremental sync cursor, and the system envelopes pushed through the hub.
//!
//! Model: `friends` stores **two rows per friendship** (a->b and b->a) written
//! in one transaction, so each side can hold its own remark. `contact_log` is
//! the append-only source for `/api/friends/changes?since=<seq>`.
//!
//! Authorization rule: a request is always attributed to the authenticated
//! caller — the body cannot name another `from_uid` (WP-02 A3).
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::friends`]。

use crate::error::{PlatformError, Result};
use crate::validate;
use crate::Platform;
use serde::Serialize;
use wxlike_core::tx::ConnectionTx as _;

use super::repo::friends::{
    accept_request, are_friends, changes, changes_head, decline_request, delete_friend,
    ensure_friend, friends, incoming_requests, insert_request, log, outgoing_requests,
    reopen_request, request_by_id, request_by_pair, set_remark, user_exists, RequestRecord,
};

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

pub(crate) fn display_name(p: &Platform, user_id: &str) -> String {
    p.profile_of(user_id).map(|x| x.name).unwrap_or_default()
}

fn request_row(r: RequestRecord) -> RequestRow {
    RequestRow {
        id: r.id,
        from_uid: r.from_uid,
        to_uid: r.to_uid,
        message: r.message,
        status: r.status,
        created_at: r.created_at,
    }
}

impl Platform {
    /// Creates (or reuses) the pending request from `from_uid` to `to_uid`.
    /// Returns `(request_id, recipient_id)`.
    pub(crate) fn send_friend_request(
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
        let c = self.db().acquire();
        if !user_exists(&c, &to_uid)? {
            return Err(PlatformError::NotFound);
        }
        if let Some((existing_id, status)) = request_by_pair(&c, &from_uid, &to_uid)? {
            // Re-adding someone you already added: idempotent for the caller but
            // never a second inbox entry.
            if status == "pending" || status == "accepted" {
                return Err(PlatformError::Conflict("request".into()));
            }
            reopen_request(&c, &existing_id, &message, now)?;
            log(&c, &to_uid, &from_uid, "request", now)?;
            log(&c, &from_uid, &to_uid, "request", now)?;
            return Ok((existing_id, to_uid));
        }
        insert_request(&c, &id, &from_uid, &to_uid, &message, now)?;
        // Both sides log it: the sender's other devices need to learn that a
        // request exists, not just the recipient.
        log(&c, &to_uid, &from_uid, "request", now)?;
        log(&c, &from_uid, &to_uid, "request", now)?;
        Ok((id, to_uid))
    }

    pub(crate) fn load_request(&self, req_id: &str) -> Result<RequestRow> {
        let c = self.db().acquire();
        request_by_id(&c, req_id)?
            .map(request_row)
            .ok_or(PlatformError::NotFound)
    }

    /// Only the recipient of a pending request may accept it, and the
    /// friendship rows + both contact-log entries commit atomically.
    pub(crate) fn accept_request(&self, me: &str, req_id: &str) -> Result<RequestRow> {
        let req = self.load_request(req_id)?;
        if req.to_uid != me {
            return Err(PlatformError::Forbidden);
        }
        if req.status != "pending" {
            return Err(PlatformError::Conflict("request".into()));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<RequestRow> {
            let conn = crate::storage::StorageConnection::sqlite_tx(tx);
            if !accept_request(&conn, req_id)? {
                // lost a race with a concurrent accept/decline
                return Err(PlatformError::Conflict("request".into()));
            }
            for (a, b) in [
                (req.from_uid.as_str(), req.to_uid.as_str()),
                (req.to_uid.as_str(), req.from_uid.as_str()),
            ] {
                ensure_friend(&conn, a, b, req_id, now)?;
                log(&conn, a, b, "accepted", now)?;
            }
            Ok(req)
        })
    }

    pub(crate) fn decline_request(&self, me: &str, req_id: &str) -> Result<RequestRow> {
        let req = self.load_request(req_id)?;
        if req.to_uid != me {
            return Err(PlatformError::Forbidden);
        }
        if req.status != "pending" {
            return Err(PlatformError::Conflict("request".into()));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<RequestRow> {
            let conn = crate::storage::StorageConnection::sqlite_tx(tx);
            // #340: the verdict rides on the write itself, like accept above —
            // the pre-check reads outside the lock, so two concurrent declines
            // can both pass it; only the conditional UPDATE picks one winner.
            if !decline_request(&conn, req_id)? {
                // lost a race with a concurrent accept/decline
                return Err(PlatformError::Conflict("request".into()));
            }
            log(&conn, &req.from_uid, &req.to_uid, "declined", now)?;
            log(&conn, &req.to_uid, &req.from_uid, "declined", now)?;
            Ok(req)
        })
    }

    pub(crate) fn incoming_requests(&self, me: &str) -> Result<Vec<RequestRow>> {
        // The inbox is the pending queue: answered requests (accepted/declined)
        // leave it, which is what makes "decline empties the inbox" observable.
        Ok(incoming_requests(&self.db().acquire(), me)?
            .into_iter()
            .map(request_row)
            .collect())
    }

    pub(crate) fn outgoing_requests(&self, me: &str) -> Result<Vec<RequestRow>> {
        // Outgoing keeps every status so the sender can see who declined.
        Ok(outgoing_requests(&self.db().acquire(), me)?
            .into_iter()
            .map(request_row)
            .collect())
    }

    pub(crate) fn friends(&self, me: &str) -> Result<Vec<FriendRow>> {
        Ok(friends(&self.db().acquire(), me)?
            .into_iter()
            .map(|f| FriendRow {
                friend_id: f.friend_id,
                remark: f.remark,
                source: f.source,
                created_at: f.created_at,
                starred: f.starred,
                tags: f.tags,
                friend_group: f.friend_group,
            })
            .collect())
    }

    pub(crate) fn are_friends(&self, a: &str, b: &str) -> Result<bool> {
        are_friends(&self.db().acquire(), a, b)
    }

    pub(crate) fn set_remark(&self, me: &str, friend: &str, remark: &str) -> Result<()> {
        let friend = validate::id(friend)?.to_string();
        let remark = validate::remark(remark)?.to_string();
        if !set_remark(&self.db().acquire(), me, &friend, &remark)? {
            return Err(PlatformError::NotFound);
        }
        Ok(())
    }

    /// Mutual removal: both rows and both log entries in one transaction.
    pub(crate) fn remove_friend(&self, me: &str, other: &str) -> Result<()> {
        let other = validate::id(other)?.to_string();
        let now = wxlike_core::types::now_ms() as i64;
        {
            let c = self.db().acquire();
            if !are_friends(&c, me, &other)? {
                return Err(PlatformError::NotFound);
            }
        }
        let mut c = self.db().lock();
        c.with_tx(|tx| -> Result<()> {
            let conn = crate::storage::StorageConnection::sqlite_tx(tx);
            delete_friend(&conn, me, &other)?;
            delete_friend(&conn, &other, me)?;
            for (a, b) in [(me, other.as_str()), (other.as_str(), me)] {
                log(&conn, a, b, "removed", now)?;
            }
            Ok(())
        })
    }

    /// Incremental address-book sync: everything for `me` after `since`.
    pub(crate) fn changes(&self, me: &str, since: i64, limit: i64) -> Result<Vec<ChangeRow>> {
        Ok(changes(&self.db().acquire(), me, since, limit)?
            .into_iter()
            .map(|c| ChangeRow {
                seq: c.seq,
                subject_id: c.subject_id,
                kind: c.kind,
                changed_at: c.changed_at,
            })
            .collect())
    }

    /// Highest log sequence, for the client to store as its next cursor.
    pub(crate) fn changes_head(&self, me: &str) -> Result<i64> {
        changes_head(&self.db().acquire(), me)
    }
}
