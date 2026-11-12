//! **仓储层**（`communities`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::domain::communities::ChannelRow;

pub(in crate::domain) fn row_channel(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelRow> {
    Ok(ChannelRow {
        id: r.get(0)?,
        community_id: r.get(1)?,
        name: r.get(2)?,
        kind: r.get(3)?,
        tenant_id: r.get(4)?,
        created_at: r.get(5)?,
    })
}
