//! **仓储层**（`relations`）：只干 SQL。可见性 `pub(in crate::domain)`——
//! handler 层在编译期看不到这里，绕不过去。

use crate::domain::relations::StrangerRequest;

pub(in crate::domain) fn row_request(r: &rusqlite::Row<'_>) -> rusqlite::Result<StrangerRequest> {
    Ok(StrangerRequest {
        id: r.get(0)?,
        from_uid: r.get(1)?,
        to_uid: r.get(2)?,
        text: r.get(3)?,
        status: r.get(4)?,
        created_at: r.get(5)?,
    })
}
