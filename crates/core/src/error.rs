//! 共享错误基础设施。
//!
//! 这个模块存在的理由：`IdError` 与 `RelError` 各自定义了 5 个**逐字相同**的
//! 变体（`NotFound` / `Conflict` / `Corrupt` / `Db` / `Json`）。同一句错误
//! 消息、同一种语义，只是挂在不同的枚举上——这是纯复制，加一个域就要再抄一遍。
//!
//! 提取到这里的价值不在"少写几行"，而在**统一错误分层**：
//!
//! * **技术基础设施错误**（数据库、序列化、IO）→ 用 `StorageError` 表达，
//!   各 crate 的错误类型通过 `From` 自动转换，不必各自重复定义。
//! * **领域语义错误**（`Failed` / `Pending` / `Cancelled`）→ 留在各 crate 的
//!   枚举里，因为它们**不是同一件事**：`AdminError::Failed` 和
//!   `RtcError::Failed` 语义完全不同。
//!
//! 迁移策略是**渐进式**的：现有 `IdError::NotFound(...)` 调用点继续工作，
//! 新代码可以用 `StorageError::not_found` 构造后 `?` 自动转换。
//!
//! 这是 `#[doc(hidden)]` 的内部 API，不进生产代码的直接返回路径。

/// 存储层的共享错误变体。
///
/// 这些变体在多个 crate 的错误枚举中重复出现，提取到这里避免跨文件复制。
/// 各 crate 的错误类型实现 `From<StorageError>` 后，可以用 `?` 自动转换。
#[doc(hidden)]
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("not found: {0}")]
    NotFound(&'static str),
    #[error("conflict: {0}")]
    Conflict(&'static str),
    #[error("corrupt stored row: {0}")]
    Corrupt(&'static str),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

impl StorageError {
    /// 构造一个"未找到"错误。
    pub fn not_found(what: &'static str) -> Self {
        Self::NotFound(what)
    }

    /// 构造一个"冲突"错误。
    pub fn conflict(what: &'static str) -> Self {
        Self::Conflict(what)
    }

    /// 构造一个"数据损坏"错误。
    ///
    /// 库里的一行读不出合法状态：这是**数据损坏**，不是客户端错误。
    /// 以前的做法是把无法识别的值折成一个"看着还行"的默认值（作业种类折成
    /// export、游标折成 0），坏数据就此伪装成功能正常。宁可让请求失败，也不
    /// 给用户/评审一个假象。
    pub fn corrupt(what: &'static str) -> Self {
        Self::Corrupt(what)
    }

    /// 从 `rusqlite::Error` 转换。
    pub fn db(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }

    /// 从 `serde_json::Error` 转换。
    pub fn json(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
