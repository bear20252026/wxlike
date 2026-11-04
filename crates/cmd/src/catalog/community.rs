//! 社区域的命令声明（#329 起按域拆分；装配顺序在 `super` 的 CATALOG）。
//!
//! 本文件只有数据：每条命令一个 `spec(...)` 调用，模板在
//! `super` 里只存在一份。逐条的标注理由写在各自上方的注释里，
//! 与拆分前 `catalog.rs` 中的原文一致。

use super::{
    spec, Capability, CommandSpec, Confirmation, Idempotency, Param, ParamType, ScopeClass,
};

pub(super) const COMMUNITY_THREADS: CommandSpec = spec(
    "community.threads",
    "社区里正在讨论的话题（跨频道汇总，带回复数与我的订阅状态）",
    "{community: string, limit?: number}",
    Capability::Read,
    false,
    ScopeClass::Content,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("community", ParamType::Str, "社区 id"),
        Param::opt("limit", ParamType::NonNegativeInt, "条数上限"),
    ],
);
