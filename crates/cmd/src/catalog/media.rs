//! 媒体域的命令声明（#329 起按域拆分；装配顺序在 `super` 的 CATALOG）。
//!
//! 本文件只有数据：每条命令一个 `spec(...)` 调用，模板在
//! `super` 里只存在一份。逐条的标注理由写在各自上方的注释里，
//! 与拆分前 `catalog.rs` 中的原文一致。

use super::{
    spec, Capability, CommandSpec, Confirmation, Idempotency, Param, ParamType, ScopeClass,
};

pub(super) const MEDIA_FILES_LIST: CommandSpec = spec(
    "media.files.list",
    "文件中心：按分类/文件夹翻页列出我能看到的文件与附件",
    "{category?: string, folder?: string, sort?: string, cursor?: string, limit?: number}",
    Capability::Read,
    false,
    ScopeClass::Media,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::opt(
            "category",
            ParamType::Str,
            "分类（如 file/image/video/voice，服务端定义）",
        ),
        Param::opt("folder", ParamType::Str, "文件夹（服务端定义）"),
        Param::opt("sort", ParamType::Str, "排序：recent（默认）/ name / size"),
        Param::opt("cursor", ParamType::Str, "上一页给的续页游标"),
        Param::opt(
            "limit",
            ParamType::NonNegativeInt,
            "本页条数（服务端有上限）",
        ),
    ],
);

pub(super) const MEDIA_UPLOAD_GET: CommandSpec = spec(
    "media.upload.get",
    "查一个上传会话的状态（进度、是否秒传完成、过期时间）",
    "{upload_id: string}",
    Capability::Read,
    false,
    ScopeClass::Media,
    Confirmation::None,
    Idempotency::None,
    &[Param::req(
        "upload_id",
        ParamType::Str,
        "上传会话 id（up_ 前缀）",
    )],
);

pub(super) const MEDIA_UPLOAD_START: CommandSpec = spec(
    "media.upload.start",
    "开一个上传会话（声明 digest 命中已有对象时秒传完成）",
    "{purpose: string, name: string, size: number, mime: string, digest?: string}",
    Capability::Write,
    false,
    ScopeClass::Media,
    Confirmation::None,
    Idempotency::None,
    &[
    Param::req(
    "purpose",
    ParamType::Str,
    "用途：attachment / avatar / sticker / voice / video / file / backup / derived（服务端校验）",
    ),
    Param::req("name", ParamType::Str, "文件名"),
    Param::req("size", ParamType::NonNegativeInt, "声明大小（字节）"),
    Param::req("mime", ParamType::Str, "MIME 类型"),
    Param::opt("digest", ParamType::Str, "内容 SHA-256（hex）：填了才有秒传快路径，服务端会核对"),
    ],
);

pub(super) const MEDIA_UPLOAD_COMPLETE: CommandSpec = spec(
    "media.upload.complete",
    "确认上传完成（分片传齐后落对象；未完成前对象不可引用）",
    "{upload_id: string, digest?: string}",
    Capability::Write,
    false,
    ScopeClass::Media,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("upload_id", ParamType::Str, "上传会话 id（up_ 前缀）"),
        Param::opt(
            "digest",
            ParamType::Str,
            "期望的内容 SHA-256（hex，可选，服务端核对）",
        ),
    ],
);

pub(super) const MEDIA_UPLOAD_CANCEL: CommandSpec = spec(
    "media.upload.cancel",
    "取消一个未完成的上传会话（释放其分片与配额）",
    "{upload_id: string}",
    Capability::Write,
    false,
    ScopeClass::Media,
    Confirmation::None,
    Idempotency::None,
    &[Param::req(
        "upload_id",
        ParamType::Str,
        "上传会话 id（up_ 前缀）",
    )],
);

pub(super) const MEDIA_OBJECT_ACCESS: CommandSpec = spec(
    "media.object.access",
    "为有权读的对象签发短期下载授权（带过期时间）",
    "{object_id: string}",
    Capability::Write,
    false,
    ScopeClass::Media,
    Confirmation::None,
    Idempotency::None,
    &[Param::req(
        "object_id",
        ParamType::Str,
        "对象 id（内容 digest）",
    )],
);

pub(super) const MEDIA_OBJECT_REVOKE: CommandSpec = spec(
    "media.object.revoke",
    "撤销一个对象已签发的全部下载授权（旧链接立即失效）",
    "{object_id: string}",
    Capability::Write,
    false,
    ScopeClass::Media,
    Confirmation::None,
    Idempotency::None,
    &[Param::req(
        "object_id",
        ParamType::Str,
        "对象 id（内容 digest）",
    )],
);
