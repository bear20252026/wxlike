//! 后台可见字段的单一白名单。
//!
//! 这张表同时被 identity 的检索投影与 admin 的脱敏日志使用；两边过去各抄
//! 一份，字段增减时靠"记得改两处"。白名单必须默认收紧——未知字段不出现，
//! 而不是默认放开——所以它只在这里定义一次，新增字段要先过这里。

/// 后台允许看到的用户元数据字段。
pub const METADATA_FIELDS: &[&str] = &[
    "user_id",
    "display_name",
    "state",
    "created_at",
    "last_seen_ms",
    "device_count",
    "group_count",
    "report_count",
    "suspended_until_ms",
    "labels",
];
