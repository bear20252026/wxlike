//! 仓储层：SQL 助手按 feature 分文件。
//!
//! 本层所有函数都是 `pub(in crate::domain)`——**handler 层编译不过**。
//! 新增 SQL 助手时不要图省事写成 `pub(crate)`，那等于把闸门拆了。
//!
//! 参数一律走绑定（`params!` / `?N`），本层不拼用户输入进 SQL。

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
