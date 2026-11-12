//! 仓储层：SQL 助手按 feature 分文件。
//!
//! 本层所有函数都是 `pub(in crate::domain)`——**handler 层编译不过**。
//! 新增 SQL 助手时不要图省事写成 `pub(crate)`，那等于把闸门拆了。
//!
//! 参数一律走绑定（`params!` / `?N`），本层不拼用户输入进 SQL。

pub mod account;
pub mod articles;
pub mod call;
pub mod communities;
pub mod content_index;
pub mod devices;
pub mod favorites;
pub mod friends;
pub mod groups;
pub mod groups_admin;
pub mod inbox;
pub mod keys;
pub mod live;
pub mod media;
pub mod moments;
pub mod notes;
pub mod notification;
pub mod perms;
pub mod qr_login;
pub mod read;
pub mod reconcile;
pub mod recovery;
pub mod relations;
pub mod report;
pub mod secret;
pub mod security;
pub mod sessions;
pub mod status;
pub mod stickers;
pub mod trust;
pub mod users;
pub mod video_feed;
pub mod video_refs;
pub mod wallet;
