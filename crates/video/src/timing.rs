//! 共享密钥比较的入口。
//!
//! 实现已收进 `wxlike_core::secure::constant_time_eq`（全仓唯一一份）；
//! 这里只保留既有的 `crate::timing::constant_time_eq` 路径，调用方不用改。

pub(crate) use wxlike_core::secure::constant_time_eq;
