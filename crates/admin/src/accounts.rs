//! adminx **自己的** SQLite：管理员账号、会话、操作留痕（D3）。
//!
//! 为什么是单独一个库文件而不是往平台库里加表：
//! * 平台库的 schema 归串行轨道管（`docs/parallel-tracks.md` §三"任何 schema
//!   迁移"），adminx 无权也不该去 `ALTER` 它；
//! * 后台账号是**运维面**的身份，与平台用户是两套凭据。把它们塞进 `users`
//!   表会让"运营账号"变成一个能登录 IM 的普通用户，那是权限泄露；
//! * 备份边界清楚：`adminx.db` 里只有账号与留痕，平台库里只有业务数据。
//!
//! 凭据原语不重写：argon2id 与 TOTP 都调 `wxlike-platform` 的公开函数——
//! 参数与容差只该有一份真相。

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::{params, Connection, Row, ToSql};
use serde::Serialize;
use wxlike_report::Action;

use crate::error::{AdminError, Result};
use crate::rbac::Role;

/// 用户名长度上限。比平台的 64 短是有意的：这个串会进 `x-admin-actor` 头，
/// 也会出现在审计与留痕里，短一点更好读。
pub const NAME_MAX_CHARS: usize = 32;

/// 留痕 detail 的上限。留痕是给追责看的，截断反而误导，所以超长直接拒。
pub const DETAIL_MAX_CHARS: usize = 2_048;

const ACCOUNT_COLS: &str = "id, name, pw_hash, role, totp_secret, totp_enabled, disabled, \
     created_at, updated_at";

/// 一次写操作的留痕（谁做的 + 做了什么 + 结果）。
#[derive(Clone, Debug)]
pub struct OpRecord {
    pub account_id: String,
    pub actor: String,
    pub op: String,
    pub capability: String,
    pub target: String,
    pub upstream: String,
    pub status: OpStatus,
    pub detail: String,
    pub mfa: bool,
}

/// 留痕的状态结论。`Denied` 也记录：运维想知道的常常是"谁试过但没成"。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpStatus {
    /// 平台确认执行成功。
    Ok,
    /// 被 adminx 的准入闸门挡下（角色不足 / 没 MFA / 参数非法）。
    Denied,
    /// 平台返回了非 2xx。
    UpstreamError,
    /// 根本没连上平台（进程没起 / 超时）。
    TransportError,
}

impl OpStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            OpStatus::Ok => "ok",
            OpStatus::Denied => "denied",
            OpStatus::UpstreamError => "upstream_error",
            OpStatus::TransportError => "transport_error",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub disabled: bool,
    /// 这次登录是否必须带动态码（= 账号已确认激活 TOTP）。
    pub mfa_enabled: bool,
    /// 只说"有没有生成过密钥"，永不透出密钥本身。
    pub totp_provisioned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

struct AccountRow {
    id: String,
    name: String,
    pw_hash: String,
    role: String,
    totp_secret: Option<String>,
    totp_enabled: bool,
    disabled: bool,
    created_at: i64,
    updated_at: i64,
}

impl AccountRow {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            name: r.get(1)?,
            pw_hash: r.get(2)?,
            role: r.get(3)?,
            totp_secret: r.get(4)?,
            totp_enabled: r.get::<_, i64>(5)? != 0,
            disabled: r.get::<_, i64>(6)? != 0,
            created_at: r.get(7)?,
            updated_at: r.get(8)?,
        })
    }

    /// 角色解析失败 = 库里的值和目前的枚举对不上。**必须报错**，
    /// 不许"认不出就按最小权限处理"——那会让一个提了权的账号突然只读，
    /// 运维会去查平台，而问题在后台自己的库里。
    fn role(&self) -> Result<Role> {
        Role::parse(&self.role).ok_or_else(|| {
            AdminError::bad_input(format!(
                "账号 {} 的角色 {:?} 无法识别（可选 viewer|moderator|superadmin）",
                self.id, self.role
            ))
        })
    }

    fn public(&self) -> Result<Account> {
        Ok(Account {
            id: self.id.clone(),
            name: self.name.clone(),
            role: self.role()?,
            disabled: self.disabled,
            mfa_enabled: self.totp_enabled,
            totp_provisioned: self.totp_secret.is_some(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Session {
    pub account_id: String,
    pub actor: String,
    pub role: Role,
    /// 这次登录有没有真的验过动态码。写操作的闸门看这个字段。
    pub mfa: bool,
    pub expires_at: i64,
}

#[derive(Clone)]
pub struct AccountStore {
    inner: Arc<Mutex<Connection>>,
}

impl AccountStore {
    /// 打开（必要时创建）adminx 自己的库。这张库是 adminx 唯一允许写的地方。
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;

            CREATE TABLE IF NOT EXISTS admin_accounts (
                id           TEXT PRIMARY KEY,
                name         TEXT NOT NULL UNIQUE,
                pw_hash      TEXT NOT NULL,
                role         TEXT NOT NULL DEFAULT 'viewer',
                totp_secret  TEXT,
                totp_enabled INTEGER NOT NULL DEFAULT 0,
                disabled     INTEGER NOT NULL DEFAULT 0,
                created_at   INTEGER NOT NULL,
                updated_at   INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS admin_sessions (
                token      TEXT PRIMARY KEY,
                account_id TEXT NOT NULL REFERENCES admin_accounts(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                mfa        INTEGER NOT NULL DEFAULT 0,
                ip         TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_as_account ON admin_sessions(account_id, expires_at);

            -- 留痕：每次"试图写"都落一行（含被拒的）。这是 D3 的验收面。
            CREATE TABLE IF NOT EXISTS admin_oplog (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                at_ms      INTEGER NOT NULL,
                account_id TEXT NOT NULL,
                actor      TEXT NOT NULL,
                op         TEXT NOT NULL,
                capability TEXT NOT NULL,
                target     TEXT NOT NULL DEFAULT '',
                upstream   TEXT NOT NULL DEFAULT '',
                status     TEXT NOT NULL,
                detail     TEXT NOT NULL DEFAULT '',
                mfa        INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_ol_actor ON admin_oplog(account_id, at_ms);
            CREATE INDEX IF NOT EXISTS idx_ol_op ON admin_oplog(op, at_ms);
            "#,
        )?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    /// 锁中毒只可能发生在 SQL panic 中途，连接还活着：恢复守卫比崩掉好。
    /// `pub(crate)` 是给本 crate 的测试留的口子——它们要**手工把库改坏**来验证
    /// "认不出的角色/状态必须报错"，而集成测试一律只走公开方法。
    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // -----------------------------------------------------------------------
    // 账号
    // -----------------------------------------------------------------------

    /// 用户名规则与平台同源且**更严：只收 ASCII**。这个名字会当作
    /// `x-admin-actor` 头发给平台——头值里出现换行就是请求走私，
    /// 出现不可见字符就是审计造假。
    pub fn check_name(name: &str) -> Result<()> {
        if name.trim().is_empty() || name != name.trim() {
            return Err(AdminError::bad_input("用户名不能为空或带首尾空白"));
        }
        let count = name.chars().count();
        if count > NAME_MAX_CHARS {
            return Err(AdminError::bad_input(format!(
                "用户名长度需在 1..={NAME_MAX_CHARS}"
            )));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@'))
        {
            return Err(AdminError::bad_input(
                "用户名只允许 ASCII 字母数字与 - _ . @（这个名字会进 HTTP 头与审计）",
            ));
        }
        Ok(())
    }

    pub fn check_password(pw: &str) -> Result<()> {
        let n = pw.chars().count();
        if n < 10 {
            return Err(AdminError::bad_input(
                "管理员口令至少 10 个字符（后台能处置工单、能改审核模式）",
            ));
        }
        if n > 256 {
            return Err(AdminError::bad_input("管理员口令过长"));
        }
        Ok(())
    }

    pub fn add(&self, name: &str, password: &str, role: Role) -> Result<Account> {
        Self::check_name(name)?;
        Self::check_password(password)?;
        let now = crate::read::now_ms();
        let id = wxlike_platform::random_hex(8);
        let hash = wxlike_platform::hash_argon2id(password)?;
        let inserted = self.lock().execute(
            "INSERT INTO admin_accounts(id, name, pw_hash, role, created_at, updated_at) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, name, hash, role.as_str(), now],
        );
        if let Err(err) = inserted {
            if err.to_string().contains("UNIQUE constraint failed") {
                return Err(AdminError::AccountExists);
            }
            return Err(AdminError::from(err));
        }
        Ok(Account {
            id,
            name: name.to_string(),
            role,
            disabled: false,
            mfa_enabled: false,
            totp_provisioned: false,
            created_at: now,
            updated_at: now,
        })
    }

    /// 一个账号都没有 = 写面全部不可用（CLI 与 `/health` 都要提示这件事）。
    pub fn has_any_account(&self) -> Result<bool> {
        let n: i64 = self
            .lock()
            .query_row("SELECT COUNT(*) FROM admin_accounts", [], |r| r.get(0))?;
        Ok(n > 0)
    }

    pub fn list(&self) -> Result<Vec<Account>> {
        let c = self.lock();
        let sql = format!("SELECT {ACCOUNT_COLS} FROM admin_accounts ORDER BY name");
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map([], AccountRow::from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?.public()?);
        }
        Ok(out)
    }

    fn row_by_name(&self, name: &str) -> Result<Option<AccountRow>> {
        let c = self.lock();
        let sql = format!("SELECT {ACCOUNT_COLS} FROM admin_accounts WHERE name = ?1");
        let mut stmt = c.prepare(&sql)?;
        let mut rows = stmt.query_map(params![name], AccountRow::from_row)?;
        // 只取第一行：name 上有 UNIQUE 索引，多行意味着库被手工改坏
        match rows.next() {
            None => Ok(None),
            Some(r) => Ok(Some(r?)),
        }
    }

    pub fn account(&self, name: &str) -> Result<Option<Account>> {
        Ok(match self.row_by_name(name)? {
            Some(r) => Some(r.public()?),
            None => None,
        })
    }

    /// 换口令同时吊销全部会话：否则旧令牌还能继续用——与平台的
    /// "重置密码即下线"是同一条要求。
    pub fn set_password(&self, name: &str, password: &str) -> Result<Account> {
        Self::check_password(password)?;
        let row = self
            .row_by_name(name)?
            .ok_or_else(|| AdminError::bad_input("没有这个管理员账号"))?;
        let hash = wxlike_platform::hash_argon2id(password)?;
        let now = crate::read::now_ms();
        {
            let c = self.lock();
            c.execute(
                "UPDATE admin_accounts SET pw_hash = ?2, updated_at = ?3 WHERE id = ?1",
                params![row.id, hash, now],
            )?;
            c.execute(
                "DELETE FROM admin_sessions WHERE account_id = ?1",
                params![row.id],
            )?;
        }
        Ok(Account {
            updated_at: now,
            ..row.public()?
        })
    }

    pub fn set_disabled(&self, name: &str, disabled: bool) -> Result<Account> {
        let row = self
            .row_by_name(name)?
            .ok_or_else(|| AdminError::bad_input("没有这个管理员账号"))?;
        let now = crate::read::now_ms();
        {
            let c = self.lock();
            c.execute(
                "UPDATE admin_accounts SET disabled = ?2, updated_at = ?3 WHERE id = ?1",
                params![row.id, disabled as i64, now],
            )?;
            if disabled {
                c.execute(
                    "DELETE FROM admin_sessions WHERE account_id = ?1",
                    params![row.id],
                )?;
            }
        }
        Ok(Account {
            disabled,
            updated_at: now,
            ..row.public()?
        })
    }

    /// 生成（或重生成）TOTP 密钥。未确认前不改变 `totp_enabled`——与平台的
    /// `totp_enable` 同一语义，避免"扫了码就以为开了"。
    pub fn totp_enroll(&self, name: &str) -> Result<String> {
        let row = self
            .row_by_name(name)?
            .ok_or_else(|| AdminError::bad_input("没有这个管理员账号"))?;
        let secret = wxlike_platform::totp_generate_secret();
        self.lock().execute(
            "UPDATE admin_accounts SET totp_secret = ?2, totp_enabled = 0, updated_at = ?3 WHERE id = ?1",
            params![row.id, secret, crate::read::now_ms()],
        )?;
        Ok(secret)
    }

    /// 首码确认激活。之后所有高于 Read 的操作都要求登录时带码。
    pub fn totp_confirm(&self, name: &str, code: &str) -> Result<()> {
        let row = self
            .row_by_name(name)?
            .ok_or_else(|| AdminError::bad_input("没有这个管理员账号"))?;
        let secret = row
            .totp_secret
            .ok_or_else(|| AdminError::bad_input("还没生成 TOTP 密钥，先调 enroll"))?;
        if !wxlike_platform::totp_verify(&secret, code) {
            return Err(AdminError::bad_input(
                "动态码不对（时钟偏差 30 秒内可接受）",
            ));
        }
        self.lock().execute(
            "UPDATE admin_accounts SET totp_enabled = 1, updated_at = ?2 WHERE id = ?1",
            params![row.id, crate::read::now_ms()],
        )?;
        Ok(())
    }

    /// 只验码、不改状态。G14-01 的步进验证（step-up）用它：
    /// 敏感操作前要求"当场再验一次码"，验完只延长一个很短的窗口，
    /// 不给会话本身加永久权限。
    pub fn verify_second_factor(&self, name: &str, code: &str) -> Result<()> {
        let row = self
            .row_by_name(name)?
            .ok_or_else(|| AdminError::bad_input("没有这个管理员账号"))?;
        if !row.totp_enabled {
            return Err(AdminError::Forbidden(
                "这个账号没开 TOTP：敏感操作要求二次验证，先 POST /api/me/totp/enroll 再 confirm"
                    .into(),
            ));
        }
        let secret = row
            .totp_secret
            .ok_or_else(|| AdminError::bad_input("TOTP 已启用但密钥缺失，重新 enroll"))?;
        if !wxlike_platform::totp_verify(&secret, code) {
            return Err(AdminError::Forbidden(
                "动态码不对（时钟偏差 30 秒内可接受）".into(),
            ));
        }
        Ok(())
    }

    /// 关闭 MFA 需要当前码。理由：关 MFA 等于降低防护，未验码的会话发起它
    /// 就是现成的降级路径（"手机丢了 → 用还没验码的会话把 MFA 关掉"）。
    pub fn totp_disable(&self, name: &str, code: &str) -> Result<()> {
        let row = self
            .row_by_name(name)?
            .ok_or_else(|| AdminError::bad_input("没有这个管理员账号"))?;
        let secret = row
            .totp_secret
            .ok_or(AdminError::bad_input("这个账号没有绑定 TOTP"))?;
        if !wxlike_platform::totp_verify(&secret, code) {
            return Err(AdminError::Forbidden("动态码不对".into()));
        }
        self.lock().execute(
            "UPDATE admin_accounts SET totp_enabled = 0, totp_secret = NULL, updated_at = ?2 WHERE id = ?1",
            params![row.id, crate::read::now_ms()],
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 登录与会话
    // -----------------------------------------------------------------------

    /// 登录。**只有验过码的会话才 `mfa=true`**；没开 MFA 的账号能登录看数据，
    /// 但拿到的会话写不了任何东西（D3 的验收面）。
    pub fn login(
        &self,
        name: &str,
        password: &str,
        totp: Option<&str>,
        ttl_ms: u64,
        ip: &str,
    ) -> Result<(Session, String)> {
        let Some(row) = self.row_by_name(name)? else {
            // 账号不存在与口令错给同一个响应：否则可以枚举账号名。
            return Err(AdminError::Unauthorized);
        };
        if row.disabled {
            return Err(AdminError::Forbidden(format!("账号 {} 已停用", row.name)));
        }
        if !wxlike_platform::verify_argon2id(password, &row.pw_hash) {
            return Err(AdminError::Unauthorized);
        }
        let role = row.role()?;
        let mfa = if row.totp_enabled {
            let Some(code) = totp.map(str::trim).filter(|c| !c.is_empty()) else {
                return Err(AdminError::MfaRequired(format!(
                    "账号 {} 已开 TOTP，登录必须带 6 位动态码",
                    row.name
                )));
            };
            let secret = row.totp_secret.ok_or_else(|| {
                AdminError::bad_input(format!("账号 {} 开了 TOTP 但没有密钥", row.name))
            })?;
            if !wxlike_platform::totp_verify(&secret, code) {
                return Err(AdminError::MfaRequired("动态码不对".into()));
            }
            true
        } else {
            // 没开 MFA 也接受登录，但会话没有写权限。多余的码忽略：
            // 一个还没绑码的账号不该因为多填字段而登不进去。
            false
        };
        let now = crate::read::now_ms();
        let token = wxlike_platform::random_hex(32);
        self.lock().execute(
            "INSERT INTO admin_sessions(token, account_id, created_at, expires_at, mfa, ip) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                token,
                row.id,
                now,
                now.saturating_add(ttl_ms as i64),
                mfa as i64,
                ip
            ],
        )?;
        Ok((
            Session {
                account_id: row.id.clone(),
                actor: row.name.clone(),
                role,
                mfa,
                expires_at: now + ttl_ms as i64,
            },
            token,
        ))
    }

    /// 会话校验：过期就删行并返回 `None`（与平台 `lookup_token` 同一做法）。
    /// 账号被停用同样立刻失效——停用必须当场生效，不能等令牌自然过期。
    pub fn session(&self, token: &str) -> Result<Option<Session>> {
        let now = crate::read::now_ms();
        let found = {
            let c = self.lock();
            let mut stmt = c.prepare(
                "SELECT s.account_id, s.expires_at, s.mfa, a.name, a.role, a.disabled \
                 FROM admin_sessions s JOIN admin_accounts a ON a.id = s.account_id \
                 WHERE s.token = ?1",
            )?;
            let mut rows = stmt.query_map(params![token], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)? != 0,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)? != 0,
                ))
            })?;
            rows.next().transpose()?
        };
        let Some((account_id, expires_at, mfa, actor, role, disabled)) = found else {
            return Ok(None);
        };
        if expires_at <= now || disabled {
            self.lock().execute(
                "DELETE FROM admin_sessions WHERE token = ?1",
                params![token],
            )?;
            return Ok(None);
        }
        let role = Role::parse(&role).ok_or_else(|| {
            AdminError::bad_input(format!(
                "账号 {account_id} 的角色 {role:?} 无法识别（可选 viewer|moderator|superadmin）"
            ))
        })?;
        Ok(Some(Session {
            account_id,
            actor,
            role,
            mfa,
            expires_at,
        }))
    }

    pub fn logout(&self, token: &str) -> Result<()> {
        self.lock().execute(
            "DELETE FROM admin_sessions WHERE token = ?1",
            params![token],
        )?;
        Ok(())
    }

    /// 吊销某账号全部会话（停口令/停用时内部使用，也供运维手工调）。
    pub fn revoke_sessions(&self, account_id: &str) -> Result<usize> {
        Ok(self.lock().execute(
            "DELETE FROM admin_sessions WHERE account_id = ?1",
            params![account_id],
        )?)
    }

    // -----------------------------------------------------------------------
    // 操作留痕
    // -----------------------------------------------------------------------

    pub fn log_op(&self, rec: &OpRecord) -> Result<()> {
        if rec.detail.chars().count() > DETAIL_MAX_CHARS {
            return Err(AdminError::bad_input(format!(
                "留痕内容超过 {DETAIL_MAX_CHARS} 字符"
            )));
        }
        self.lock().execute(
            "INSERT INTO admin_oplog(at_ms, account_id, actor, op, capability, target, upstream, status, detail, mfa) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                crate::read::now_ms(),
                rec.account_id,
                rec.actor,
                rec.op,
                rec.capability,
                rec.target,
                rec.upstream,
                rec.status.as_str(),
                rec.detail,
                rec.mfa as i64
            ],
        )?;
        Ok(())
    }

    pub fn ops(&self, q: &OpLogQuery) -> Result<crate::read::Paged<OpLogRow>> {
        let limit = q
            .limit
            .unwrap_or(crate::read::DEFAULT_PAGE)
            .clamp(1, crate::read::MAX_PAGE) as i64;
        let offset = q.offset.unwrap_or(0).max(0);
        let mut args: Vec<Arg> = Vec::new();
        let mut where_sql = String::new();
        let mut next = 1usize;
        for (col, v) in [
            ("actor =", clean(q.actor.clone())),
            ("op =", clean(q.op.clone())),
            ("status =", clean(q.status.clone())),
        ] {
            if let Some(val) = v {
                push_cond(&mut where_sql, next, col);
                args.push(Arg::Text(val));
                next += 1;
            }
        }
        if let Some(from) = q.from.filter(|v| *v >= 0) {
            push_cond(&mut where_sql, next, "at_ms >=");
            args.push(Arg::Int(from));
            next += 1;
        }
        let c = self.lock();
        let total: i64 = {
            let mut stmt = c.prepare(&format!("SELECT COUNT(*) FROM admin_oplog{where_sql}"))?;
            let refs: Vec<&dyn ToSql> = args.iter().map(Arg::r#dyn).collect();
            stmt.query_row(refs.as_slice(), |r| r.get(0))?
        };
        let sql = format!(
            "SELECT at_ms, actor, op, capability, target, upstream, status, detail, mfa \
             FROM admin_oplog{where_sql} ORDER BY id DESC LIMIT ?{next} OFFSET ?{}",
            next + 1
        );
        let mut stmt = c.prepare(&sql)?;
        let mut refs: Vec<&dyn ToSql> = args.iter().map(Arg::r#dyn).collect();
        refs.push(&limit);
        refs.push(&offset);
        let rows = stmt.query_map(refs.as_slice(), |r| {
            Ok(OpLogRow {
                at_ms: r.get(0)?,
                actor: r.get(1)?,
                op: r.get(2)?,
                capability: r.get(3)?,
                target: r.get(4)?,
                upstream: r.get(5)?,
                status: r.get(6)?,
                detail: r.get(7)?,
                mfa: r.get::<_, i64>(8)? != 0,
            })
        })?;
        Ok(crate::read::Paged {
            total,
            limit,
            offset,
            rows: rows.collect::<std::result::Result<Vec<_>, _>>()?,
        })
    }
}

/// 动态绑定的槽（拥有数据）。
enum Arg {
    Text(String),
    Int(i64),
}

impl Arg {
    fn r#dyn(&self) -> &dyn ToSql {
        // &String / &i64 都能 coerce 成 &dyn ToSql；写成 s.as_str() 反而是
        // 把 &str（unsized）往 &dyn 上转，编不过。
        match self {
            Arg::Text(s) => s,
            Arg::Int(i) => i,
        }
    }
}

fn push_cond(sql: &mut String, index: usize, fragment: &str) {
    sql.push_str(if index == 1 { " WHERE" } else { " AND" });
    sql.push_str(&format!(" {fragment} ?{index}"));
}

fn clean(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[derive(Clone, Debug, Serialize)]
pub struct OpLogRow {
    pub at_ms: i64,
    pub actor: String,
    pub op: String,
    pub capability: String,
    pub target: String,
    pub upstream: String,
    pub status: String,
    pub detail: String,
    pub mfa: bool,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct OpLogQuery {
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub from: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// 处置动作的解析。`wxlike_report::Action` 是**内部标签**枚举
/// （`tag = "action"`），所以平铺写法 `{"action":"mute","duration_ms":60000}`
/// 就是它的原生形状；另外也接 `{"action":{"action":"ban"}}` 这种嵌套写法。
/// 两种都**不许猜默认值**：缺 `duration_ms` 的 mute 会被 serde 直接拒。
pub fn parse_action(body: &serde_json::Value) -> Result<Action> {
    const HELP: &str = "可选 warn / mute{\"duration_ms\":N} / ban / remove_content / none";
    match serde_json::from_value::<Action>(body.clone()) {
        Ok(action) => Ok(action),
        Err(flat_err) => match body.get("action") {
            Some(inner) => serde_json::from_value::<Action>(inner.clone()).map_err(|nested_err| {
                AdminError::bad_input(format!(
                    "action 不合法: flat={flat_err} nested={nested_err}（{HELP}）"
                ))
            }),
            None => Err(AdminError::bad_input(format!(
                "缺少 action: {flat_err}（{HELP}）"
            ))),
        },
    }
}

/// 备注（工单流转的依据）。空备注一律拒。
pub fn parse_note(body: &serde_json::Value) -> Result<String> {
    let note = body
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if note.is_empty() {
        return Err(AdminError::bad_input("note 必填：处置结论要能说清依据"));
    }
    if note.chars().count() > 512 {
        return Err(AdminError::bad_input("note 超过 512 字符"));
    }
    Ok(note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmp::TmpDir;

    /// 生成当前动态码。参数必须与平台 `totp_verify` 完全一致
    /// （SHA1 / 6 位 / skew 1 / 30 秒），否则测出来的"通过"是假通过。
    fn current_code(secret_b32: &str) -> String {
        let totp = totp_rs::TOTP::new(
            totp_rs::Algorithm::SHA1,
            6,
            1,
            30,
            totp_rs::Secret::Encoded(secret_b32.to_string())
                .to_bytes()
                .unwrap(),
        )
        .unwrap();
        totp.generate_current().unwrap()
    }

    fn store(tag: &str) -> (TmpDir, AccountStore) {
        let dir = TmpDir::create(tag);
        let s = AccountStore::open(&dir.join("adminx.db")).unwrap();
        (dir, s)
    }

    #[test]
    fn name_rules_block_header_injection_shapes() {
        assert!(AccountStore::check_name("ops-alice").is_ok());
        assert!(AccountStore::check_name("ops\r\nX-Evil: 1").is_err());
        assert!(AccountStore::check_name("ops alice").is_err());
        assert!(AccountStore::check_name("运维").is_err());
        assert!(AccountStore::check_name("").is_err());
        assert!(AccountStore::check_name(" lead").is_err());
        assert!(AccountStore::check_name(&"a".repeat(33)).is_err());
        assert!(AccountStore::check_password("short").is_err());
        assert!(AccountStore::check_password("a-fairly-long-pass").is_ok());
    }

    #[test]
    fn login_without_mfa_works_but_grants_no_write_power() {
        let (_dir, s) = store("login-basic");
        let acc = s
            .add("ops1", "a-fairly-long-pass", Role::Moderator)
            .unwrap();
        assert_eq!(acc.role, Role::Moderator);
        assert!(!acc.mfa_enabled);
        assert!(s.account("nope").unwrap().is_none());
        assert!(s.has_any_account().unwrap());

        let (sess, token) = s
            .login("ops1", "a-fairly-long-pass", None, 60_000, "")
            .unwrap();
        assert!(!sess.mfa, "没绑 MFA 的会话 mfa 必须为 false");
        assert_eq!(sess.role, Role::Moderator);
        assert_eq!(s.session(&token).unwrap().unwrap().account_id, acc.id);
        // 错口令与不存在的账号给出同一个错误（不能被枚举）
        assert!(matches!(
            s.login("ops1", "wrong-password-here", None, 60_000, "")
                .unwrap_err(),
            AdminError::Unauthorized
        ));
        assert!(matches!(
            s.login("ghost", "whatever-pass-here", None, 60_000, "")
                .unwrap_err(),
            AdminError::Unauthorized
        ));
        s.logout(&token).unwrap();
        assert!(s.session(&token).unwrap().is_none());
    }

    #[test]
    fn expiry_and_disable_revokes_sessions_immediately() {
        let (_dir, s) = store("session-expiry");
        s.add("ops2", "a-fairly-long-pass", Role::Viewer).unwrap();
        let (_, token) = s.login("ops2", "a-fairly-long-pass", None, 0, "").unwrap();
        assert!(s.session(&token).unwrap().is_none(), "ttl=0 就是立刻过期");
        let (_, token2) = s
            .login("ops2", "a-fairly-long-pass", None, 60_000, "")
            .unwrap();
        s.set_disabled("ops2", true).unwrap();
        assert!(
            s.session(&token2).unwrap().is_none(),
            "停用后已发出的会话必须当场失效"
        );
        assert!(s
            .login("ops2", "a-fairly-long-pass", None, 60_000, "")
            .is_err());
        s.set_disabled("ops2", false).unwrap();
        assert!(s
            .login("ops2", "a-fairly-long-pass", None, 60_000, "")
            .is_ok());
    }

    #[test]
    fn totp_gate_requires_a_real_code_and_marks_the_session() {
        let (_dir, s) = store("totp");
        s.add("ops3", "a-fairly-long-pass", Role::Superadmin)
            .unwrap();
        let secret = s.totp_enroll("ops3").unwrap();
        // 没确认前不算开了 MFA
        assert!(!s.account("ops3").unwrap().unwrap().mfa_enabled);
        assert!(s.account("ops3").unwrap().unwrap().totp_provisioned);
        assert!(s.totp_confirm("ops3", "000000").is_err());
        let code = current_code(&secret);
        s.totp_confirm("ops3", &code).unwrap();
        assert!(s.account("ops3").unwrap().unwrap().mfa_enabled);

        // 激活后：缺码 = 明确告诉调用方要码；错码 = 拒；对码 = 有写权限的会话
        let err = s
            .login("ops3", "a-fairly-long-pass", None, 60_000, "")
            .unwrap_err();
        assert!(matches!(err, AdminError::MfaRequired(_)), "{err}");
        assert!(err.to_string().contains("动态码"), "{err}");
        assert!(s
            .login("ops3", "a-fairly-long-pass", Some("999999"), 60_000, "")
            .is_err());
        let (sess, _) = s
            .login("ops3", "a-fairly-long-pass", Some(&code), 60_000, "")
            .unwrap();
        assert!(sess.mfa, "验过码的会话才能写");
        // 关 MFA 必须用当前码；未验码的降级路径要堵死
        assert!(s.totp_disable("ops3", "999999").is_err());
        s.totp_disable("ops3", &code).unwrap();
        assert!(!s.account("ops3").unwrap().unwrap().mfa_enabled);
    }

    #[test]
    fn password_change_revokes_sessions_and_keeps_the_account() {
        let (_dir, s) = store("passwd");
        s.add("ops4", "a-fairly-long-pass", Role::Viewer).unwrap();
        let (_, token) = s
            .login("ops4", "a-fairly-long-pass", None, 60_000, "")
            .unwrap();
        s.set_password("ops4", "another-fairly-long-pass").unwrap();
        assert!(
            s.session(&token).unwrap().is_none(),
            "改口令必须下线全部会话"
        );
        assert!(s
            .login("ops4", "a-fairly-long-pass", None, 60_000, "")
            .is_err());
        assert!(s
            .login("ops4", "another-fairly-long-pass", None, 60_000, "")
            .is_ok());
    }

    #[test]
    fn duplicate_name_is_conflict_not_silent_overwrite() {
        let (_dir, s) = store("dup");
        s.add("ops5", "a-fairly-long-pass", Role::Viewer).unwrap();
        let err = s
            .add("ops5", "another-fairly-long-pass", Role::Superadmin)
            .unwrap_err();
        assert!(matches!(err, AdminError::AccountExists), "{err:?}");
        assert_eq!(s.list().unwrap().len(), 1);
        // 原来的角色没有被覆盖成 superadmin
        assert_eq!(s.list().unwrap()[0].role, Role::Viewer);
    }

    #[test]
    fn unknown_role_is_an_error_not_silent_viewer() {
        let (_dir, s) = store("bad-role");
        s.add("ops6", "a-fairly-long-pass", Role::Viewer).unwrap();
        s.lock()
            .execute("UPDATE admin_accounts SET role = 'root'", [])
            .unwrap();
        let err = s.list().unwrap_err();
        assert!(err.to_string().contains("root"), "{err}");
        assert!(s.account("ops6").is_err());
        // 登录也过不去：认不出的角色既不能当 viewer，也不能当 superadmin
        assert!(s
            .login("ops6", "a-fairly-long-pass", None, 60_000, "")
            .is_err());
    }

    #[test]
    fn oplog_keeps_denied_attempts_and_filters() {
        let (_dir, s) = store("oplog");
        let acc = s.add("ops7", "a-fairly-long-pass", Role::Viewer).unwrap();
        s.log_op(&OpRecord {
            account_id: acc.id.clone(),
            actor: acc.name.clone(),
            op: "report.act".into(),
            capability: "destructive".into(),
            target: "abc".into(),
            upstream: "".into(),
            status: OpStatus::Denied,
            detail: "not permitted".into(),
            mfa: false,
        })
        .unwrap();
        s.log_op(&OpRecord {
            account_id: acc.id.clone(),
            actor: acc.name.clone(),
            op: "report.triage".into(),
            capability: "write".into(),
            target: "abc".into(),
            upstream: "POST /api/admin/reports/abc/triage".into(),
            status: OpStatus::Ok,
            detail: "{\"state\":\"triaged\"}".into(),
            mfa: true,
        })
        .unwrap();
        let all = s.ops(&OpLogQuery::default()).unwrap();
        assert_eq!(all.total, 2);
        assert_eq!(all.rows[0].op, "report.triage", "新的在前");
        let denied = s
            .ops(&OpLogQuery {
                status: Some("denied".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(denied.total, 1);
        assert_eq!(denied.rows[0].op, "report.act");
        let combo = s
            .ops(&OpLogQuery {
                actor: Some("ops7".into()),
                op: Some("report.triage".into()),
                from: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(combo.total, 1);
        assert!(combo.rows[0].mfa);
        assert_eq!(
            s.ops(&OpLogQuery {
                actor: Some("ghost".into()),
                ..Default::default()
            })
            .unwrap()
            .total,
            0
        );
    }

    #[test]
    fn oplog_rejects_absurdly_long_detail() {
        let (_dir, s) = store("oplog-detail");
        let acc = s.add("ops8", "a-fairly-long-pass", Role::Viewer).unwrap();
        let err = s
            .log_op(&OpRecord {
                account_id: acc.id.clone(),
                actor: acc.name.clone(),
                op: "x".into(),
                capability: "read".into(),
                target: "".into(),
                upstream: "".into(),
                status: OpStatus::Ok,
                detail: "y".repeat(DETAIL_MAX_CHARS + 1),
                mfa: false,
            })
            .unwrap_err();
        assert!(err.to_string().contains("2048"), "{err}");
        assert_eq!(s.ops(&OpLogQuery::default()).unwrap().total, 0);
    }

    #[test]
    fn action_parser_accepts_both_shapes_and_rejects_garbage() {
        assert_eq!(
            parse_action(&serde_json::json!({"action": "warn"})).unwrap(),
            Action::Warn
        );
        assert_eq!(
            parse_action(&serde_json::json!({"action": {"action": "ban"}})).unwrap(),
            Action::Ban
        );
        assert_eq!(
            parse_action(&serde_json::json!({"action": "mute", "duration_ms": 60_000})).unwrap(),
            Action::Mute {
                duration_ms: 60_000
            }
        );
        // 平铺的 mute 缺时长：不许猜
        assert!(parse_action(&serde_json::json!({"action": "mute"})).is_err());
        assert!(parse_action(&serde_json::json!({"note": "只有备注"})).is_err());
        assert!(parse_action(&serde_json::json!({"action": "shoot"})).is_err());
        assert_eq!(
            parse_note(&serde_json::json!({"note": "  有依据 "})).unwrap(),
            "有依据"
        );
        assert!(parse_note(&serde_json::json!({"note": "   "})).is_err());
        assert!(parse_note(&serde_json::json!({})).is_err());
    }
}
