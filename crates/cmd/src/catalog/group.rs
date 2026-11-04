//! 群域的命令声明（#329 起按域拆分；装配顺序在 `super` 的 CATALOG）。
//!
//! 本文件只有数据：每条命令一个 `spec(...)` 调用，模板在
//! `super` 里只存在一份。逐条的标注理由写在各自上方的注释里，
//! 与拆分前 `catalog.rs` 中的原文一致。

use super::{
    spec, Capability, CommandSpec, Confirmation, Idempotency, Param, ParamType, ScopeClass,
};

pub(super) const GROUP_MEMBERS: CommandSpec = spec(
    "group.members",
    "群成员列表（按游标翻页；翻页目前切在客户端）",
    "{conv: string, after_user?: string, limit?: number}",
    Capability::Read,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("conv", ParamType::Str, "群 id"),
        Param::opt(
            "after_user",
            ParamType::Str,
            "游标：上一页最后一个 user_id，从它之后继续；它已不在名单里就给空页",
        ),
        Param::opt(
            "limit",
            ParamType::NonNegativeInt,
            "这一页最多几条（缺省=剩下的全给）",
        ),
    ],
);

pub(super) const GROUP_INFO: CommandSpec = spec(
    "group.info",
    "群基本信息与我的角色",
    "{conv: string}",
    Capability::Read,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[Param::req("conv", ParamType::Str, "群 id")],
);

pub(super) const GROUP_ANNOUNCEMENT_GET: CommandSpec = spec(
    "group.announcement.get",
    "查看群公告（仅群成员，无公告时报错）",
    "{conv: string}",
    Capability::Read,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[Param::req("conv", ParamType::Str, "群 id")],
);

pub(super) const GROUP_MUTE_ALL_GET: CommandSpec = spec(
    "group.mute_all.get",
    "查看全员禁言状态（仅群成员）",
    "{conv: string}",
    Capability::Read,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[Param::req("conv", ParamType::Str, "群 id")],
);

pub(super) const GROUP_CREATE: CommandSpec = spec(
    "group.create",
    "建群并拉入初始成员",
    "{name: string, member_ids: [string], idempotency_key: string}",
    Capability::Write,
    false,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::Required,
    &[
        Param::req("name", ParamType::Str, "群名"),
        Param::opt("member_ids", ParamType::StrList, "初始成员 user_id 列表"),
        Param::req("idempotency_key", ParamType::Str, "幂等键"),
    ],
);

pub(super) const GROUP_ADD_MEMBER: CommandSpec = spec(
    "group.add_member",
    "拉人进群",
    "{conv: string, user_ids: [string]}",
    Capability::Write,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("conv", ParamType::Str, "群 id"),
        Param::req("user_ids", ParamType::StrList, "要拉进来的 user_id 列表"),
    ],
);

pub(super) const GROUP_ANNOUNCEMENT_SET: CommandSpec = spec(
    "group.announcement.set",
    "设置/修改群公告（仅群主或管理员）",
    "{conv: string, text: string, media_id?: string}",
    Capability::Write,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("conv", ParamType::Str, "群 id"),
        Param::req(
            "text",
            ParamType::Str,
            "公告全文（上限 2000 字，服务端校验）",
        ),
        Param::opt(
            "media_id",
            ParamType::Str,
            "附带媒体 id（可选，服务端默认空）",
        ),
    ],
);

pub(super) const GROUP_MUTE_ALL_SET: CommandSpec = spec(
    "group.mute_all.set",
    "开启/关闭全员禁言（仅群主或管理员，对管理者无效）",
    "{conv: string, on: boolean}",
    Capability::Write,
    true,
    ScopeClass::Conversation,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("conv", ParamType::Str, "群 id"),
        Param::req("on", ParamType::Bool, "true = 开启全员禁言；false = 关闭"),
    ],
);

pub(super) const GROUP_KICK: CommandSpec = spec(
    "group.kick",
    "把成员移出群（不可撤销）",
    "{conv: string, user_id: string, confirmation_token?: string}",
    Capability::Destructive,
    true,
    ScopeClass::Conversation,
    Confirmation::Required,
    Idempotency::None,
    &[
        Param::req("conv", ParamType::Str, "群 id"),
        Param::req("user_id", ParamType::Str, "被移出的 user_id"),
        Param::opt(
            "confirmation_token",
            ParamType::Str,
            "确认令牌：先 prepare，再把令牌带回来",
        ),
    ],
);
