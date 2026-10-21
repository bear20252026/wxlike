//! 请求体提取器——实现在 `wxlike-api`，这里只保留路径。
//!
//! 为什么保留这个模块而不是让调用方直接 `use wxlike_api::ApiJson`：
//! `admin` / `server` 有十几处 `use wxlike_platform::extract::ApiJson;`，
//! 为了搬一个类型去改十几行 import 是无谓的 churn。更重要的是——
//! **形状只有一个定义**（在 `wxlike-api`），这里没有第二份实现，
//! 所以不会出现"平台收 JSON、视频收纯文本"那种漂移。

pub use wxlike_api::{ApiJson, ApiResult};
