//! **仓储层**（`account`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::{PlatformError, Result};
use crate::storage::{params, OptionalExt as _, StorageConnection, StorageRow};
use serde_json::Value;
use std::collections::BTreeMap;
use wxlike_identity::preferences::{Job, JobKind, JobStatus};

/// 注销后 display name 的替换前缀（真名是个人数据，不能留）。
const DELETED_NAME_PREFIX: &str = "deleted-";

/// 已注销标记。`None` = 没注销（或没有这个用户——调用方都得当成"不许清理"）。
pub(in crate::domain) fn deleted_at(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<Option<i64>> {
    let row = conn
        .query_row(
            "SELECT deleted_at FROM users WHERE id = ?1",
            params![user_id],
            |r: &StorageRow<'_>| r.get::<_, Option<i64>>(0),
        )
        .optional()?;
    Ok(row.flatten())
}

/// 有没有还在排队/执行中的注销作业。用的就是 `preferences::list` 这条
/// 既有查询，不另开一条 SQL——判"是否已在注销"的口径只能有一处。
pub(in crate::domain) fn open_delete_job(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<Option<Job>> {
    conn.query_row(
        "SELECT id, user_id, status, requested_at, artifact, bytes, error FROM identity_job \
         WHERE user_id = ?1 AND kind = 'delete' AND status IN ('queued', 'running') \
         ORDER BY requested_at DESC LIMIT 1",
        params![user_id],
        |r: &StorageRow<'_>| {
            let status = r.get::<_, String>(2)?;
            Ok(Job {
                id: r.get(0)?,
                user_id: r.get(1)?,
                kind: JobKind::Delete,
                status: if status == "running" {
                    JobStatus::Running
                } else {
                    JobStatus::Queued
                },
                requested_at: r.get(3)?,
                artifact: r.get(4)?,
                bytes: r.get(5)?,
                error: r.get(6)?,
            })
        },
    )
    .optional()
}

/// 钱包余额。用 SQL 的 COALESCE 表达"没有钱包行就是 0"，不在 Rust 里
/// `unwrap_or(0)`——那种写法会把"读库失败"也读成 0（然后真的去删钱包）。
pub(in crate::domain) fn wallet_balance(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<i64> {
    conn.query_row(
        "SELECT COALESCE((SELECT balance FROM wallets WHERE user_id = ?1), 0)",
        params![user_id],
        |r: &StorageRow<'_>| r.get(0),
    )
}

/// 一条只带一个用户 id 参数的查询 → JSON 数组，列名由调用方给。
fn rows_json(
    conn: &StorageConnection<'_>,
    sql: &str,
    user_id: &str,
    cols: &[&str],
) -> Result<Vec<Value>> {
    let stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![user_id], |r: &StorageRow<'_>| {
        let mut obj = serde_json::Map::new();
        for (i, name) in cols.iter().enumerate() {
            obj.insert((*name).to_string(), r.json_value(i)?);
        }
        Ok(Value::Object(obj))
    })?;
    rows.collect::<Result<Vec<_>>>()
}

/// 逐域执行 DELETE，返回每个域清掉的行数（写进作业的摘要）。
pub(in crate::domain) fn purge_rows(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<Vec<(&'static str, usize)>> {
    let mut by_domain: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (domain, sql) in PURGE_DELETES {
        let n = conn.execute(sql, params![user_id])?;
        *by_domain.entry(domain).or_insert(0) += n;
    }
    Ok(by_domain.into_iter().collect())
}

pub(in crate::domain) fn mask_rows(conn: &StorageConnection<'_>, user_id: &str) -> Result<()> {
    for (_, sql) in PURGE_MASKS {
        conn.execute(sql, params![user_id])?;
    }
    Ok(())
}

/// 匿名化用户行本身：display name、头像、签名都不是"已注销账号"该留的东西，
/// 口令材料全部清零（连摘要都不留，库副本泄露出去也换不回任何凭证）。
///
/// 行留着而不是 DELETE：`groups.owner_uid` / `communities.owner_uid` /
/// `threads.created_by` 三条外键没有 CASCADE，硬删会把无关的域顶坏。
pub(in crate::domain) fn anonymize_user(conn: &StorageConnection<'_>, user_id: &str) -> Result<()> {
    let anon_name = format!("{DELETED_NAME_PREFIX}{user_id}");
    let n = conn.execute(
        "UPDATE users SET name = ?2, avatar = '', signature = '', pw_salt = '', \
         pw_hash = '', pw_rounds = 0, pw_algo = 'deleted', totp_secret = NULL, \
         totp_enabled = 0 WHERE id = ?1",
        params![user_id, anon_name],
    )?;
    if n != 1 {
        // 用户行都不在，这次"清理成功"就是假的。
        return Err(PlatformError::NotFound);
    }
    Ok(())
}

// ── 导出取数 ────────────────────────────────────────────────────────────────
//
/// 导出六个域的取数 SQL。每条只有一个 `?1`（本人 user id）参数，
/// 取不到的列一律不取——口令摘要、TOTP 种子、pre-key 材料都不在导出里。
const SQL_PROFILE: &str = "SELECT id, name, avatar, signature, created_at FROM users \
     WHERE id = ?1 AND deleted_at IS NULL";
const PROFILE_COLS: &[&str] = &["id", "name", "avatar", "signature", "created_at"];

const SQL_CONTACTS: &str = "SELECT f.friend_id, u.name, f.remark, f.source, f.friend_group, \
                            f.created_at FROM friends f JOIN users u ON u.id = f.friend_id \
                            WHERE f.user_id = ?1 ORDER BY f.created_at";
const CONTACTS_COLS: &[&str] = &[
    "friend_id",
    "name",
    "remark",
    "source",
    "friend_group",
    "created_at",
];

/// `messages` 域：本人的收藏快照（原消息被撤回也仍在）。会话信箱本体在
/// `wxlike-message` 的读扩散库里、按会话而非按人归属，不在这里越界删。
const SQL_MESSAGES: &str = "SELECT kind, text, media_id, sender, sent_at_ms, source_kind, \
                            source_id, snapshot_label, created_at FROM favorites \
                            WHERE user_id = ?1 AND revoked = 0 ORDER BY created_at";
const MESSAGES_COLS: &[&str] = &[
    "kind",
    "text",
    "media_id",
    "sender",
    "sent_at_ms",
    "source_kind",
    "source_id",
    "snapshot_label",
    "created_at",
];

const SQL_MEDIA: &str = "SELECT id, size, content_type, conv_id, public, has_thumb, created_at \
                         FROM media WHERE uploader = ?1 ORDER BY created_at";
const MEDIA_COLS: &[&str] = &[
    "id",
    "size",
    "content_type",
    "conv_id",
    "public",
    "has_thumb",
    "created_at",
];

const SQL_LEDGER: &str = "SELECT kind, amount, counterparty, ref_id, memo, created_at FROM ledger \
                          WHERE user_id = ?1 ORDER BY id";
const LEDGER_COLS: &[&str] = &[
    "kind",
    "amount",
    "counterparty",
    "ref_id",
    "memo",
    "created_at",
];

const SQL_USER_SETTINGS: &str = "SELECT allow_strangers FROM user_settings WHERE user_id = ?1";
const USER_SETTINGS_COLS: &[&str] = &["allow_strangers"];

pub(in crate::domain) fn export_profile(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<Vec<Value>> {
    rows_json(conn, SQL_PROFILE, user_id, PROFILE_COLS)
}

pub(in crate::domain) fn export_contacts(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<Vec<Value>> {
    rows_json(conn, SQL_CONTACTS, user_id, CONTACTS_COLS)
}

pub(in crate::domain) fn export_messages(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<Vec<Value>> {
    rows_json(conn, SQL_MESSAGES, user_id, MESSAGES_COLS)
}

pub(in crate::domain) fn export_media(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<Vec<Value>> {
    rows_json(conn, SQL_MEDIA, user_id, MEDIA_COLS)
}

pub(in crate::domain) fn export_ledger(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<Vec<Value>> {
    rows_json(conn, SQL_LEDGER, user_id, LEDGER_COLS)
}

pub(in crate::domain) fn export_user_settings(
    conn: &StorageConnection<'_>,
    user_id: &str,
) -> Result<Vec<Value>> {
    rows_json(conn, SQL_USER_SETTINGS, user_id, USER_SETTINGS_COLS)
}

/// 注销清理：八个域落到具体表的 DELETE。
///
/// 口径与 `preferences.rs` 模块头一致——**本人名下的行删掉，法定/业务必需的
/// 记录由对应域自己留着**（见 [`PURGE_MASKS`]）。
///
/// 顺序有意义：子行先于父行（外键不是 DEFERRABLE）。
const PURGE_DELETES: &[(&str, &str)] = &[
    // 会话与一次性凭证。会话在 `request_delete` 里已经撤过，这里是兜底：
    // 任何"清账之后又冒出来的会话行"都必须在这一步消失。
    ("sessions", "DELETE FROM sessions WHERE user_id = ?1"),
    (
        "sessions",
        "DELETE FROM identity_step_up WHERE user_id = ?1",
    ),
    // 导出下载令牌是**凭据**：注销后还能兑换，等于把数据留在门外面。
    (
        "sessions",
        "DELETE FROM identity_export_link WHERE user_id = ?1",
    ),
    (
        "sessions",
        "DELETE FROM identity_reset_token WHERE user_id = ?1",
    ),
    ("sessions", "DELETE FROM recovery_codes WHERE user_id = ?1"),
    (
        "device_keys",
        "DELETE FROM prekey_bundles WHERE user_id = ?1",
    ),
    ("device_keys", "DELETE FROM devices WHERE user_id = ?1"),
    (
        "device_keys",
        "DELETE FROM identity_device WHERE user_id = ?1",
    ),
    ("messages", "DELETE FROM favorites WHERE user_id = ?1"),
    ("messages", "DELETE FROM contact_log WHERE user_id = ?1"),
    (
        "messages",
        "DELETE FROM notification_settings WHERE user_id = ?1",
    ),
    ("messages", "DELETE FROM notifications WHERE user_id = ?1"),
    (
        "messages",
        "DELETE FROM message_requests WHERE from_uid = ?1",
    ),
    ("messages", "DELETE FROM message_requests WHERE to_uid = ?1"),
    ("messages", "DELETE FROM thread_subs WHERE user_id = ?1"),
    (
        "messages",
        "DELETE FROM secret_verifications WHERE user_id = ?1",
    ),
    ("messages", "DELETE FROM live_viewers WHERE user_id = ?1"),
    ("media", "DELETE FROM content_media_refs WHERE owner = ?1"),
    // 表里的媒体行删掉就再也取不到；字节本体由媒体域自己的 GC 回收——
    // 平台的 MediaStore 接口没有 delete，这里不越界造一个。
    ("media", "DELETE FROM media WHERE uploader = ?1"),
    ("relations", "DELETE FROM friends WHERE user_id = ?1"),
    ("relations", "DELETE FROM friends WHERE friend_id = ?1"),
    ("relations", "DELETE FROM blocks WHERE user_id = ?1"),
    ("relations", "DELETE FROM blocks WHERE blocked_id = ?1"),
    (
        "relations",
        "DELETE FROM friend_requests WHERE from_uid = ?1",
    ),
    ("relations", "DELETE FROM friend_requests WHERE to_uid = ?1"),
    ("relations", "DELETE FROM group_members WHERE user_id = ?1"),
    ("relations", "DELETE FROM group_mutes WHERE user_id = ?1"),
    (
        "relations",
        "DELETE FROM group_join_requests WHERE user_id = ?1",
    ),
    (
        "relations",
        "DELETE FROM group_invites WHERE created_by = ?1",
    ),
    (
        "relations",
        "DELETE FROM community_members WHERE user_id = ?1",
    ),
    ("relations", "DELETE FROM channel_role WHERE user_id = ?1"),
    (
        "relations",
        "DELETE FROM publisher_follows WHERE user_id = ?1",
    ),
    ("relations", "DELETE FROM article_reads WHERE user_id = ?1"),
    (
        "relations",
        "DELETE FROM article_reactions WHERE user_id = ?1",
    ),
    ("relations", "DELETE FROM video_follows WHERE user_id = ?1"),
    ("relations", "DELETE FROM video_follows WHERE creator = ?1"),
    ("relations", "DELETE FROM video_likes WHERE user_id = ?1"),
    ("relations", "DELETE FROM moment_likes WHERE user_id = ?1"),
    (
        "relations",
        "DELETE FROM status_reactions WHERE user_id = ?1",
    ),
    ("relations", "DELETE FROM user_stickers WHERE user_id = ?1"),
    ("commerce", "DELETE FROM wallet_idem WHERE user_id = ?1"),
    // 余额非零在 `run_delete_job` 里就被拒绝了，走到这里必须是 0。
    ("commerce", "DELETE FROM wallets WHERE user_id = ?1"),
    ("content", "DELETE FROM moment_comments WHERE author = ?1"),
    ("content", "DELETE FROM moments WHERE author = ?1"),
    ("content", "DELETE FROM moment_drafts WHERE author = ?1"),
    ("content", "DELETE FROM video_comments WHERE author = ?1"),
    ("content", "DELETE FROM video_posts WHERE author = ?1"),
    ("content", "DELETE FROM articles WHERE author = ?1"),
    ("content", "DELETE FROM notes WHERE user_id = ?1"),
    ("content", "DELETE FROM sticker_packs WHERE owner = ?1"),
    (
        "content",
        "DELETE FROM status_background_drafts WHERE owner = ?1",
    ),
    ("content", "DELETE FROM statuses WHERE owner = ?1"),
    ("content", "DELETE FROM publishers WHERE owner = ?1"),
    ("search_index", "DELETE FROM content_index WHERE owner = ?1"),
    (
        "preferences",
        "DELETE FROM identity_pref WHERE user_id = ?1",
    ),
    (
        "preferences",
        "DELETE FROM user_settings WHERE user_id = ?1",
    ),
];

/// 留下的记录做**脱敏**而不是删：账目是资金守恒的证据（审计要能对上），
/// 红包的领取关系是**别人**账目的凭证——删掉会让另一个用户的账说不清。
const PURGE_MASKS: &[(&str, &str)] = &[
    (
        "commerce",
        "UPDATE ledger SET memo = '', counterparty = '', ref_id = '' WHERE user_id = ?1",
    ),
    (
        "commerce",
        "UPDATE redpackets SET memo = '', claimers = '[]' WHERE from_uid = ?1",
    ),
];

#[cfg(test)]
mod pg_tests {
    use super::{deleted_at, open_delete_job, purge_rows, wallet_balance};
    use crate::storage::params;
    use crate::storage::StorageProfile;
    use crate::Db;

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL and M014 applied"]
    fn postgres_account_jobs_and_wallet_are_readable() {
        let Ok(url) = std::env::var("WXLIKE_TEST_PG_URL") else {
            return;
        };
        let db = Db::open_profile(StorageProfile::Postgres { url }, None).expect("pg db");
        let conn = db.acquire();
        let user = "pg-account-user";
        conn.execute(
            "DELETE FROM identity_job WHERE user_id = ?1",
            params![user],
        )
        .expect("clean jobs");
        conn.execute(
            "INSERT INTO identity_job(id, user_id, kind, status, requested_at) \
             VALUES(?1, ?2, 'delete', 'queued', ?3)",
            params!["pg-job-1", user, 7_i64],
        )
        .expect("insert job");
        let job = open_delete_job(&conn, user)
            .expect("query job")
            .expect("job exists");
        assert_eq!(job.id, "pg-job-1");
        assert!(matches!(
            job.status,
            wxlike_identity::preferences::JobStatus::Queued
        ));
        assert_eq!(wallet_balance(&conn, user).expect("wallet"), 0);
        assert_eq!(deleted_at(&conn, user).expect("deleted"), None);
        let counts = purge_rows(&conn, user).expect("purge");
        assert!(counts.iter().all(|(_, n)| *n == 0));
    }
}
