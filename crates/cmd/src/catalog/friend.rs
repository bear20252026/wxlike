//! 好友 / relations 域的命令声明（#329 起按域拆分；装配顺序在 `super` 的 CATALOG）。
//!
//! 本文件只有数据：每条命令一个 `spec(...)` / `read_no_args(...)` 调用，模板在
//! `super` 里只存在一份。逐条的标注理由写在各自上方的注释里，
//! 与拆分前 `catalog.rs` 中的原文一致。

use super::{
    read_no_args, spec, Capability, CommandSpec, Confirmation, Idempotency, Param, ParamType,
    ScopeClass,
};

pub(super) const FRIEND_LIST: CommandSpec = read_no_args(
    "friend.list",
    "好友列表（我的通讯录，按加入时间排）",
    ScopeClass::Account,
);

pub(super) const FRIEND_REQUESTS_INCOMING: CommandSpec = read_no_args(
    "friend.requests.incoming",
    "待我处理的好友申请（收件向，只含 pending）",
    ScopeClass::Account,
);

pub(super) const FRIEND_REQUESTS_OUTGOING: CommandSpec = read_no_args(
    "friend.requests.outgoing",
    "我发出去的好友申请（发起向，含已接受/已拒绝）",
    ScopeClass::Account,
);

pub(super) const FRIEND_REQUEST_CREATE: CommandSpec = spec(
    "friend.request.create",
    "发起好友申请（同一对象重复提交回 409，不会排第二队）",
    "{to_user_id: string, message?: string}",
    Capability::Write,
    false,
    ScopeClass::Account,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("to_user_id", ParamType::Str, "要添加的那个人 user_id"),
        Param::opt(
            "message",
            ParamType::Str,
            "申请附言；不给=只发空附言，服务端限长",
        ),
    ],
);

pub(super) const FRIEND_REQUEST_ACCEPT: CommandSpec = spec(
    "friend.request.accept",
    "同意一条好友申请（只有 pending 能被改，重复同意回 409）",
    "{req_id: string}",
    Capability::Write,
    false,
    ScopeClass::Account,
    Confirmation::None,
    Idempotency::None,
    &[Param::req(
        "req_id",
        ParamType::Str,
        "申请 id（incoming 列表里的 id 字段）",
    )],
);

pub(super) const FRIEND_REQUEST_DECLINE: CommandSpec = spec(
    "friend.request.decline",
    "拒绝一条好友申请（只有 pending 能被改，重复拒绝回 409）",
    "{req_id: string}",
    Capability::Write,
    false,
    ScopeClass::Account,
    Confirmation::None,
    Idempotency::None,
    &[Param::req(
        "req_id",
        ParamType::Str,
        "申请 id（incoming 列表里的 id 字段）",
    )],
);

pub(super) const FRIEND_REMARK_SET: CommandSpec = spec(
    "friend.remark.set",
    "给好友设置/修改备注（只有自己可见，随时可改）",
    "{user_id: string, remark: string}",
    Capability::Write,
    false,
    ScopeClass::Account,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("user_id", ParamType::Str, "好友的 user_id"),
        Param::req("remark", ParamType::Str, "备注名（服务端校验长度）"),
    ],
);

pub(super) const FRIEND_REMOVE: CommandSpec = spec(
    "friend.remove",
    "删除好友（双向移除、不可撤销，需确认令牌）",
    "{user_id: string, confirmation_token?: string}",
    Capability::Destructive,
    false,
    ScopeClass::Account,
    Confirmation::Required,
    Idempotency::None,
    &[
        Param::req("user_id", ParamType::Str, "要删除的好友 user_id"),
        Param::opt(
            "confirmation_token",
            ParamType::Str,
            "确认令牌：先 prepare，再把令牌带回来",
        ),
    ],
);

pub(super) const FRIEND_BLOCK_LIST: CommandSpec = read_no_args(
    "friend.block.list",
    "列出我的黑名单（只有自己看得到）",
    ScopeClass::Account,
);

pub(super) const FRIEND_BLOCK_ADD: CommandSpec = spec(
    "friend.block.add",
    "拉黑一个用户（其申请与陌生人消息将被拒绝，需确认令牌）",
    "{user_id: string, confirmation_token?: string}",
    Capability::Destructive,
    false,
    ScopeClass::Account,
    Confirmation::Required,
    Idempotency::None,
    &[
        Param::req("user_id", ParamType::Str, "要拉黑的 user_id"),
        Param::opt(
            "confirmation_token",
            ParamType::Str,
            "确认令牌：先 prepare，再把令牌带回来",
        ),
    ],
);

pub(super) const FRIEND_BLOCK_REMOVE: CommandSpec = spec(
    "friend.block.remove",
    "解除拉黑（恢复对方发申请与陌生人消息的通道）",
    "{user_id: string}",
    Capability::Write,
    false,
    ScopeClass::Account,
    Confirmation::None,
    Idempotency::None,
    &[Param::req(
        "user_id",
        ParamType::Str,
        "要解除拉黑的 user_id",
    )],
);

pub(super) const FRIEND_PRIVACY_GET: CommandSpec = read_no_args(
    "friend.privacy.get",
    "查看我的隐私规则（当前：是否允许陌生人发消息请求）",
    ScopeClass::Account,
);

pub(super) const FRIEND_PRIVACY_SET: CommandSpec = spec(
    "friend.privacy.set",
    "设置我的隐私规则（是否允许陌生人发消息请求）",
    "{allow: boolean}",
    Capability::Write,
    false,
    ScopeClass::Account,
    Confirmation::None,
    Idempotency::None,
    &[Param::req(
        "allow",
        ParamType::Bool,
        "true = 允许陌生人发消息请求（默认）；false = 直接拒绝",
    )],
);
