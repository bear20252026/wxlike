//! 平台库的**只读**访问面（D1 的立身之本）。
//!
//! 三条硬约束，都能在测试里验证：
//!
//! 1. 连接用 `SQLITE_OPEN_READ_ONLY` 打开，并且**再叠一层** `PRAGMA query_only=ON`。
//!    "我们没写 INSERT 语句"是约定，pragma 是数据库替你挡住的——后台进程永远
//!    不该成为平台库的第二个写者。
//! 2. 不跑迁移。adminx 不创建表、不 `ALTER`、不加索引（理由见 `docs/adminx.md`
//!    的"schema 属于串行轨道"）。表缺了就在 `/health` 里点名并让统计报错，
//!    而不是静默返回一个空看板。
//! 3. 不查无界全表。看板每个数字都注明代价；审计这种无界表只按主键取窗口。
//!
//! WAL 的边角：以只读方式打开一个 WAL 库时，如果 `-shm` 还不存在（服务刚好
//! 干净退出过），SQLite 会拒绝打开。所以有一条**回退路径**：改用读写标志打开，
//! 但立刻 `query_only=ON` —— 写仍然被数据库拒绝，可读性却不再依赖 `-shm`
//! 存在的时机。两条路径的"写必失败"各有用例：
//! `strict_read_only_path_blocks_writes` 与 `fallback_path_also_blocks_writes`。

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::{params, Connection, OpenFlags, Row, ToSql};
use serde::{Deserialize, Serialize};
use wxlike_report::{Action, Reason, State as TicketState, Ticket, Transition};

use crate::error::{AdminError, Result};

/// 看板上"最近活动"的采样窗口：审计表无界，只按主键倒序取这么多行来统计。
pub const AUDIT_SAMPLE_ROWS: i64 = 1_000;

/// 单页上限。面板是给人看的，不是导数据管道（导出另有规划，见 docs/adminx.md）。
pub const MAX_PAGE: usize = 200;
pub const DEFAULT_PAGE: usize = 50;

/// 一个筛选值的最长可接受长度。actor / action / target 都是系统生成的短串，
/// 超长一定是误用；拒绝它比给它建索引便宜。
pub const FILTER_MAX_CHARS: usize = 128;

/// `/health` 与统计都依赖的表。缺任何一张 = 这个库不是当前版本产出的。
pub const REQUIRED_TABLES: [&str; 12] = [
    "users",
    "sessions",
    "groups",
    "group_members",
    "friends",
    "audit_log",
    "reports",
    "moderation_queue",
    "wallets",
    "reconcile_runs",
    "communities",
    "owner_seq",
];

pub fn now_ms() -> i64 {
    wxlike_core::types::now_ms() as i64
}

pub fn page_limit(limit: Option<usize>) -> i64 {
    limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE) as i64
}

fn clean(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// 动态绑定用的参数槽（拥有数据，查询时再借出去）。
enum Bind {
    Text(String),
    Int(i64),
}

/// WHERE 片段的产物：形状 + 值 + 下一个可用占位符编号。
///
/// 拼进 SQL 的永远只有 `AND col = ?n` 这种**形状**，值一律走绑定
/// （本仓库的硬规矩：字符串拼 SQL 视为违规）。
struct Clause {
    sql: String,
    binds: Vec<Bind>,
    next: usize,
}

impl Clause {
    fn new() -> Self {
        Self {
            sql: String::new(),
            binds: Vec::new(),
            next: 1,
        }
    }

    fn add(&mut self, fragment: &str, bind: Bind) {
        self.sql.push_str(if self.binds.is_empty() {
            " WHERE"
        } else {
            " AND"
        });
        self.sql.push_str(fragment);
        self.sql.push_str(&self.next.to_string());
        self.binds.push(bind);
        self.next += 1;
    }

    /// 追加 `order` + `LIMIT ?n OFFSET ?n+1`，并把两个数排进绑定表。
    /// 返回完整 SQL；调用方必须先算 COUNT（那时 `self.sql` 还是纯 WHERE 片段）。
    fn finish(&mut self, order: &str, limit: i64, offset: i64) -> String {
        let mut sql = std::mem::take(&mut self.sql);
        sql.push_str(order);
        sql.push_str(" LIMIT ?");
        sql.push_str(&self.next.to_string());
        sql.push_str(" OFFSET ?");
        sql.push_str(&(self.next + 1).to_string());
        self.sql = sql.clone();
        self.binds.push(Bind::Int(limit));
        self.binds.push(Bind::Int(offset));
        sql
    }

    fn refs(&self) -> Vec<&dyn ToSql> {
        self.binds
            .iter()
            .map(|b| match b {
                Bind::Text(s) => s as &dyn ToSql,
                Bind::Int(i) => i as &dyn ToSql,
            })
            .collect()
    }
}

/// 一个 `Mutex<Connection>`，形状与平台层的 `Db` 一致（同一把锁的纪律）。
#[derive(Clone)]
pub struct PlatformRead {
    inner: Arc<Mutex<Connection>>,
    /// `read_only` = 严格只读；`query_only` = 读写标志 + pragma 兜底。
    /// `/health` 要把它透出去：运维需要知道这个进程到底是以哪种方式碰库的。
    open_mode: &'static str,
}

impl PlatformRead {
    /// 只读打开平台库。文件不存在直接失败——创建空库会得到一个"全 0 的看板"，
    /// 那是本项目明令禁止的静默降级。
    pub fn open(path: &Path) -> Result<Self> {
        match Self::open_with(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(read) => Ok(read),
            Err(strict_err) => {
                let fallback = Self::open_with(path, OpenFlags::SQLITE_OPEN_READ_WRITE);
                match fallback {
                    Ok(read) => {
                        tracing::warn!(
                            "严格只读打开失败（{strict_err}），改用 query_only 回退：\
                             该连接仍然写不进平台库"
                        );
                        Ok(read.with_mode("query_only"))
                    }
                    Err(err) => {
                        tracing::error!("只读打开失败: {strict_err}；query_only 回退也失败: {err}");
                        Err(AdminError::from(err))
                    }
                }
            }
        }
    }

    fn open_with(path: &Path, flags: OpenFlags) -> rusqlite::Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            flags | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        // 平台在写；读撞上 WAL 检查点要等一会儿，而不是立刻 500。
        conn.pragma_update(None, "busy_timeout", 5_000)?;
        conn.pragma_update(None, "query_only", "ON")?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
            open_mode: if flags.contains(OpenFlags::SQLITE_OPEN_READ_ONLY) {
                "read_only"
            } else {
                "query_only"
            },
        })
    }

    fn with_mode(mut self, mode: &'static str) -> Self {
        self.open_mode = mode;
        self
    }

    /// 锁中毒只可能发生在 SQL panic 中途，连接本身还活着：恢复守卫比崩掉好。
    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn open_mode(&self) -> &'static str {
        self.open_mode
    }

    /// 缺哪些必需表。返回空 = 库形状符合预期。
    pub fn missing_tables(&self) -> Result<Vec<String>> {
        let c = self.lock();
        let mut missing = Vec::new();
        for table in REQUIRED_TABLES {
            let one: i64 = c.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |r| r.get(0),
            )?;
            if one == 0 {
                missing.push(table.to_string());
            }
        }
        Ok(missing)
    }

    // -----------------------------------------------------------------------
    // 看板
    // -----------------------------------------------------------------------

    pub fn stats(&self) -> Result<Stats> {
        let now = now_ms();
        let day = 24 * 60 * 60 * 1000;
        let c = self.lock();
        let scalar = |sql: &str| -> Result<i64> { Ok(c.query_row(sql, [], |r| r.get(0))?) };
        let scalar1 =
            |sql: &str, v: i64| -> Result<i64> { Ok(c.query_row(sql, params![v], |r| r.get(0))?) };
        let users_total = scalar("SELECT COUNT(*) FROM users")?;
        let users_created_24h = scalar1(
            "SELECT COUNT(*) FROM users WHERE created_at >= ?1",
            now - day,
        )?;
        let active_sessions = scalar1("SELECT COUNT(*) FROM sessions WHERE expires_at > ?1", now)?;
        let groups_total = scalar("SELECT COUNT(*) FROM groups")?;
        let communities_total = scalar("SELECT COUNT(*) FROM communities")?;
        // owner_seq 每个会话一行：这两个数与该表行数成正比，与信箱大小无关。
        let conversations = scalar("SELECT COUNT(*) FROM owner_seq")?;
        let message_seqs = scalar("SELECT COALESCE(SUM(seq), 0) FROM owner_seq")?;
        let reports_open =
            scalar("SELECT COUNT(*) FROM reports WHERE state IN ('open','triaged','actioned')")?;
        let reports_over_24h = c.query_row(
            "SELECT COUNT(*) FROM reports \
             WHERE state IN ('open','triaged','actioned') AND created_at < ?1",
            params![now - day],
            |r| r.get(0),
        )?;
        let moderation_held = scalar("SELECT COUNT(*) FROM moderation_queue")?;
        let friend_requests_pending =
            scalar("SELECT COUNT(*) FROM friend_requests WHERE status = 'pending'")?;
        let wallet_sum = scalar("SELECT COALESCE(SUM(balance), 0) FROM wallets")?;
        // 无界表：先按主键倒序取窗口，再在窗口里数——绝不全表扫。
        let audit_in_sample_24h = c.query_row(
            "SELECT COUNT(*) FROM (SELECT at_ms FROM audit_log ORDER BY id DESC LIMIT ?1) \
             WHERE at_ms >= ?2",
            params![AUDIT_SAMPLE_ROWS, now - day],
            |r| r.get(0),
        )?;
        let last_reconcile = {
            let row = c.query_row(
                "SELECT at_ms, users_checked, mismatches, ok \
                 FROM reconcile_runs ORDER BY id DESC LIMIT 1",
                [],
                |r| {
                    Ok(ReconcileRow {
                        at_ms: r.get(0)?,
                        users_checked: r.get(1)?,
                        mismatches: r.get(2)?,
                        ok: r.get::<_, i64>(3)? != 0,
                    })
                },
            );
            match row {
                Ok(v) => Some(v),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(err) => return Err(AdminError::from(err)),
            }
        };
        Ok(Stats {
            at_ms: now,
            users_total,
            users_created_24h,
            active_sessions,
            groups_total,
            communities_total,
            conversations,
            message_seqs,
            reports_open,
            // 看板上的"超时"用固定的 24 h 口径；工单面按配置的 SLA 算 overdue。
            // 两处口径不同是刻意的（一个是趋势、一个是待办），docs 里写清楚。
            reports_over_24h,
            moderation_held,
            friend_requests_pending,
            wallet_sum_minor: wallet_sum,
            audit_sample_rows: AUDIT_SAMPLE_ROWS,
            audit_in_sample_24h,
            last_reconcile,
        })
    }

    // -----------------------------------------------------------------------
    // 用户
    // -----------------------------------------------------------------------

    /// `?1 limit · ?2 offset · ?3 活跃判定 · ?4 名字前缀（仅带 q 时）`
    pub fn users(&self, filter: &ListFilter) -> Result<Paged<UserRow>> {
        let now = now_ms();
        let c = self.lock();
        let mut sql = String::from(USERS_HEAD);
        let mut pattern: Option<String> = None;
        if let Some(q) = filter.q.as_deref() {
            pattern = Some(format!("{}%", escape_like(q)));
            sql.push_str(" WHERE name LIKE ?4 ESCAPE '\\'");
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT ?1 OFFSET ?2");

        let total: i64 = match &pattern {
            Some(p) => c.query_row(
                "SELECT COUNT(*) FROM users WHERE name LIKE ?1 ESCAPE '\\'",
                params![p],
                |r| r.get(0),
            )?,
            None => c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?,
        };
        let rows = {
            let mut stmt = c.prepare(&sql)?;
            let it = match &pattern {
                Some(p) => stmt.query_map(
                    params![filter.limit, filter.offset, now, p],
                    UserRow::from_row,
                )?,
                None => {
                    stmt.query_map(params![filter.limit, filter.offset, now], UserRow::from_row)?
                }
            };
            it.collect::<std::result::Result<Vec<_>, _>>()?
        };
        Ok(Paged {
            total,
            limit: filter.limit,
            offset: filter.offset,
            rows,
        })
    }

    // -----------------------------------------------------------------------
    // 群
    // -----------------------------------------------------------------------

    pub fn groups(&self, filter: &ListFilter) -> Result<Paged<GroupRow>> {
        let c = self.lock();
        let total: i64 = c.query_row("SELECT COUNT(*) FROM groups", [], |r| r.get(0))?;
        let mut stmt = c.prepare(GROUPS_SQL)?;
        let rows = stmt.query_map(params![filter.limit, filter.offset], GroupRow::from_row)?;
        Ok(Paged {
            total,
            limit: filter.limit,
            offset: filter.offset,
            rows: rows.collect::<std::result::Result<Vec<_>, _>>()?,
        })
    }

    // -----------------------------------------------------------------------
    // 审计检索（D2）
    // -----------------------------------------------------------------------

    /// 平台侧的命令审计约定（`audit_mw.rs`）：`action = "cmd.<命令 id>"`。
    /// 所以 `command=group.create` 必须能查到 `cmd.group.create`，
    /// 这一条就是"agent 执行过哪些写操作"的检索面。
    pub fn audit(&self, f: &AuditFilter) -> Result<Paged<AuditRow>> {
        let c = self.lock();
        let mut clause = f.clause();
        let count_sql = format!("SELECT COUNT(*) FROM audit_log{}", clause.sql);
        let total: i64 = {
            let refs = clause.refs();
            c.query_row(&count_sql, refs.as_slice(), |r| r.get(0))?
        };
        let tail = clause.finish(" ORDER BY id DESC", f.limit, f.offset);
        let sql = format!("{AUDIT_SQL_HEAD}{tail}");
        let mut stmt = c.prepare(&sql)?;
        let refs = clause.refs();
        let rows = stmt.query_map(refs.as_slice(), AuditRow::from_row)?;
        Ok(Paged {
            total,
            limit: f.limit,
            offset: f.offset,
            rows: rows.collect::<std::result::Result<Vec<_>, _>>()?,
        })
    }

    // -----------------------------------------------------------------------
    // 工单（D2）
    // -----------------------------------------------------------------------

    pub fn reports(&self, f: &ReportFilter) -> Result<Paged<TicketRow>> {
        let c = self.lock();
        let mut clause = f.clause();
        let total: i64 = {
            let refs = clause.refs();
            c.query_row(
                &format!("SELECT COUNT(*) FROM reports{}", clause.sql),
                refs.as_slice(),
                |r| r.get(0),
            )?
        };
        let sql = clause.finish(" ORDER BY created_at ASC", f.limit, f.offset);
        let sql = format!("{REPORTS_HEAD}{sql}");
        let loads = {
            let mut stmt = c.prepare(&sql)?;
            let refs = clause.refs();
            let rows = stmt.query_map(refs.as_slice(), ticket_load_from_row)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut rows = Vec::with_capacity(loads.len());
        for load in loads {
            let ticket = build_ticket(load)?;
            rows.push(TicketRow {
                id: ticket.id.clone(),
                ticket,
                overdue: false,
            });
        }
        Ok(Paged {
            total,
            limit: f.limit,
            offset: f.offset,
            rows,
        })
    }

    /// 单张工单（详情页 / 写操作后的回执核对）。
    pub fn report(&self, id: &str) -> Result<Option<Ticket>> {
        let c = self.lock();
        let load = {
            let mut stmt = c.prepare(&format!("{REPORTS_HEAD} WHERE id = ?1"))?;
            match stmt.query_row(params![id], ticket_load_from_row) {
                Ok(load) => load,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                Err(err) => return Err(AdminError::from(err)),
            }
        };
        Ok(Some(build_ticket(load)?))
    }

    // -----------------------------------------------------------------------
    // 审核挂起队列（只读面；处置动作属于 D3，走平台）
    // -----------------------------------------------------------------------

    pub fn moderation(&self, filter: &ListFilter) -> Result<Paged<HeldRow>> {
        let c = self.lock();
        let total: i64 = c.query_row("SELECT COUNT(*) FROM moderation_queue", [], |r| r.get(0))?;
        let mut stmt = c.prepare(
            "SELECT id, conv_id, sender, text, labels, decision, created_at \
             FROM moderation_queue ORDER BY created_at ASC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![filter.limit, filter.offset], |r| {
            Ok(HeldRow {
                id: r.get(0)?,
                conv_id: r.get(1)?,
                sender: r.get(2)?,
                text: r.get(3)?,
                // labels 是平台写的 JSON 数组；解不出来就当没有标签，
                // 因为这一列只影响展示，不影响任何判定。
                labels: serde_json::from_str::<Vec<String>>(&r.get::<_, String>(4)?)
                    .unwrap_or_default(),
                decision: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?;
        Ok(Paged {
            total,
            limit: filter.limit,
            offset: filter.offset,
            rows: rows.collect::<std::result::Result<Vec<_>, _>>()?,
        })
    }
}

const USERS_HEAD: &str = "SELECT id, name, avatar, created_at, \
     (SELECT COUNT(*) FROM friends f WHERE f.user_id = u.id), \
     (SELECT COALESCE(SUM(CASE WHEN kind IN ('transfer_out','redpacket_out') THEN -amount ELSE amount END), 0) \
        FROM ledger l WHERE l.user_id = u.id), \
     (SELECT balance FROM wallets w WHERE w.user_id = u.id), \
     (SELECT MAX(last_seen) FROM sessions s WHERE s.user_id = u.id AND s.expires_at > ?3) \
     FROM users u";

const GROUPS_SQL: &str =
    "SELECT g.id, g.name, g.owner_uid, g.created_at, g.approval, g.all_muted, \
     (SELECT COUNT(*) FROM group_members m WHERE m.group_id = g.id) \
     FROM groups g ORDER BY g.created_at DESC LIMIT ?1 OFFSET ?2";

const REPORTS_HEAD: &str = "SELECT id, reporter, target_kind, target_id, reason, detail, state, \
     action_json, handler, created_at, updated_at, history_json FROM reports";

const AUDIT_SQL_HEAD: &str = "SELECT id, at_ms, actor, action, target, detail FROM audit_log";

/// 转义 LIKE 的元字符：用户可以搜 `a%b` 这样的串，`%` 不能变成通配符。
fn escape_like(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 4);
    for ch in q.chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

#[derive(Clone, Debug, Serialize)]
pub struct Paged<T> {
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub rows: Vec<T>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReconcileRow {
    pub at_ms: i64,
    pub users_checked: i64,
    pub mismatches: i64,
    pub ok: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Stats {
    pub at_ms: i64,
    pub users_total: i64,
    pub users_created_24h: i64,
    pub active_sessions: i64,
    pub groups_total: i64,
    pub communities_total: i64,
    /// 有信箱的会话数（`owner_seq` 行数）。
    pub conversations: i64,
    /// 已分配序列号之和（各属主 head 相加）。精确的消息行数要全表扫信箱，
    /// 看板故意不做——见 `docs/adminx.md` 的性能口径。
    pub message_seqs: i64,
    pub reports_open: i64,
    pub reports_over_24h: i64,
    pub moderation_held: i64,
    pub friend_requests_pending: i64,
    /// 全平台余额，整数分（永不浮点）。
    pub wallet_sum_minor: i64,
    pub audit_sample_rows: i64,
    pub audit_in_sample_24h: i64,
    pub last_reconcile: Option<ReconcileRow>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UserRow {
    pub id: String,
    pub name: String,
    pub avatar: String,
    pub created_at: i64,
    pub friends: i64,
    /// 账本流水净额。与 `wallet_balance` 不等 = 这个用户被绕过账本改过余额，
    /// 对账页看总体，这里看单个。
    pub ledger_net: i64,
    pub wallet_balance: Option<i64>,
    pub last_seen: Option<i64>,
}

impl UserRow {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            name: r.get(1)?,
            avatar: r.get(2)?,
            created_at: r.get(3)?,
            friends: r.get(4)?,
            ledger_net: r.get(5)?,
            wallet_balance: r.get(6)?,
            last_seen: r.get(7)?,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct GroupRow {
    pub id: String,
    pub name: String,
    pub owner_uid: String,
    pub created_at: i64,
    pub approval: bool,
    pub all_muted: bool,
    pub members: i64,
}

impl GroupRow {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            name: r.get(1)?,
            owner_uid: r.get(2)?,
            created_at: r.get(3)?,
            approval: r.get::<_, i64>(4)? != 0,
            all_muted: r.get::<_, i64>(5)? != 0,
            members: r.get(6)?,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct AuditRow {
    pub id: i64,
    pub at_ms: i64,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub detail: String,
}

impl AuditRow {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            at_ms: r.get(1)?,
            actor: r.get(2)?,
            action: r.get(3)?,
            target: r.get(4)?,
            detail: r.get(5)?,
        })
    }
}

/// 审核挂起的一行。`decision` 原样透出（面板只展示，不重新解释平台的判定）。
#[derive(Clone, Debug, Serialize)]
pub struct HeldRow {
    pub id: String,
    pub conv_id: String,
    pub sender: String,
    pub text: String,
    pub labels: Vec<String>,
    pub decision: String,
    pub created_at: i64,
}

/// 工单视图。**状态用 `wxlike-report` 的真类型**，看板不自造一套词汇——
/// 否则会出现"面板显示 open、平台里其实是 triaged"这种最难查的偏差。
#[derive(Clone, Debug, Serialize)]
pub struct TicketRow {
    pub id: String,
    pub ticket: Ticket,
    /// 由 `GET /api/reports` 按配置 SLA 现算。
    pub overdue: bool,
}

/// `reports` 一行的读取形状（12 列，用具名结构而不是元组：可读 + 过 clippy）。
struct TicketLoad {
    id: String,
    reporter: String,
    target_kind: String,
    target_id: String,
    reason: String,
    detail: String,
    state: String,
    action_json: Option<String>,
    handler: Option<String>,
    created_at: i64,
    updated_at: i64,
    history_json: String,
}

fn ticket_load_from_row(r: &Row<'_>) -> rusqlite::Result<TicketLoad> {
    Ok(TicketLoad {
        id: r.get(0)?,
        reporter: r.get(1)?,
        target_kind: r.get(2)?,
        target_id: r.get(3)?,
        reason: r.get(4)?,
        detail: r.get(5)?,
        state: r.get(6)?,
        action_json: r.get(7)?,
        handler: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
        history_json: r.get(11)?,
    })
}

/// 库里的字符串必须是目前状态机认得的值；认不得就报错。
///
/// 为什么不降级成"未知状态原样显示"：工单状态决定按钮能不能点，
/// 一个错的状态会让运维在非法流转上浪费一整轮操作，还会让人以为平台坏了。
fn build_ticket(load: TicketLoad) -> Result<Ticket> {
    let TicketLoad {
        id,
        reporter,
        target_kind,
        target_id,
        reason,
        detail,
        state,
        action_json,
        handler,
        created_at,
        updated_at,
        history_json,
    } = load;
    let reason = Reason::parse(&reason).ok_or_else(|| {
        AdminError::bad_input(format!("工单 {id} 的 reason 无法识别: {reason:?}"))
    })?;
    let state = TicketState::parse(&state)
        .ok_or_else(|| AdminError::bad_input(format!("工单 {id} 的 state 无法识别: {state:?}")))?;
    let action: Option<Action> =
        match action_json.as_deref() {
            None | Some("") => None,
            Some(raw) => Some(serde_json::from_str(raw).map_err(|e| {
                AdminError::bad_input(format!("工单 {id} 的 action_json 损坏: {e}"))
            })?),
        };
    let history: Vec<Transition> = if history_json.is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&history_json)
            .map_err(|e| AdminError::bad_input(format!("工单 {id} 的 history 损坏: {e}")))?
    };
    Ok(Ticket {
        id,
        reporter,
        target_kind,
        target_id,
        reason,
        detail,
        state,
        created_at: created_at.max(0) as u64,
        updated_at: updated_at.max(0) as u64,
        action,
        handler,
        history,
    })
}

/// `?limit=&offset=&q=` 的统一形状。`q` 只对支持搜索的资源生效。
#[derive(Clone, Debug)]
pub struct ListFilter {
    pub q: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

impl ListFilter {
    pub fn new(q: Option<String>, limit: Option<usize>, offset: Option<i64>) -> Self {
        Self {
            q: clean(q),
            limit: page_limit(limit),
            offset: offset.unwrap_or(0).max(0),
        }
    }
}

/// 审计检索条件（D2 的验收面）。
#[derive(Clone, Debug, Default)]
pub struct AuditFilter {
    pub actor: Option<String>,
    /// 命令 id（`group.create`）→ `action = "cmd.group.create"`。
    pub command: Option<String>,
    /// 原始 action 精确匹配（`user.login` / `report.action` …）。
    pub action: Option<String>,
    pub target: Option<String>,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub limit: i64,
    pub offset: i64,
}

/// 审计 `action` 的命令前缀。唯一真相在平台的 `audit_mw.rs`，这里只是消费方。
pub const COMMAND_ACTION_PREFIX: &str = "cmd.";

impl AuditFilter {
    pub fn new(q: AuditQuery) -> Result<Self> {
        let f = Self {
            actor: clean(q.actor),
            command: clean(q.command),
            action: clean(q.action),
            target: clean(q.target),
            from_ms: q.from.filter(|v| *v >= 0),
            to_ms: q.to.filter(|v| *v >= 0),
            limit: page_limit(q.limit),
            offset: q.offset.unwrap_or(0).max(0),
        };
        for (name, v) in [
            ("actor", &f.actor),
            ("command", &f.command),
            ("action", &f.action),
            ("target", &f.target),
        ] {
            if let Some(s) = v {
                if s.chars().count() > FILTER_MAX_CHARS {
                    return Err(AdminError::bad_input(format!(
                        "参数 {name} 超过 {FILTER_MAX_CHARS} 字符（这些值都是系统生成的短串）"
                    )));
                }
            }
        }
        if let (Some(from), Some(to)) = (f.from_ms, f.to_ms) {
            if from > to {
                // 明确报错，而不是回一个空列表：区间写反是用法错误，静默空会让人
                // 以为"没有记录"，那是本项目禁止的降级。
                return Err(AdminError::bad_input(format!(
                    "from({from}) 晚于 to({to})：时间区间是反的"
                )));
            }
        }
        Ok(f)
    }

    fn clause(&self) -> Clause {
        let mut c = Clause::new();
        if let Some(actor) = &self.actor {
            c.add(" actor = ?", Bind::Text(actor.clone()));
        }
        if let Some(command) = &self.command {
            c.add(
                " action = ?",
                Bind::Text(format!("{COMMAND_ACTION_PREFIX}{command}")),
            );
        }
        if let Some(action) = &self.action {
            c.add(" action = ?", Bind::Text(action.clone()));
        }
        if let Some(target) = &self.target {
            c.add(" target = ?", Bind::Text(target.clone()));
        }
        if let Some(from) = self.from_ms {
            c.add(" at_ms >= ?", Bind::Int(from));
        }
        if let Some(to) = self.to_ms {
            c.add(" at_ms <= ?", Bind::Int(to));
        }
        c
    }
}

/// 工单过滤：状态、处理人、被举报对象、举报人、只看未终结。
#[derive(Clone, Debug, Default)]
pub struct ReportFilter {
    pub state: Option<String>,
    pub handler: Option<String>,
    pub target_id: Option<String>,
    pub reporter: Option<String>,
    pub open_only: bool,
    pub limit: i64,
    pub offset: i64,
}

pub const TICKET_STATES: [&str; 5] = ["open", "triaged", "actioned", "closed", "rejected"];
const OPEN_STATES: &str = " state IN ('open','triaged','actioned')";

impl ReportFilter {
    pub fn new(q: ReportQuery) -> Result<Self> {
        let state = clean(q.state);
        if let Some(s) = state.as_deref() {
            if !TICKET_STATES.contains(&s) {
                return Err(AdminError::bad_input(format!(
                    "未知工单状态 {s:?}，可选：{}",
                    TICKET_STATES.join(" | ")
                )));
            }
        }
        Ok(Self {
            state,
            handler: clean(q.handler),
            target_id: clean(q.target_id),
            reporter: clean(q.reporter),
            open_only: q.open_only.unwrap_or(false),
            limit: page_limit(q.limit),
            offset: q.offset.unwrap_or(0).max(0),
        })
    }

    fn clause(&self) -> Clause {
        let mut c = Clause::new();
        if let Some(s) = &self.state {
            c.add(" state = ?", Bind::Text(s.clone()));
        }
        if let Some(h) = &self.handler {
            c.add(" handler = ?", Bind::Text(h.clone()));
        }
        if let Some(t) = &self.target_id {
            c.add(" target_id = ?", Bind::Text(t.clone()));
        }
        if let Some(r) = &self.reporter {
            c.add(" reporter = ?", Bind::Text(r.clone()));
        }
        if self.open_only {
            c.sql
                .push_str(if c.binds.is_empty() { " WHERE" } else { " AND" });
            c.sql.push_str(OPEN_STATES);
        }
        c
    }
}

/// `GET /api/audit` 的查询参数。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AuditQuery {
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    /// epoch 毫秒（含）。
    #[serde(default)]
    pub from: Option<i64>,
    /// epoch 毫秒（含）。
    #[serde(default)]
    pub to: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// `GET /api/reports` 的查询参数。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ReportQuery {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub handler: Option<String>,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub reporter: Option<String>,
    #[serde(default)]
    pub open_only: Option<bool>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<i64>,
}

/// `GET /api/{users,groups,moderation}` 的查询参数。
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmp::TmpDir;
    use wxlike_platform::Platform;

    /// 建一个**真的**平台库（跑平台自己的迁移），再只读打开。全程没有 mock。
    /// `hold` = 保留平台实例（WAL 的 `-shm` 还在），否则干净关闭后再读。
    fn seeded_db(tag: &str, hold: bool) -> (TmpDir, PlatformRead, Option<Platform>) {
        let dir = TmpDir::create(tag);
        let path = dir.join("wxlike.db");
        let p = Platform::open(Some(path.to_str().unwrap())).unwrap();
        let alice = p.register("alice", "pw").unwrap();
        let bob = p.register("bob_x", "pw").unwrap();
        let bobby = p.register("bobby", "pw").unwrap();
        assert_ne!(bob, bobby);
        p.audit(
            &alice,
            "cmd.group.create",
            "/api/groups",
            "status=200 method=POST",
        );
        p.audit(&alice, "user.login", &alice, "ok");
        p.audit(
            &bob,
            "cmd.msg.send",
            "/api/conv/g-1/send",
            "status=200 method=POST",
        );
        let read = PlatformRead::open(&path).unwrap();
        if hold {
            (dir, read, Some(p))
        } else {
            drop(p);
            (dir, read, None)
        }
    }

    #[test]
    fn strict_read_only_path_blocks_writes() {
        // 平台实例还在 => -shm 存在 => 走严格只读
        let (_dir, read, p) = seeded_db("strict", true);
        assert!(p.is_some(), "持有平台实例才有严格的 -shm");
        assert_eq!(read.open_mode(), "read_only");
        let c = read.lock();
        let err = c
            .execute("UPDATE users SET name = 'pwned' WHERE name = 'alice'", [])
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("readonly")
                || err.to_string().to_lowercase().contains("read-only")
                || err.to_string().to_lowercase().contains("read only"),
            "必须被数据库挡住，实际: {err}"
        );
    }

    #[test]
    fn fallback_path_also_blocks_writes() {
        // 回退路径的等价性：读写标志 + query_only=ON 仍然写不进去。
        let dir = TmpDir::create("fallback-write");
        let path = dir.join("wxlike.db");
        drop(Platform::open(Some(path.to_str().unwrap())).unwrap());
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
        conn.pragma_update(None, "query_only", "ON").unwrap();
        let err = conn.execute("DELETE FROM audit_log", []).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("readonly")
                || err.to_string().to_lowercase().contains("read only"),
            "query_only 必须挡住写，实际: {err}"
        );
    }

    #[test]
    fn reopened_after_clean_close_is_still_unwritable() {
        // 平台干净退出后 -shm 被删，这一步可能走回退；无论哪条路径都不能写。
        let (_dir, read, _) = seeded_db("closed", false);
        assert!(
            matches!(read.open_mode(), "read_only" | "query_only"),
            "打开方式必须二选一，实际 {}",
            read.open_mode()
        );
        let c = read.lock();
        assert!(c.execute("INSERT INTO users(id) VALUES('x')", []).is_err());
    }

    #[test]
    fn missing_table_is_named_not_ignored() {
        // 空库必须被点名：静默返回全 0 看板是本项目明令禁止的降级。
        let dir = TmpDir::create("empty-db");
        let path = dir.join("bare.db");
        {
            let c = Connection::open(&path).unwrap();
            c.execute("CREATE TABLE users(id TEXT)", []).unwrap();
        }
        let read = PlatformRead::open(&path).unwrap();
        let missing = read.missing_tables().unwrap();
        assert!(missing.contains(&"audit_log".to_string()), "{missing:?}");
        assert!(!missing.contains(&"users".to_string()));
        assert!(read.stats().is_err(), "缺表时统计必须报错");
    }

    #[test]
    fn nonexistent_db_file_does_not_create_one() {
        let dir = TmpDir::create("no-file");
        let path = dir.join("does-not-exist.db");
        assert!(PlatformRead::open(&path).is_err());
        assert!(!path.exists(), "只读打开不能顺手建库");
    }

    #[test]
    fn stats_come_from_the_same_db() {
        let (_dir, read, _p) = seeded_db("stats", true);
        let s = read.stats().unwrap();
        assert_eq!(s.users_total, 3);
        // 3 次 register（平台自己落的）+ 3 次手工 audit()
        assert_eq!(s.audit_in_sample_24h, 6);
        assert_eq!(s.audit_sample_rows, AUDIT_SAMPLE_ROWS);
        assert_eq!(s.reports_open, 0);
        assert_eq!(s.wallet_sum_minor, 0);
        assert_eq!(s.groups_total, 0);
        assert!(s.last_reconcile.is_none(), "没跑过对账就不该有这一行");
    }

    #[test]
    fn like_underscore_is_escaped_but_prefix_search_still_works() {
        let (_dir, read, _p) = seeded_db("like", true);
        // "bob_" 里的 _ 是单字符通配：不转义就会同时命中 bobby
        let hit = read
            .users(&ListFilter::new(Some("bob_".into()), Some(10), Some(0)))
            .unwrap();
        assert_eq!(hit.total, 1, "_ 不能当通配用: {:?}", hit.rows);
        assert_eq!(hit.rows[0].name, "bob_x");
        // 真正的前缀搜索仍然有效
        let prefix = read
            .users(&ListFilter::new(Some("bob".into()), Some(10), Some(0)))
            .unwrap();
        assert_eq!(prefix.total, 2, "bob_x + bobby: {:?}", prefix.rows);
        let all = read.users(&ListFilter::new(None, None, None)).unwrap();
        assert_eq!(all.total, 3);
    }

    #[test]
    fn audit_search_by_command_finds_agent_write_ops() {
        let (_dir, read, _p) = seeded_db("audit-cmd", true);
        let hits = read
            .audit(
                &AuditFilter::new(AuditQuery {
                    command: Some("group.create".into()),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(hits.total, 1);
        assert_eq!(hits.rows[0].action, "cmd.group.create");
        assert_eq!(hits.rows[0].target, "/api/groups");
        assert_eq!(hits.rows[0].detail, "status=200 method=POST");

        // command + actor 组合：alice 没发过 msg.send
        let alice = read
            .users(&ListFilter::new(Some("alice".into()), None, None))
            .unwrap();
        let uid = alice.rows[0].id.clone();
        let none = read
            .audit(
                &AuditFilter::new(AuditQuery {
                    command: Some("msg.send".into()),
                    actor: Some(uid.clone()),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(none.total, 0);
        // alice 的三条：register（平台落的）+ cmd.group.create + user.login
        let alice_rows = read
            .audit(
                &AuditFilter::new(AuditQuery {
                    actor: Some(uid),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(alice_rows.total, 3);
        let alice_cmds = read
            .audit(
                &AuditFilter::new(AuditQuery {
                    actor: Some(alice.rows[0].id.clone()),
                    action: Some("cmd.group.create".into()),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(alice_cmds.total, 1);
        let by_target = read
            .audit(
                &AuditFilter::new(AuditQuery {
                    target: Some("/api/groups".into()),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(by_target.total, 1);
    }

    #[test]
    fn audit_window_is_inclusive_and_rejects_a_reversed_range() {
        let (_dir, read, _p) = seeded_db("audit-window", true);
        let now = now_ms();
        let all = read
            .audit(
                &AuditFilter::new(AuditQuery {
                    from: Some(0),
                    to: Some(now + 1_000),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(all.total, 6);
        let future = read
            .audit(
                &AuditFilter::new(AuditQuery {
                    from: Some(now + 10_000),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(future.total, 0, "未来窗口就该是空");
        let reversed = AuditQuery {
            from: Some(now),
            to: Some(1),
            ..Default::default()
        };
        assert!(
            AuditFilter::new(reversed).is_err(),
            "区间反了要报错，不能静默返回空"
        );
    }

    #[test]
    fn audit_rows_newest_first_and_paged() {
        let (_dir, read, _p) = seeded_db("audit-page", true);
        let page = read
            .audit(
                &AuditFilter::new(AuditQuery {
                    limit: Some(2),
                    offset: Some(0),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(page.total, 6);
        assert_eq!(page.rows.len(), 2);
        assert!(page.rows[0].id > page.rows[1].id, "新的在前");
        let rest = read
            .audit(
                &AuditFilter::new(AuditQuery {
                    limit: Some(2),
                    offset: Some(2),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(rest.rows.len(), 2);
        assert_eq!(rest.total, 6);
        // 翻页不重不漏：第二页最老的一条必须严格小于第一页最老的一条
        assert!(rest.rows[1].id < page.rows[1].id);
    }

    #[test]
    fn unknown_ticket_state_in_query_is_rejected_with_the_menu() {
        let err = ReportFilter::new(ReportQuery {
            state: Some("pending".into()),
            ..Default::default()
        })
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("open") && msg.contains("rejected"), "{msg}");
    }

    #[test]
    fn reports_read_through_the_real_state_machine_types() {
        // 用 wxlike-report 的 Ticket 类型读：状态、处置、历史必须原样还原，
        // 否则看板显示的状态在平台里根本不存在。
        let dir = TmpDir::create("reports");
        let path = dir.join("wxlike.db");
        let ticket_id = {
            let p = Platform::open(Some(path.to_str().unwrap())).unwrap();
            let alice = p.register("alice", "pw").unwrap();
            let t = p
                .create_report(&alice, "user", "bob", Reason::Spam, "广告轰炸")
                .unwrap();
            p.triage_report(&t.id, "ops", "先看").unwrap();
            p.act_on_report(&t.id, "ops", Action::Warn, "警告一次")
                .unwrap();
            t.id
        };
        let read = PlatformRead::open(&path).unwrap();
        let page = read
            .reports(&ReportFilter::new(ReportQuery::default()).unwrap())
            .unwrap();
        assert_eq!(page.total, 1);
        let t = &page.rows[0].ticket;
        assert_eq!(t.id, ticket_id);
        assert_eq!(t.state, TicketState::Actioned);
        assert_eq!(t.action, Some(Action::Warn));
        assert_eq!(t.history.len(), 2, "两次流转都要在");
        assert_eq!(t.reason, Reason::Spam);
        assert_eq!(t.detail, "广告轰炸");
        assert_eq!(t.handler.as_deref(), Some("ops"));
        // 单张读取 + 过滤
        let one = read.report(&ticket_id).unwrap().expect("在库里");
        assert_eq!(one.state, TicketState::Actioned);
        assert!(read.report("deadbeef").unwrap().is_none());
        let closed = read
            .reports(
                &ReportFilter::new(ReportQuery {
                    state: Some("closed".into()),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(closed.total, 0);
        let open_only = read
            .reports(
                &ReportFilter::new(ReportQuery {
                    open_only: Some(true),
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        assert_eq!(open_only.total, 1, "actioned 也是未终结");
    }

    #[test]
    fn corrupt_state_string_is_an_error_not_a_default() {
        // 直接往库里塞一个状态机认不得的值（模拟版本漂移/手工改库）
        let dir = TmpDir::create("corrupt-report");
        let path = dir.join("wxlike.db");
        {
            let p = Platform::open(Some(path.to_str().unwrap())).unwrap();
            let alice = p.register("alice", "pw").unwrap();
            let t = p
                .create_report(&alice, "user", "bob", Reason::Spam, "x")
                .unwrap();
            // 用平台自己的连接改坏（adminx 只读，改坏它也没用——这正是设计）
            p.db()
                .lock()
                .execute(
                    "UPDATE reports SET state = 'under_review' WHERE id = ?1",
                    params![t.id],
                )
                .unwrap();
        }
        let read = PlatformRead::open(&path).unwrap();
        let err = read
            .reports(&ReportFilter::new(ReportQuery::default()).unwrap())
            .unwrap_err();
        assert!(err.to_string().contains("state"), "{err}");
    }

    #[test]
    fn long_filter_values_are_refused() {
        let err = AuditFilter::new(AuditQuery {
            actor: Some("x".repeat(200)),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("128"), "{err}");
    }
}
