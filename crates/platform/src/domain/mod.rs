//! `wxlike-platform` 的领域层：业务规则与类型。
//!
//! # 分层
//!
//! 每个 feature 拆成两半：
//!
//! * `domain::<feature>` —— `impl Platform` + 纯函数 + 类型 + 常量 + 测试。
//! * `repo::<feature>` —— SQL 助手，可见性是 `pub(in crate::domain)`。
//!
//! handler 层（crate 根的 `<feature>.rs`）**在编译期看不到** `repo`：
//! 它只能走 `Platform` 的方法。这不是约定，是 `pub(in ...)` 的语义
//! （`repo` 嵌在 `domain` 里，`domain` 是它所有可见性路径的祖先）。
//!
//! 于是"handler 直接写 SQL"这类问题不再靠 review 拦，靠编译器拦。
//!
//! 唯一还在 crate 根的 `impl Platform` 是 `lib.rs` 里的基础设施
//! （构造、路由装配、事件/治理句柄注入）——它们不属于任何 feature；
//! 会话签发与审计日志已分别落在本层的 `sessions` / `audit`。

pub mod account;
pub mod articles;
pub mod audit;
pub mod call;
pub mod communities;
pub mod content_index;
pub mod devices;
pub mod favorites;
pub mod friends;
pub(crate) mod governance;
pub mod groups;
pub mod groups_admin;
pub mod identity_admin;
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

/// 仓储子模块清单只维护这一份：`repo` 的每个模块都必须有同名域模块，
/// 两份手写 `pub mod` 列表一旦漂移就会在 `repo::x` 路径上编译失败。
macro_rules! repo_modules {
    ($($name:ident),+ $(,)?) => {
        $(pub mod $name;)+
    };
}
pub(crate) use repo_modules;

pub mod repo;

/// 验证码字段的两段式读取：id 与答案必须同时存在，否则视为“没带验证码”。
pub(crate) fn captcha_pair<'a>(
    id: Option<&'a str>,
    answer: Option<&'a str>,
) -> Option<(&'a str, &'a str)> {
    match (id, answer) {
        (Some(id), Some(answer)) => Some((id, answer)),
        _ => None,
    }
}
