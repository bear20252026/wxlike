//! 身份域的最小 SQL 执行面：文本/整数参数、按列类型取一行。
//!
//! 只服务“状态机 SQL 不想为 PG 再抄一遍”的场景；事务仍由调用方的
//! `Db::with_tx` 负责。

use rusqlite::Connection;

use crate::{IdError, Result};

/// 端口无关的一条绑定参数：二维码状态机只用到文本与整数。
#[derive(Clone, Debug)]
pub enum SqlParam {
    Text(String),
    Int(i64),
}

/// 查询结果列的目标类型。端口实现按它取值，避免把整数列读成文本。
#[derive(Clone, Copy, Debug)]
pub enum ColKind {
    Text,
    Int,
}

/// 端口返回的一列值。
#[derive(Clone, Debug)]
pub enum SqlValue {
    Text(String),
    Int(i64),
}

impl SqlValue {
    pub(crate) fn into_text(self) -> Result<String> {
        match self {
            Self::Text(v) => Ok(v),
            Self::Int(_) => Err(IdError::Corrupt("sql port.column type")),
        }
    }

    pub(crate) fn into_int(self) -> Result<i64> {
        match self {
            Self::Int(v) => Ok(v),
            Self::Text(_) => Err(IdError::Corrupt("sql port.column type")),
        }
    }
}

/// 后端无关的最小执行面：`exec` 写、`query` 读一行。
///
/// 平台侧把 `StorageConnection` 适配到这个 trait，于是同一份域 SQL 在
/// SQLite 与 PostgreSQL 上都跑；identity 自身用 [`SqliteExec`] 适配。
pub trait SqlExec {
    fn exec(&self, sql: &str, params: &[SqlParam]) -> Result<usize>;
    fn query(
        &self,
        sql: &str,
        params: &[SqlParam],
        kinds: &[ColKind],
    ) -> Result<Option<Vec<SqlValue>>>;
}

/// identity 自带的后端：直接走 `rusqlite`。
pub struct SqliteExec<'a>(pub &'a Connection);

fn sqlite_param(p: &SqlParam) -> rusqlite::types::Value {
    match p {
        SqlParam::Text(v) => rusqlite::types::Value::Text(v.clone()),
        SqlParam::Int(v) => rusqlite::types::Value::Integer(*v),
    }
}

impl SqlExec for SqliteExec<'_> {
    fn exec(&self, sql: &str, params: &[SqlParam]) -> Result<usize> {
        let values: Vec<_> = params.iter().map(sqlite_param).collect();
        Ok(self
            .0
            .execute(sql, rusqlite::params_from_iter(values.iter()))?)
    }

    fn query(
        &self,
        sql: &str,
        params: &[SqlParam],
        kinds: &[ColKind],
    ) -> Result<Option<Vec<SqlValue>>> {
        let values: Vec<_> = params.iter().map(sqlite_param).collect();
        let mut stmt = self.0.prepare(sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(values.iter()))?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let mut out = Vec::with_capacity(kinds.len());
        for (i, kind) in kinds.iter().enumerate() {
            let value = match (kind, row.get_ref(i)?) {
                (ColKind::Text, rusqlite::types::ValueRef::Text(v)) => {
                    SqlValue::Text(String::from_utf8_lossy(v).into_owned())
                }
                (ColKind::Text, _) => return Err(IdError::Corrupt("sql port.column type")),
                (ColKind::Int, rusqlite::types::ValueRef::Integer(v)) => SqlValue::Int(v),
                (ColKind::Int, _) => return Err(IdError::Corrupt("sql port.column type")),
            };
            out.push(value);
        }
        Ok(Some(out))
    }
}
