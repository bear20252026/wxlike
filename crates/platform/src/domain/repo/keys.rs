//! **仓储层**（`keys`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::{params, OptionalExt as _, StorageConnection};


pub(in crate::domain) fn upsert_bundle(
    c: &StorageConnection<'_>,
    user: &str,
    identity: &str,
    identity_signing: &str,
    signature: &str,
    otks: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "INSERT INTO prekey_bundles(user_id, identity, identity_signing, signature, otks, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(user_id) DO UPDATE SET
           identity = excluded.identity,
           identity_signing = excluded.identity_signing,
           signature = excluded.signature,
           otks = excluded.otks,
           updated_at = excluded.updated_at",
        params![user, identity, identity_signing, signature, otks, now],
    )?;
    Ok(())
}

pub(in crate::domain) fn shares_group(c: &StorageConnection<'_>, a: &str, b: &str) -> Result<bool> {
    let shares: Option<i64> = c
        .query_row(
            "SELECT 1 FROM group_members a JOIN group_members b
                ON a.group_id = b.group_id
             WHERE a.user_id = ?1 AND b.user_id = ?2 LIMIT 1",
            params![a, b],
            |r| r.get(0),
        )
        .optional()?;
    Ok(shares.is_some())
}

pub(in crate::domain) fn identity_for(
    c: &StorageConnection<'_>,
    user: &str,
) -> Result<Option<String>> {
    c.query_row(
        "SELECT identity FROM prekey_bundles WHERE user_id = ?1",
        params![user],
        |r| r.get(0),
    )
    .optional()
}

pub(in crate::domain) struct BundleRow {
    pub(crate) identity: String,
    pub(crate) identity_signing: String,
    pub(crate) signature: String,
    pub(crate) otks: String,
}

pub(in crate::domain) fn bundle_row(
    c: &StorageConnection<'_>,
    user: &str,
) -> Result<Option<BundleRow>> {
    c.query_row(
        "SELECT identity, identity_signing, signature, otks FROM prekey_bundles WHERE user_id = ?1",
        params![user],
        |r| {
            Ok(BundleRow {
                identity: r.get(0)?,
                identity_signing: r.get(1)?,
                signature: r.get(2)?,
                otks: r.get(3)?,
            })
        },
    )
    .optional()
}

pub(in crate::domain) fn update_otks(
    c: &StorageConnection<'_>,
    user: &str,
    otks: &str,
    now: i64,
) -> Result<()> {
    c.execute(
        "UPDATE prekey_bundles SET otks = ?1, updated_at = ?2 WHERE user_id = ?3",
        params![otks, now, user],
    )?;
    Ok(())
}
