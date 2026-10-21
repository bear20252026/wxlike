//! `wxlike-ffi`：把客户端协议层暴露给移动端（ADR-11：Flutter + UniFFI）。
//!
//! 分工只有一条：**逻辑在 Rust，Dart 只画界面**。
//! 所以这个 crate 导出的每一个方法都在 Rust 侧做完该做的判断（权限、重试、
//! 离线兜底、消息怎么变成可渲染的行），再把结果交给绑定。Dart 拿到的
//! 是"可以直接渲染的东西"，不是"需要再算一遍的原始数据"。
//!
//! 三条边界：
//!
//! 1. **不碰 `crates/net`**。那边的 QUIC 客户端与 hub 在同一个 crate 里，
//!    链接它等于把 axum 塞进手机的 .so。移动端的实时性由 B4 的推送 +
//!    `history/unread` 的主动拉取承担。
//! 2. **命令一律走 `wxlike_client::execute::Executor`**。权限判定
//!    （`crates/cmd` 的 Policy）必须在发请求之前发生，移动端不能绕过它
//!    自己拼 URL——绕过就是第四套实现。
//! 3. **本地库只做渲染缓存**。未读、排序、序列号的真相在服务端信箱
//!    （`crates/message`）；这里的 SQLite 只存"断网时还能显示什么"。
//!
//! 会话凭据与消息正文存在应用私有目录的同一个 SQLite 文件里（与桌面端
//! `LocalStore` 同口径：本地不做静态加密）。这是已知待加固项，见
//! `docs/mobile-acceptance.md`。

// MSVC 的 link.exe 在给 cdylib 生成导入库时会往 stdout 打一行
// "正在创建库 … 和对象 …"。那是正常提示，但 rustc 1.9x 起把它算作
// `linker_messages` 警告，而六关里的 `clippy --all-targets -- -D warnings`
// 会因为链接器的一句客套话红掉。带上 `unknown_lints` 是为了在还没有这个
// lint 的旧 rustc 上不反过来产生"未知 lint"警告。
#![allow(unknown_lints, linker_messages)]

mod cache;
mod client;
mod error;
mod types;

pub use cache::Cache;
pub use client::{direct_conv_id, WxlikeClient};
pub use error::{classify, WxlikeError};
pub use types::{
    ChatMessage, ConversationList, ConversationRow, FlushReport, LiveInfo, LiveState, MessagePage,
    OutboxEntry, OutboxState, OutboxSummary, SendOutcome, SessionInfo, UnreadState,
};

uniffi::setup_scaffolding!();
