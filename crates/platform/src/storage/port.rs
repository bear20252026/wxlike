//! 双后端 repo 执行端口：同一份 SQL 在 SQLite 与 PostgreSQL 上执行。
//!
//! 这是 G00-04 PG 线的第一块拼图。旧 repo 的 SQL 是 SQLite 方言（`?N` 占位符、
//! 少量 `INSERT OR IGNORE`），但表结构在 PG 上由 M011+ 的兼容层承接。与其把
//! 353 个函数逐条改写成 sqlx，不如把差异收在一处：
//!
//! * 占位符：PG 侧把 `?N` 转成 `$N`；
//! * `INSERT OR IGNORE`：PG 侧转成 `INSERT ... ON CONFLICT DO NOTHING`；
//! * 参数：`params!` 产出后端中立的 [`Param`]，SQLite 用 `ToSql`，PG 用 `bind`；
//! * 行： [`StorageRow`] 同时包装 `rusqlite::Row` 与 `sqlx::PgRow`，repo 的
//!   `r.get(0)` 不用改。
//!
//! 事务不在本模块：`with_tx` / `unchecked_transaction` 仍只走 SQLite，等 PG
//! 事务端口单独落地。这里先把**无事务**的 repo 批量化，避免把两件事混在一起。

use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::Connection;
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row as _, ValueRef as _};

use crate::error::{PlatformError, Result};
use crate::storage::{PostgresPool, StorageConnection};

/// 一条绑定参数。类型信息要在 PG 上保留，`NULL` 不能一律当文本。
#[derive(Clone, Debug)]
pub enum Param {
    NullText,
    NullInt,
    Text(String),
    Int(i64),
    Real(f64),
    Blob(Vec<u8>),
}

/// 参数集合。`params!` 产出的就是这个。
pub struct Params(Vec<Param>);

impl Params {
    #[must_use]
    pub fn new(values: Vec<Param>) -> Self {
        Self(values)
    }

    fn as_slice(&self) -> &[Param] {
        &self.0
    }
}

/// 值 → 参数。`&T` 由 blanket impl 覆盖，所以 `params![&name]` 也能用。
pub trait ToParam {
    fn to_param(&self) -> Param;

    /// `Option<T>` 的 None 落成哪种 NULL：默认文本，整数需要覆盖。
    fn null_param() -> Param {
        Param::NullText
    }
}

impl<T: ToParam + ?Sized> ToParam for &T {
    fn to_param(&self) -> Param {
        (*self).to_param()
    }
}

impl ToParam for str {
    fn to_param(&self) -> Param {
        Param::Text(self.to_string())
    }
}

impl ToParam for String {
    fn to_param(&self) -> Param {
        Param::Text(self.clone())
    }
}

impl ToParam for i64 {
    fn to_param(&self) -> Param {
        Param::Int(*self)
    }

    fn null_param() -> Param {
        Param::NullInt
    }
}

impl ToParam for i32 {
    fn to_param(&self) -> Param {
        Param::Int(i64::from(*self))
    }

    fn null_param() -> Param {
        Param::NullInt
    }
}

impl ToParam for u64 {
    fn to_param(&self) -> Param {
        Param::Int(*self as i64)
    }

    fn null_param() -> Param {
        Param::NullInt
    }
}

impl ToParam for bool {
    fn to_param(&self) -> Param {
        Param::Int(i64::from(*self))
    }

    fn null_param() -> Param {
        Param::NullInt
    }
}

impl ToParam for f64 {
    fn to_param(&self) -> Param {
        Param::Real(*self)
    }
}

impl ToParam for Vec<u8> {
    fn to_param(&self) -> Param {
        Param::Blob(self.clone())
    }
}

impl<T: ToParam> ToParam for Option<T> {
    fn to_param(&self) -> Param {
        match self {
            Some(v) => v.to_param(),
            None => T::null_param(),
        }
    }

    fn null_param() -> Param {
        T::null_param()
    }
}

/// `params![a, b, c]`：后端中立，替代 `rusqlite::params!`。
macro_rules! params {
    ($($value:expr),* $(,)?) => {
        $crate::storage::port::Params::new(vec![
            $($crate::storage::port::ToParam::to_param(&$value)),*
        ])
    };
}

pub(crate) use params;

impl rusqlite::ToSql for Param {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self {
            Param::NullText | Param::NullInt => ToSqlOutput::Borrowed(ValueRef::Null),
            Param::Text(v) => ToSqlOutput::Borrowed(ValueRef::Text(v.as_bytes())),
            Param::Int(v) => ToSqlOutput::Borrowed(ValueRef::Integer(*v)),
            Param::Real(v) => ToSqlOutput::Borrowed(ValueRef::Real(*v)),
            Param::Blob(v) => ToSqlOutput::Borrowed(ValueRef::Blob(v)),
        })
    }
}

/// `?N` → `$N`，并把 SQLite 的 `INSERT OR IGNORE` 换成 PG 的冲突忽略。
fn pg_sql(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '?' {
            out.push(c);
            continue;
        }
        let mut digits = String::new();
        while let Some(d) = chars.peek().copied().filter(char::is_ascii_digit) {
            digits.push(d);
            chars.next();
        }
        if digits.is_empty() {
            out.push('?');
        } else {
            out.push('$');
            out.push_str(&digits);
        }
    }
    if let Some(rest) = out.strip_prefix("INSERT OR IGNORE") {
        let mut fixed = String::from("INSERT");
        fixed.push_str(rest);
        if !fixed.trim_end().ends_with(';') {
            fixed.push_str(" ON CONFLICT DO NOTHING");
        }
        return fixed;
    }
    out
}

fn pg_error(e: sqlx::Error) -> PlatformError {
    PlatformError::Storage(format!("postgres: {e}"))
}

fn bind_all<'q>(
    sql: &'q str,
    params: &[Param],
) -> sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments> {
    let mut query = sqlx::query(sql);
    for p in params {
        query = match p {
            Param::NullText => query.bind(Option::<String>::None),
            Param::NullInt => query.bind(Option::<i64>::None),
            Param::Text(v) => query.bind(v.clone()),
            Param::Int(v) => query.bind(*v),
            Param::Real(v) => query.bind(*v),
            Param::Blob(v) => query.bind(v.clone()),
        };
    }
    query
}

/// 后端连接视图：repo 只看见它，不直接碰 rusqlite / sqlx。
#[derive(Clone, Copy)]
pub(crate) enum PortConn<'a> {
    Sqlite(&'a Connection),
    Pg(&'a PostgresPool),
}

/// 一行。SQLite 与 PG 的行都包在这里，`get` 的位置参数语义一致。
pub struct StorageRow<'a>(RowRef<'a>);

enum RowRef<'a> {
    Sqlite(&'a rusqlite::Row<'a>),
    Pg(&'a PgRow),
}

impl<'a> StorageRow<'a> {
    fn sqlite(row: &'a rusqlite::Row<'a>) -> Self {
        Self(RowRef::Sqlite(row))
    }

    fn pg(row: &'a PgRow) -> Self {
        Self(RowRef::Pg(row))
    }

    /// 按列序号取值。签名保留 `get::<_, T>(i)` 的两个泛型，repo 不用改。
    pub fn get<I: RowIndex, T: FromStorage>(&self, idx: I) -> Result<T> {
        T::from_row(self, idx.position())
    }
}

/// 列序号。只支持位置：旧 repo 的 `r.get(0)` 全是位置取值。
pub trait RowIndex {
    fn position(&self) -> usize;
}

impl RowIndex for usize {
    fn position(&self) -> usize {
        *self
    }
}

impl RowIndex for i32 {
    fn position(&self) -> usize {
        *self as usize
    }
}

/// 行 → 值。PG 的 INT4/INT8 都归到 i64，避免调用点关心 PG 的整数宽度。
pub trait FromStorage: Sized {
    fn from_row(row: &StorageRow<'_>, idx: usize) -> Result<Self>;
}

impl FromStorage for String {
    fn from_row(row: &StorageRow<'_>, idx: usize) -> Result<Self> {
        match &row.0 {
            RowRef::Sqlite(r) => r.get(idx).map_err(PlatformError::from),
            RowRef::Pg(r) => r.try_get(idx).map_err(pg_error),
        }
    }
}

impl FromStorage for i64 {
    fn from_row(row: &StorageRow<'_>, idx: usize) -> Result<Self> {
        match &row.0 {
            RowRef::Sqlite(r) => r.get(idx).map_err(PlatformError::from),
            RowRef::Pg(r) => r
                .try_get::<i64, _>(idx)
                .or_else(|_| r.try_get::<i32, _>(idx).map(i64::from))
                .map_err(pg_error),
        }
    }
}

impl FromStorage for i32 {
    fn from_row(row: &StorageRow<'_>, idx: usize) -> Result<Self> {
        i64::from_row(row, idx).map(|v| v as i32)
    }
}

impl FromStorage for u64 {
    fn from_row(row: &StorageRow<'_>, idx: usize) -> Result<Self> {
        i64::from_row(row, idx).map(|v| v as u64)
    }
}

impl FromStorage for bool {
    fn from_row(row: &StorageRow<'_>, idx: usize) -> Result<Self> {
        match &row.0 {
            RowRef::Sqlite(r) => r
                .get::<_, i64>(idx)
                .map(|v| v != 0)
                .map_err(PlatformError::from),
            RowRef::Pg(r) => r
                .try_get::<bool, _>(idx)
                .or_else(|_| r.try_get::<i64, _>(idx).map(|v| v != 0))
                .map_err(pg_error),
        }
    }
}

impl FromStorage for f64 {
    fn from_row(row: &StorageRow<'_>, idx: usize) -> Result<Self> {
        match &row.0 {
            RowRef::Sqlite(r) => r.get(idx).map_err(PlatformError::from),
            RowRef::Pg(r) => r.try_get(idx).map_err(pg_error),
        }
    }
}

impl FromStorage for Vec<u8> {
    fn from_row(row: &StorageRow<'_>, idx: usize) -> Result<Self> {
        match &row.0 {
            RowRef::Sqlite(r) => r.get(idx).map_err(PlatformError::from),
            RowRef::Pg(r) => r.try_get(idx).map_err(pg_error),
        }
    }
}

impl<T: FromStorage> FromStorage for Option<T> {
    fn from_row(row: &StorageRow<'_>, idx: usize) -> Result<Self> {
        let is_null = match &row.0 {
            RowRef::Sqlite(r) => matches!(
                r.get_ref(idx).map_err(PlatformError::from)?,
                rusqlite::types::ValueRef::Null
            ),
            RowRef::Pg(r) => r.try_get_raw(idx).map_err(pg_error)?.is_null(),
        };
        if is_null {
            Ok(None)
        } else {
            T::from_row(row, idx).map(Some)
        }
    }
}

/// `query_row` 没有命中时用 `NotFound` 表达；`.optional()` 把它折成 `None`。
pub(crate) trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>>;
}

impl<T> OptionalExt<T> for Result<T> {
    fn optional(self) -> Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(PlatformError::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// 预编译语句的替代物。SQLite 每次调用重新 prepare；PG 直接执行。
pub struct StorageStatement<'a> {
    conn: PortConn<'a>,
    sql: String,
}

impl<'a> StorageStatement<'a> {
    pub fn query_map<T, F>(
        &self,
        params: Params,
        mut map: F,
    ) -> Result<std::vec::IntoIter<Result<T>>>
    where
        F: FnMut(&StorageRow<'_>) -> Result<T>,
    {
        match self.conn {
            PortConn::Sqlite(conn) => {
                let mut stmt = conn.prepare(&self.sql)?;
                let mut rows = stmt.query(rusqlite::params_from_iter(params.as_slice().iter()))?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    let row = StorageRow::sqlite(row);
                    out.push(map(&row));
                }
                Ok(out.into_iter())
            }
            PortConn::Pg(pool) => {
                let sql = pg_sql(&self.sql);
                let query = bind_all(&sql, params.as_slice());
                let rows = pool
                    .block_on(query.fetch_all(pool.pool()))
                    .map_err(pg_error)?;
                let out: Vec<Result<T>> =
                    rows.iter().map(|row| map(&StorageRow::pg(row))).collect();
                Ok(out.into_iter())
            }
        }
    }
}

impl StorageConnection<'_> {
    /// 端口视图：repo 从这里拿 execute / query_row / prepare。
    pub(crate) fn ops(&self) -> PortConn<'_> {
        match self {
            StorageConnection::Sqlite(c) => PortConn::Sqlite(c),
            StorageConnection::SqliteTx(c) => PortConn::Sqlite(c),
            StorageConnection::Postgres(p) => PortConn::Pg(p),
        }
    }

    pub(crate) fn execute(&self, sql: &str, params: Params) -> Result<usize> {
        match self.ops() {
            PortConn::Sqlite(c) => c
                .execute(sql, rusqlite::params_from_iter(params.as_slice().iter()))
                .map_err(PlatformError::from),
            PortConn::Pg(p) => {
                let sql = pg_sql(sql);
                let query = bind_all(&sql, params.as_slice());
                p.block_on(query.execute(p.pool()))
                    .map(|done| done.rows_affected() as usize)
                    .map_err(pg_error)
            }
        }
    }

    pub(crate) fn query_row<T, F>(&self, sql: &str, params: Params, map: F) -> Result<T>
    where
        F: FnOnce(&StorageRow<'_>) -> Result<T>,
    {
        match self.ops() {
            PortConn::Sqlite(c) => {
                let mut stmt = c.prepare(sql)?;
                let mut rows = stmt.query(rusqlite::params_from_iter(params.as_slice().iter()))?;
                let Some(row) = rows.next()? else {
                    return Err(PlatformError::NotFound);
                };
                map(&StorageRow::sqlite(row))
            }
            PortConn::Pg(p) => {
                let sql = pg_sql(sql);
                let query = bind_all(&sql, params.as_slice());
                let row = p
                    .block_on(query.fetch_optional(p.pool()))
                    .map_err(pg_error)?;
                let Some(row) = row else {
                    return Err(PlatformError::NotFound);
                };
                map(&StorageRow::pg(&row))
            }
        }
    }

    pub(crate) fn prepare(&self, sql: &str) -> Result<StorageStatement<'_>> {
        Ok(StorageStatement {
            conn: self.ops(),
            sql: sql.to_string(),
        })
    }
}
