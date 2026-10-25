//! G03-03: conversation projection, unread derivation and cross-device
//! preferences.
//!
//! The list is a projection table with a stable keyset cursor — never an
//! `OFFSET` scan over the mailbox. Unread is derived as
//! `last_seq - max(read_cursor, cleared_before_seq)`, so it is identical on
//! every device of one account. Pin/mute/archive are account-scoped preferences
//! (server rows), while "clear local history" and "delete for everyone" are
//! deliberately different commands with different domain events.

use rusqlite::{params, OptionalExtension as _};

use super::{MailError, MessageStore, OwnerType, Result};

/// Who may see the page. The mailbox does not own membership (the social layer
/// does), so the caller passes the set it is authorised for.
///
/// [`ConversationScope::All`] is the moderation/audit path; a client list must
/// use [`ConversationScope::Only`] with the conversation ids its owner belongs
/// to — otherwise every conversation in the deployment would be enumerable from
/// the list endpoint.
#[derive(Clone, Copy, Debug)]
pub enum ConversationScope<'a> {
    All,
    Only(&'a [String]),
}

/// Projection filter. `None` means "do not constrain this dimension".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConversationFilter {
    pub archived: Option<bool>,
    pub pinned: Option<bool>,
    pub muted: Option<bool>,
    pub include_hidden: bool,
}

impl Default for ConversationFilter {
    /// Main list: not archived, hidden conversations removed.
    fn default() -> Self {
        Self {
            archived: Some(false),
            pinned: None,
            muted: None,
            include_hidden: false,
        }
    }
}

impl ConversationFilter {
    pub fn all() -> Self {
        Self {
            archived: None,
            pinned: None,
            muted: None,
            include_hidden: true,
        }
    }

    pub fn archived_only() -> Self {
        Self {
            archived: Some(true),
            ..Self::default()
        }
    }
}

/// Keyset cursor over `(pinned DESC, last_at_ms DESC, conv_id DESC)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationCursor {
    pub pinned: bool,
    pub last_at_ms: u64,
    pub conv_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationRow {
    pub conv_id: String,
    pub owner_type: String,
    pub last_seq: u64,
    pub last_at_ms: u64,
    pub last_sender: String,
    pub last_type: String,
    pub preview: String,
    pub member_count: u64,
    pub last_withdrawn: bool,
    pub pinned: bool,
    pub muted: bool,
    pub archived: bool,
    pub hidden: bool,
    pub pref_version: u64,
    pub cleared_before_seq: u64,
    pub unread: u64,
}

impl ConversationRow {
    pub fn cursor(&self) -> ConversationCursor {
        ConversationCursor {
            pinned: self.pinned,
            last_at_ms: self.last_at_ms,
            conv_id: self.conv_id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConversationPage {
    pub items: Vec<ConversationRow>,
    pub next: Option<ConversationCursor>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConvPrefs {
    pub pinned: bool,
    pub muted: bool,
    pub archived: bool,
    pub hidden: bool,
    pub version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationEvent {
    pub id: i64,
    pub actor: String,
    pub conv_id: String,
    pub kind: String,
    pub payload: String,
    pub at_ms: u64,
}

/// A preference that is shared by all of a user's devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefField {
    Pinned,
    Muted,
    Archived,
}

impl PrefField {
    fn column(self) -> &'static str {
        match self {
            PrefField::Pinned => "pinned",
            PrefField::Muted => "muted",
            PrefField::Archived => "archived",
        }
    }
}

/// Upper bound for an `Only` scope: the ids become SQL placeholders, and the
/// projection is keyset-paginated anyway, so a caller that needs more than this
/// is paging conversations, not listing them.
pub const MAX_SCOPE_CONVERSATIONS: usize = 400;

impl MessageStore {
    /// conversation.list — keyset paginated, filtered, unread derived.
    ///
    /// The query itself is built by `list_page_query()` so this stays a
    /// fetch-and-page loop rather than a 100-line SQL blob.
    pub fn conversations(
        &self,
        user: &str,
        scope: ConversationScope<'_>,
        filter: ConversationFilter,
        after: Option<&ConversationCursor>,
        limit: usize,
    ) -> Result<ConversationPage> {
        let limit = limit.clamp(1, MAX_PAGE) as i64;
        let (sql, args) = list_page_query(user, scope, filter, after, limit)?;
        let c = self.c();
        let mut stmt = c.prepare(&sql)?;
        let mut items = stmt
            .query_map(rusqlite::params_from_iter(args.iter()), row_to_conversation)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let has_more = items.len() > limit as usize;
        if has_more {
            items.truncate(limit as usize);
        }
        let next = if has_more {
            items.last().map(ConversationRow::cursor)
        } else {
            None
        };
        Ok(ConversationPage { items, next })
    }

    pub fn conversation_prefs(&self, user: &str, conv_id: &str) -> Result<ConvPrefs> {
        let c = self.c();
        let row: Option<(i64, i64, i64, i64, i64)> = c
            .query_row(
                "SELECT pinned, muted, archived, hidden, version FROM conversation_pref
                  WHERE user_id = ?1 AND conv_id = ?2",
                params![user, conv_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        Ok(row
            .map(|(p, m, a, h, v)| ConvPrefs {
                pinned: p != 0,
                muted: m != 0,
                archived: a != 0,
                hidden: h != 0,
                version: v as u64,
            })
            .unwrap_or(ConvPrefs {
                pinned: false,
                muted: false,
                archived: false,
                hidden: false,
                version: 0,
            }))
    }

    /// conversation.pin/mute/archive — idempotent. A repeated write with the
    /// same value does not bump the version, so duplicate commands do not cause
    /// spurious cross-device sync.
    pub fn set_conversation_pref(
        &self,
        user: &str,
        conv_id: &str,
        field: PrefField,
        value: bool,
    ) -> Result<ConvPrefs> {
        let now = wxlike_core::types::now_ms();
        let mut conn = self.c();
        let tx = conn.transaction()?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM conversation_projection WHERE conv_id = ?1",
                params![conv_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(MailError::BadOwner(conv_id.to_string()));
        }
        tx.execute(
            "INSERT OR IGNORE INTO conversation_pref(
                 user_id, conv_id, pinned, muted, archived, hidden, version, updated_at)
             VALUES(?1, ?2, 0, 0, 0, 0, 0, ?3)",
            params![user, conv_id, now as i64],
        )?;
        let current: (i64, i64, i64, i64, i64) = tx.query_row(
            "SELECT pinned, muted, archived, hidden, version FROM conversation_pref
              WHERE user_id = ?1 AND conv_id = ?2",
            params![user, conv_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        let unchanged = match field {
            PrefField::Pinned => (current.0 != 0) == value,
            PrefField::Muted => (current.1 != 0) == value,
            PrefField::Archived => (current.2 != 0) == value,
        };
        if !unchanged {
            tx.execute(
                &format!(
                    "UPDATE conversation_pref
                        SET {column} = ?3, version = version + 1, updated_at = ?4
                      WHERE user_id = ?1 AND conv_id = ?2",
                    column = field.column()
                ),
                params![user, conv_id, value as i64, now as i64],
            )?;
        }
        tx.commit()?;
        drop(conn);
        self.conversation_prefs(user, conv_id)
    }

    /// conversation.read — account cursor (unread) plus the device cursor.
    pub fn conversation_read(
        &self,
        user: &str,
        device_id: &str,
        conv_id: &str,
        seq: u64,
    ) -> Result<bool> {
        self.read_ack(user, device_id, conv_id, seq)
    }

    /// conversation.clear — only this user's view. Messages stay in the
    /// mailbox, other members' unread and search are untouched.
    pub fn clear_local_history(&self, user: &str, conv_id: &str, before_seq: u64) -> Result<u64> {
        let now = wxlike_core::types::now_ms();
        let mut conn = self.c();
        let tx = conn.transaction()?;
        let head: Option<i64> = tx
            .query_row(
                "SELECT last_seq FROM conversation_projection
                  WHERE conv_id = ?1 AND deleted = 0",
                params![conv_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(head) = head else {
            return Err(MailError::BadOwner(conv_id.to_string()));
        };
        let before = before_seq.min(head as u64);
        let current: i64 = tx
            .query_row(
                "SELECT cleared_before_seq FROM conversation_clear
                  WHERE user_id = ?1 AND conv_id = ?2",
                params![user, conv_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let next = current.max(before as i64);
        tx.execute(
            "INSERT INTO conversation_clear(user_id, conv_id, cleared_before_seq, updated_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(user_id, conv_id) DO UPDATE SET
                 cleared_before_seq = excluded.cleared_before_seq,
                 updated_at = excluded.updated_at",
            params![user, conv_id, next, now as i64],
        )?;
        if next > current {
            emit_event(
                &tx,
                user,
                conv_id,
                "conversation.cleared",
                &format!("{{\"cleared_before_seq\":{next}}}"),
                now,
            )?;
        }
        tx.commit()?;
        Ok(next as u64)
    }

    /// Remove the conversation from this user's list. Distinct from
    /// [`MessageStore::delete_conversation`].
    pub fn hide_conversation(&self, user: &str, conv_id: &str) -> Result<()> {
        let now = wxlike_core::types::now_ms();
        let mut conn = self.c();
        let tx = conn.transaction()?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM conversation_projection WHERE conv_id = ?1",
                params![conv_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(MailError::BadOwner(conv_id.to_string()));
        }
        tx.execute(
            "INSERT INTO conversation_pref(
                 user_id, conv_id, pinned, muted, archived, hidden, version, updated_at)
             VALUES(?1, ?2, 0, 0, 0, 1, 1, ?3)
             ON CONFLICT(user_id, conv_id) DO UPDATE SET
                 hidden = 1,
                 version = conversation_pref.version + CASE
                     WHEN conversation_pref.hidden = 1 THEN 0 ELSE 1 END,
                 updated_at = excluded.updated_at",
            params![user, conv_id, now as i64],
        )?;
        emit_event(&tx, user, conv_id, "conversation.deleted_local", "{}", now)?;
        tx.commit()?;
        Ok(())
    }

    /// conversation.delete — hide for everyone (soft delete of the projection).
    /// Permission (owner/admin) is checked by the caller; the domain event
    /// tells search and attachment services to drop references.
    pub fn delete_conversation(&self, actor: &str, conv_id: &str) -> Result<()> {
        let now = wxlike_core::types::now_ms();
        let mut conn = self.c();
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE conversation_projection SET deleted = 1, updated_at = ?2
              WHERE conv_id = ?1 AND deleted = 0",
            params![conv_id, now as i64],
        )?;
        if changed == 0 {
            return Err(MailError::BadOwner(conv_id.to_string()));
        }
        emit_event(&tx, actor, conv_id, "conversation.deleted", "{}", now)?;
        emit_event(
            &tx,
            actor,
            conv_id,
            "search.index.remove",
            "{\"scope\":\"conversation\"}",
            now,
        )?;
        emit_event(
            &tx,
            actor,
            conv_id,
            "attachment.release",
            "{\"scope\":\"conversation\"}",
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Member-count maintenance for the list (system messages remain ordinary
    /// `system` timeline rows, so their preview/unread behaviour is unchanged).
    pub fn set_member_count(&self, owner: (OwnerType, &str), count: u64) -> Result<()> {
        let now = wxlike_core::types::now_ms();
        let c = self.c();
        c.execute(
            "INSERT INTO conversation_projection(
                 conv_id, owner_type, last_seq, last_at_ms, last_sender, last_type, preview,
                 member_count, deleted, last_withdrawn, updated_at)
             VALUES(?1, ?2, 0, 0, '', '', '', ?3, 0, 0, ?4)
             ON CONFLICT(conv_id) DO UPDATE SET
                 member_count = excluded.member_count,
                 updated_at = excluded.updated_at",
            params![owner.1, owner.0.as_str(), count as i64, now as i64],
        )?;
        Ok(())
    }

    /// Domain event feed for search/attachment invalidation. Consumers keep an
    /// id cursor; events are append-only.
    pub fn conversation_events_since(
        &self,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<ConversationEvent>> {
        let limit = limit.clamp(1, 500) as i64;
        let c = self.c();
        let mut stmt = c.prepare(
            "SELECT id, actor, conv_id, kind, payload, at_ms FROM conversation_event
              WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![after_id, limit], |r| {
                Ok(ConversationEvent {
                    id: r.get(0)?,
                    actor: r.get(1)?,
                    conv_id: r.get(2)?,
                    kind: r.get(3)?,
                    payload: r.get(4)?,
                    at_ms: r.get::<_, i64>(5)? as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// Rows per page ceiling: a client asking for "all conversations" gets a page.
pub const MAX_PAGE: usize = 200;

/// Build the keyset query and its bound arguments.
///
/// `Only` expands to one placeholder per conversation id (values are bound,
/// never interpolated). The row-value comparison in the cursor predicate is
/// exactly the `ORDER BY` tuple, which is what makes paging repeat-free.
fn list_page_query(
    user: &str,
    scope: ConversationScope<'_>,
    filter: ConversationFilter,
    after: Option<&ConversationCursor>,
    limit: i64,
) -> Result<(String, Vec<rusqlite::types::Value>)> {
    let visible = match scope {
        ConversationScope::All => None,
        ConversationScope::Only(ids) => {
            if ids.len() > MAX_SCOPE_CONVERSATIONS {
                return Err(MailError::InvalidInput("conversation scope is too large"));
            }
            Some(ids)
        }
    };
    let placeholders: Vec<String> = (0..visible.map_or(0, <[String]>::len))
        .map(|i| format!("?{}", FIRST_SCOPE_PARAM + i))
        .collect();
    let scope_sql = if placeholders.is_empty() {
        String::new()
    } else {
        format!(" AND c.conv_id IN ({})", placeholders.join(","))
    };
    let sql = format!(
        "SELECT {LIST_COLUMNS}{LIST_UNREAD}
           FROM conversation_projection c
           LEFT JOIN conversation_pref p ON p.conv_id = c.conv_id AND p.user_id = ?1
           LEFT JOIN conversation_clear cl ON cl.conv_id = c.conv_id AND cl.user_id = ?1
           LEFT JOIN read_cursor rc ON rc.owner_type = c.owner_type
                                   AND rc.owner_id = c.conv_id AND rc.user_id = ?1
          WHERE c.deleted = 0
            AND (?2 IS NULL OR COALESCE(p.archived, 0) = ?2)
            AND (?3 IS NULL OR COALESCE(p.pinned, 0) = ?3)
            AND (?4 IS NULL OR COALESCE(p.muted, 0) = ?4)
            AND (?5 = 1 OR COALESCE(p.hidden, 0) = 0)
            AND (?6 IS NULL OR
                 (COALESCE(p.pinned, 0), c.last_at_ms, c.conv_id) < (?6, ?7, ?8)){scope_sql}
          ORDER BY COALESCE(p.pinned, 0) DESC, c.last_at_ms DESC, c.conv_id DESC
          LIMIT ?9"
    );
    let args = list_args(user, visible, filter, after, limit);
    Ok((sql, args))
}

/// Positional bind values for [`list_page_query`]: the eight filter/cursor slots
/// followed by the scope ids.
fn list_args(
    user: &str,
    visible: Option<&[String]>,
    filter: ConversationFilter,
    after: Option<&ConversationCursor>,
    limit: i64,
) -> Vec<rusqlite::types::Value> {
    let num = |v: Option<i64>| {
        v.map(rusqlite::types::Value::Integer)
            .unwrap_or(rusqlite::types::Value::Null)
    };
    let mut args: Vec<rusqlite::types::Value> = vec![
        rusqlite::types::Value::Text(user.to_string()),
        num(filter.archived.map(bool_to_int)),
        num(filter.pinned.map(bool_to_int)),
        num(filter.muted.map(bool_to_int)),
        rusqlite::types::Value::Integer(bool_to_int(filter.include_hidden)),
        num(after.map(|c| bool_to_int(c.pinned))),
        num(after.map(|c| c.last_at_ms as i64)),
        match after {
            Some(c) => rusqlite::types::Value::Text(c.conv_id.clone()),
            None => rusqlite::types::Value::Null,
        },
        rusqlite::types::Value::Integer(limit + 1),
    ];
    if let Some(ids) = visible {
        args.extend(
            ids.iter()
                .map(|id| rusqlite::types::Value::Text(id.clone())),
        );
    }
    args
}

fn bool_to_int(v: bool) -> i64 {
    v as i64
}

/// The first placeholder number used by the scope `IN (...)` list.
const FIRST_SCOPE_PARAM: usize = 10;

/// Selected columns of the list page, in decode order.
const LIST_COLUMNS: &str = "c.conv_id, c.owner_type, c.last_seq, c.last_at_ms, c.last_sender,
                    c.last_type, c.preview, c.member_count, c.last_withdrawn,
                    COALESCE(p.pinned, 0), COALESCE(p.muted, 0), COALESCE(p.archived, 0),
                    COALESCE(p.hidden, 0), COALESCE(p.version, 0),
                    COALESCE(cl.cleared_before_seq, 0)";

/// Unread is a difference, never a counter: `last_seq` minus the further of the
/// account read cursor and this viewer's cleared-watermark.
const LIST_UNREAD: &str = ",
                    CASE WHEN c.last_seq > MAX(COALESCE(rc.last_ack_seq, 0),
                                               COALESCE(cl.cleared_before_seq, 0))
                         THEN c.last_seq - MAX(COALESCE(rc.last_ack_seq, 0),
                                               COALESCE(cl.cleared_before_seq, 0))
                         ELSE 0 END";

/// Column positions of the list page decode (see `LIST_COLUMNS` + unread).
const C_MUTED: usize = 10;
const C_ARCHIVED: usize = 11;
const C_HIDDEN: usize = 12;
const C_PREF_VERSION: usize = 13;
const C_CLEARED: usize = 14;
const C_UNREAD: usize = 15;

fn row_to_conversation(r: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationRow> {
    Ok(ConversationRow {
        conv_id: r.get(0)?,
        owner_type: r.get(1)?,
        last_seq: r.get::<_, i64>(2)? as u64,
        last_at_ms: r.get::<_, i64>(3)? as u64,
        last_sender: r.get(4)?,
        last_type: r.get(5)?,
        preview: r.get(6)?,
        member_count: r.get::<_, i64>(7)? as u64,
        last_withdrawn: r.get::<_, i64>(8)? != 0,
        pinned: r.get::<_, i64>(9)? != 0,
        muted: r.get::<_, i64>(C_MUTED)? != 0,
        archived: r.get::<_, i64>(C_ARCHIVED)? != 0,
        hidden: r.get::<_, i64>(C_HIDDEN)? != 0,
        pref_version: r.get::<_, i64>(C_PREF_VERSION)? as u64,
        cleared_before_seq: r.get::<_, i64>(C_CLEARED)? as u64,
        unread: r.get::<_, i64>(C_UNREAD)? as u64,
    })
}

fn emit_event(
    tx: &rusqlite::Transaction<'_>,
    actor: &str,
    conv_id: &str,
    kind: &str,
    payload: &str,
    now: u64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO conversation_event(actor, conv_id, kind, payload, at_ms)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![actor, conv_id, kind, payload, now as i64],
    )?;
    Ok(())
}
