//! 频道域的命令声明（#329 起按域拆分；装配顺序在 `super` 的 CATALOG）。
//!
//! 本文件只有数据：每条命令一个 `spec(...)` 调用，模板在
//! `super` 里只存在一份。逐条的标注理由写在各自上方的注释里，
//! 与拆分前 `catalog.rs` 中的原文一致。

use super::{
    spec, Capability, CommandSpec, Confirmation, Idempotency, Param, ParamType, ScopeClass,
};

pub(super) const CHANNEL_ROLES: CommandSpec = spec(
    "channel.roles",
    "频道角色名单：谁在这个频道里是 owner/moderator/member/guest，含继承标记",
    "{conv: string}",
    Capability::Read,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[Param::req("conv", ParamType::Str, "频道 id")],
);
