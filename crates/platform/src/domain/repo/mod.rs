//! 仓储层：SQL 助手按 feature 分文件。
//!
//! 本层所有函数都是 `pub(in crate::domain)`——**handler 层编译不过**。
//! 新增 SQL 助手时不要图省事写成 `pub(crate)`，那等于把闸门拆了。
//!
//! 参数一律走绑定（`params!` / `?N`），本层不拼用户输入进 SQL。

pub(in crate::domain) mod preferences;

/// 生成"按 `(id, user_id)` 删一行并返回受影响行数"的 `delete_one`。
///
/// 收藏、笔记等按 (行 id, 属主) 归属的表只有表名不同；表名用 `literal`
/// 传入，拼进 SQL 的永远是编译期常量，用户输入仍走 `params!`。
macro_rules! forward_backend {
    ($(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty;)*) => {
        $(
            fn $name(&self, $($arg: $ty),*) -> Result<$ret> {
                match self {
                    Self::Sqlite(repo) => repo.$name($($arg),*),
                    Self::Postgres(repo) => repo.$name($($arg),*),
                }
            }
        )*
    };
}
pub(in crate::domain) use forward_backend;

macro_rules! delete_owned_row {
    ($table:literal) => {
        pub(in crate::domain) fn delete_one(
            c: &StorageConnection<'_>,
            user: &str,
            id: &str,
        ) -> Result<usize> {
            c.execute(
                concat!("DELETE FROM ", $table, " WHERE id = ?1 AND user_id = ?2"),
                crate::storage::params![id, user],
            )
        }
    };
}

crate::domain::repo_modules! {
    account,
    articles,
    audit,
    call,
    communities,
    content_index,
    devices,
    favorites,
    friends,
    governance,
    groups,
    groups_admin,
    inbox,
    keys,
    live,
    media,
    moments,
    notes,
    notification,
    perms,
    qr_login,
    read,
    reconcile,
    recovery,
    relations,
    report,
    secret,
    security,
    sessions,
    status,
    stickers,
    trust,
    users,
    video_feed,
    video_refs,
    wallet,
}
