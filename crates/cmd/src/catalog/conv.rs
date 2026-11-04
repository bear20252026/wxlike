//! 会话域的命令声明（#329 起按域拆分；装配顺序在 `super` 的 CATALOG）。
//!
//! 本文件只有数据：每条命令一个 `spec(...)` / `read_no_args(...)` 调用，模板在
//! `super` 里只存在一份。逐条的标注理由写在各自上方的注释里，
//! 与拆分前 `catalog.rs` 中的原文一致。

use super::{
    read_no_args, spec, Capability, CommandSpec, Confirmation, Idempotency, Param, ParamType,
    ScopeClass,
};

pub(super) const CONV_LIST: CommandSpec = read_no_args(
    "conv.list",
    "列出我参与的会话（含未读数）",
    ScopeClass::Conversation,
);

pub(super) const CONV_UNREAD: CommandSpec = spec(
    "conv.unread",
    "某个会话的未读快照：head_seq / read_seq / unread",
    "{conv: string}",
    Capability::Read,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[Param::req("conv", ParamType::Str, "会话 id")],
);

pub(super) const CONV_HISTORY: CommandSpec = spec(
    "conv.history",
    "翻历史消息，按会话内序列号倒序分页",
    "{conv: string, before_seq?: number, limit?: number}",
    Capability::Read,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("conv", ParamType::Str, "会话 id"),
        Param::opt("before_seq", ParamType::NonNegativeInt, "只取 seq 小于它的"),
        Param::opt("limit", ParamType::NonNegativeInt, "条数上限"),
    ],
);

pub(super) const CONV_SYNC: CommandSpec = spec(
    "conv.sync",
    "五层差分指令：客户端该全量拉还是增量拉",
    "{conv: string, version: number}",
    Capability::Read,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("conv", ParamType::Str, "会话 id"),
        Param::req("version", ParamType::NonNegativeInt, "客户端持有的版本号"),
    ],
);

pub(super) const CONV_ACK: CommandSpec = spec(
    "conv.ack",
    "把已读游标推到指定 seq（缺省=最新）",
    "{conv: string, seq?: number}",
    Capability::Write,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("conv", ParamType::Str, "会话 id"),
        Param::opt("seq", ParamType::NonNegativeInt, "推到哪个 seq"),
    ],
);
