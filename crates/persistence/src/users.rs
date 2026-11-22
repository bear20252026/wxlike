//! `users` 的逻辑端口（G00-04 S2 第一批）。

/// 新建账户的输入。字段与旧 `INSERT INTO users(...)` 的显式列一一对应。
#[derive(Debug, Clone)]
pub struct NewUser<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub pw_salt: &'a str,
    pub pw_hash: &'a str,
    pub pw_rounds: i64,
    pub pw_algo: &'a str,
    pub avatar: &'a str,
    pub signature: &'a str,
    pub created_at: i64,
}

/// 登录校验需要的最小行；不把 `name` 带进来，避免调用方误用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserCredentials {
    pub user_id: String,
    pub pw_salt: String,
    pub pw_hash: String,
    pub pw_rounds: i64,
    pub pw_algo: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserProfileRow {
    pub name: String,
    pub avatar: String,
    pub signature: String,
}

/// `users` 的唯一端口。
///
/// 方法按**业务动作**命名，不按 SQL 命名；SQLite / PostgreSQL 实现各自决定
/// 表名、占位符与视图兼容方式。`find_id_by_name` 的 `include_deleted` /
/// `exclude_id` 覆盖注册重名、登录、改资料三条既有查询，不额外拆成三个方法。
///
/// `Error` 由实现决定：平台侧是 `PlatformError`，身份 feature 侧可以是
/// 自己的错误类型；端口本身不依赖任何一方。
pub trait UsersRepo {
    type Error;

    fn find_id_by_name(
        &self,
        name: &str,
        include_deleted: bool,
        exclude_id: Option<&str>,
    ) -> Result<Option<String>, Self::Error>;

    fn load_credentials(&self, name: &str) -> Result<Option<UserCredentials>, Self::Error>;

    fn load_profile(&self, user_id: &str) -> Result<Option<UserProfileRow>, Self::Error>;

    /// 账号是否存在且未注销。资料更新前的存在性检查必须和“已注销不可写”
    /// 用同一口径，不能在 domain 里另写一条 SQL。
    fn active_exists(&self, user_id: &str) -> Result<bool, Self::Error>;

    /// 原子写资料补丁：只写 `Some` 字段；任一字段失败整批回滚。
    ///
    /// `None` 表示“不修改该字段”，空字符串是合法值，不能被当成不修改。
    fn update_profile(
        &self,
        user_id: &str,
        name: Option<&str>,
        avatar: Option<&str>,
        signature: Option<&str>,
    ) -> Result<usize, Self::Error>;

    fn insert(&self, user: &NewUser<'_>) -> Result<(), Self::Error>;

    fn update_name(&self, user_id: &str, name: &str) -> Result<usize, Self::Error>;

    fn update_avatar(&self, user_id: &str, avatar: &str) -> Result<usize, Self::Error>;

    fn update_signature(&self, user_id: &str, signature: &str) -> Result<usize, Self::Error>;

    fn mark_deleted(&self, user_id: &str, deleted_at: i64) -> Result<usize, Self::Error>;

    fn update_password_hash(&self, user_id: &str, phc: &str) -> Result<usize, Self::Error>;

    /// 读取 TOTP 密钥；行不存在或未配置都返回 `None`。是否启用由
    /// [`UsersRepo::totp_enabled`] 单独回答，避免把“没配”和“没启用”混在一起。
    fn totp_secret(&self, user_id: &str) -> Result<Option<String>, Self::Error>;

    fn totp_enabled(&self, user_id: &str) -> Result<bool, Self::Error>;

    fn set_totp_secret(&self, user_id: &str, secret: &str) -> Result<usize, Self::Error>;

    fn enable_totp(&self, user_id: &str) -> Result<usize, Self::Error>;

    fn disable_totp(&self, user_id: &str) -> Result<usize, Self::Error>;
}
