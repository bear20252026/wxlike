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

    /// 令牌所属会话的设备 id。外层 `None` = 没有这一行；内层 `None` = 行在但
    /// `device_id` 为空（历史会话确实可能没记），两者不能混成同一个答案。
    fn device_id_by_token(&self, token: &str) -> Result<Option<Option<String>>, Self::Error>;

    /// 该用户名下的会话行数（含过期行）。注销流程在落标记前用它报“撤了多少”。
    fn count_for_user(&self, user_id: &str) -> Result<i64, Self::Error>;

    /// 按 sid 读设备展示名；刷新轮换时用来沿用旧会话的设备名。行不存在或
    /// 设备名为空都返回 `None`，由调用方决定默认值。
    fn device_name_by_sid(&self, sid: &str) -> Result<Option<String>, Self::Error>;

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
