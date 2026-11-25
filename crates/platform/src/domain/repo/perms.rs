//! **仓储层**（`perms`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::error::Result;
use crate::storage::{params, StorageConnection};

pub(in crate::domain) fn rename_channel(
    c: &StorageConnection<'_>,
    channel: &str,
    name: &str,
) -> Result<()> {
    c.execute(
        "UPDATE channels SET name = ?1 WHERE id = ?2",
        params![name, channel],
    )?;
    Ok(())
}

pub(in crate::domain) fn set_channel_kind(
    c: &StorageConnection<'_>,
    channel: &str,
    kind: &str,
) -> Result<()> {
    c.execute(
        "UPDATE channels SET kind = ?1 WHERE id = ?2",
        params![kind, channel],
    )?;
    Ok(())
}

pub(in crate::domain) fn delete_channel_row(
    c: &StorageConnection<'_>,
    channel: &str,
) -> Result<()> {
    c.execute("DELETE FROM channels WHERE id = ?1", params![channel])?;
    Ok(())
}

pub(in crate::domain) fn set_member_role_row(
    c: &StorageConnection<'_>,
    community: &str,
    target: &str,
    role: i64,
) -> Result<()> {
    c.execute(
        "UPDATE community_members SET role = ?1 WHERE community_id = ?2 AND user_id = ?3",
        params![role, community, target],
    )?;
    Ok(())
}

pub(in crate::domain) fn remove_member(
    c: &StorageConnection<'_>,
    community: &str,
    target: &str,
) -> Result<()> {
    c.execute(
        "DELETE FROM community_members WHERE community_id = ?1 AND user_id = ?2",
        params![community, target],
    )?;
    Ok(())
}
