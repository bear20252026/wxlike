//! `sessions` 的逻辑端口（G00-04 S2 第一批）。

/// 新会话。`token` 是摘要（AR-3/H-2），不是明文。
#[derive(Debug, Clone)]
pub struct NewSession<'a> {
    pub token: &'a str,
    pub user_id: &'a str,
    pub created_at: i64,
    pub expires_at: i64,
    pub sid: Option<&'a str>,
    pub device_id: &'a str,
    pub device_name: &'a str,
    pub ip: &'a str,
    pub last_seen: i64,
    pub audience: &'a str,
}

/// 读出来的一行；`current` 由调用方按 token 比对，不在这里算。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub token: String,
    pub user_id: String,
    pub sid: Option<String>,
    pub device_id: String,
    pub device_name: String,
    pub ip: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub last_seen: i64,
    pub audience: String,
}

/// `sessions` 的唯一端口。
///
/// `delete_expired_batch` 只需要“有界删除”的语义：SQLite 实现用 `rowid` 子查询，
/// PostgreSQL 实现直接用 `token` 子查询，不依赖兼容视图的 `ctid` 伪列。
pub trait SessionsRepo {
    type Error;

    fn insert(&self, session: &NewSession<'_>) -> Result<(), Self::Error>;

    fn find_by_token(&self, token: &str) -> Result<Option<SessionRecord>, Self::Error>;

    fn sid_by_token(&self, token: &str) -> Result<Option<String>, Self::Error>;

    /// 该用户全部有效会话，最近活跃在前；顺手清掉过期行（与旧实现一致）。
    fn list_for_user(&self, user_id: &str, now: i64) -> Result<Vec<SessionRecord>, Self::Error>;

    fn delete_by_token(&self, token: &str) -> Result<usize, Self::Error>;

    fn delete_by_user(&self, user_id: &str) -> Result<usize, Self::Error>;

    fn delete_expired_batch(&self, now: i64, limit: i64) -> Result<usize, Self::Error>;

    fn revoke_by_sid(&self, user_id: &str, sid: &str) -> Result<usize, Self::Error>;

    fn revoke_others(&self, user_id: &str, keep_token: &str) -> Result<usize, Self::Error>;

    fn update_sid(&self, token: &str, sid: &str) -> Result<usize, Self::Error>;

    fn delete_by_sid_except_token(&self, sid: &str, token: &str) -> Result<usize, Self::Error>;

    fn delete_by_device(&self, user_id: &str, device_id: &str) -> Result<usize, Self::Error>;

    fn touch(&self, token: &str, now: i64) -> Result<usize, Self::Error>;
}
