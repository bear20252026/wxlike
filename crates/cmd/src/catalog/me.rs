//! 自查域的命令声明（#329 起按域拆分；装配顺序在 `super` 的 CATALOG）。
//!
//! 本文件只有数据：每条命令一个 `spec(...)` / `read_no_args(...)` 调用，模板在
//! `super` 里只存在一份。逐条的标注理由写在各自上方的注释里，
//! 与拆分前 `catalog.rs` 中的原文一致。

use super::{read_no_args, CommandSpec, ScopeClass};

pub(super) const ME_INFO: CommandSpec = read_no_args(
    "me.info",
    "我是谁：user_id、昵称、在线设备",
    ScopeClass::Account,
);

pub(super) const ME_SESSIONS: CommandSpec = read_no_args(
    "me.sessions",
    "我的登录会话（设备/时间），用于排查异地登录",
    ScopeClass::Account,
);
