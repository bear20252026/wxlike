//! 视频 / 点播域的命令声明（#329 起按域拆分；装配顺序在 `super` 的 CATALOG）。
//!
//! 本文件只有数据：每条命令一个 `spec(...)` 调用，模板在
//! `super` 里只存在一份。逐条的标注理由写在各自上方的注释里，
//! 与拆分前 `catalog.rs` 中的原文一致。

use super::{
    spec, Capability, CommandSpec, Confirmation, HostCapability, Idempotency, Param, ParamType,
    ScopeClass,
};

pub(super) const VIDEO_SELECT: CommandSpec = spec(
    "video.select",
    "按实测带宽选一档码率（服务端留余量，三端选到同一档）",
    "{video_id: string, kbps: number}",
    Capability::Read,
    false,
    ScopeClass::Media,
    Confirmation::None,
    Idempotency::None,
    &[
        Param::req("video_id", ParamType::Str, "视频 id"),
        Param::req(
            "kbps",
            ParamType::NonNegativeInt,
            "实测可用带宽（千比特/秒）",
        ),
    ],
);

pub(super) const VIDEO_STATS: CommandSpec = spec(
    "video.stats",
    "点播观看统计：条数 / 独立观众 / 总时长 / 人均时长",
    "{video_id: string}",
    Capability::Read,
    false,
    ScopeClass::Media,
    Confirmation::None,
    Idempotency::None,
    &[Param::req("video_id", ParamType::Str, "视频 id")],
);

pub(super) const VIDEO_JOB_GET: CommandSpec = spec(
    "video.job.get",
    "查一个转码作业的状态与失败原因",
    "{job_id: string}",
    Capability::Read,
    false,
    ScopeClass::Media,
    Confirmation::None,
    Idempotency::None,
    &[Param::req("job_id", ParamType::Str, "作业 id")],
);

pub(super) const VIDEO_JOB_CREATE: CommandSpec = super::with_host(
    spec(
        "video.job.create",
        "提交一个本地视频文件转码（读本地路径，上传后入队）",
        "{path: string, callback_url?: string}",
        Capability::Write,
        false,
        ScopeClass::Media,
        Confirmation::None,
        Idempotency::None,
        &[
            Param::req("path", ParamType::Str, "本地文件路径"),
            Param::opt("callback_url", ParamType::Str, "完成后回调地址"),
        ],
    ),
    HostCapability::FilePicker,
);

pub(super) const VIDEO_JOB_RETRY: CommandSpec = spec(
    "video.job.retry",
    "重试一个失败的转码作业（同一条作业，不新建）",
    "{job_id: string}",
    Capability::Write,
    false,
    ScopeClass::Media,
    Confirmation::None,
    Idempotency::None,
    &[Param::req("job_id", ParamType::Str, "作业 id")],
);

pub(super) const VIDEO_VIEW_REPORT: CommandSpec = spec(
    "video.view.report",
    "上报一次观看心跳（本次新增时长必须为正）",
    "{video_id: string, viewer: string, watched_ms: number, position_ms?: number, completed?: boolean, session_id?: string}",
    Capability::Write,
    false,
    ScopeClass::Media,
    Confirmation::None,
    Idempotency::None,
    &[
    Param::req("video_id", ParamType::Str, "视频 id"),
    Param::req("viewer", ParamType::Str, "观众 user_id"),
    Param::req("watched_ms", ParamType::NonNegativeInt, "本次新增观看时长"),
    Param::opt("position_ms", ParamType::NonNegativeInt, "当前播放位置"),
    Param::opt("completed", ParamType::Bool, "是否看完"),
    Param::opt("session_id", ParamType::Str, "播放会话 id"),
    ],
);
