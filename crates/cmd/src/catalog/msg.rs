//! 消息域的命令声明（#329 起按域拆分；装配顺序在 `super` 的 CATALOG）。
//!
//! 本文件只有数据：每条命令一个 `spec(...)` 调用，模板在
//! `super` 里只存在一份。逐条的标注理由写在各自上方的注释里，
//! 与拆分前 `catalog.rs` 中的原文一致。

use super::{
    spec, Capability, CommandSpec, Confirmation, Idempotency, Param, ParamType, ScopeClass,
};

pub(super) const MSG_LIST: CommandSpec = spec(
    "msg.list",
    "按 seq 区间取消息（不翻页，给脚本用）",
    "{conv: string, from_seq: number, to_seq?: number}",
    Capability::Read,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("conv", ParamType::Str, "会话 id"),
        Param::req("from_seq", ParamType::NonNegativeInt, "起始 seq（含）"),
        Param::opt("to_seq", ParamType::NonNegativeInt, "结束 seq（含）"),
    ],
);

pub(super) const MSG_SEARCH: CommandSpec = spec(
    "msg.search",
    "在可见会话内全文搜索",
    "{q: string, limit?: number}",
    Capability::Read,
    false,
    ScopeClass::Content,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("q", ParamType::Str, "搜索词"),
        Param::opt("limit", ParamType::NonNegativeInt, "条数上限"),
    ],
);

pub(super) const MSG_GET: CommandSpec = spec(
    "msg.get",
    "取单条消息（含撤回状态）",
    "{conv: string, msg_id: string}",
    Capability::Read,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("conv", ParamType::Str, "会话 id"),
        Param::req("msg_id", ParamType::Str, "消息 id"),
        // #315 撤销声明：这里原本挂着 `confirmation_token?`，但本条是
        // `Confirmation::None` 的读命令——`ConfirmationStore::prepare` 对
        // 非 Required 的命令直接回 `NotRequired`（`crate::confirmation::prepare`），
        // 令牌**根本签发不出来**，MCP 的 `confirm_step` 也只在
        // `spec.confirmation.is_required()` 时才走（`crates/mcp/src/server.rs:275-283`）。
        // 留着它等于让 AI 以为"读一条消息要先确认"。
        // 同一条不变式由 `crates/cmd/tests/idempotency_annotations.rs` 守着：
        // 声明 `confirmation_token` ⇔ `Confirmation::Required`。
    ],
);

pub(super) const MSG_SEND: CommandSpec = spec(
    "msg.send",
    "发一条文本消息（必须带 idempotency_key，重试不会重复发）",
    "{conv: string, text: string, reply_to?: string, idempotency_key: string}",
    Capability::Write,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::Required,
    &[
        Param::req("conv", ParamType::Str, "会话 id"),
        Param::req("text", ParamType::Str, "消息正文"),
        Param::opt(
            "reply_to",
            ParamType::Str,
            "引用回复：被引消息的 msg_id（hex）；服务端会校验它属于本会话且未撤回",
        ),
        Param::req("idempotency_key", ParamType::Str, "幂等键"),
    ],
);

pub(super) const MSG_FORWARD: CommandSpec = spec(
    "msg.forward",
    "转发一条消息到另一个会话",
    "{from_conv: string, to_conv: string, msg_id: string, idempotency_key: string}",
    Capability::Write,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::Required,
    &[
        Param::req("from_conv", ParamType::Str, "来源会话"),
        Param::req("to_conv", ParamType::Str, "目标会话"),
        Param::req("msg_id", ParamType::Str, "消息 id"),
        Param::req("idempotency_key", ParamType::Str, "幂等键"),
    ],
);

pub(super) const MSG_RECALL: CommandSpec = spec(
    "msg.recall",
    "撤回一条消息（他人消息需管理员）",
    "{conv: string, msg_id: string, confirmation_token?: string}",
    Capability::Destructive,
    true,
    ScopeClass::Conversation,
    Confirmation::Required,
    Idempotency::None,
    &[
        Param::req("conv", ParamType::Str, "会话 id"),
        Param::req("msg_id", ParamType::Str, "消息 id"),
        Param::opt(
            "confirmation_token",
            ParamType::Str,
            "确认令牌：先 prepare，再把令牌带回来",
        ),
    ],
);
