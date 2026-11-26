//! 关系域的存储执行端口：只提供治理需要的三种动作。
//!
//! 与 identity 的 `SqlExec` 分开定义，是为了不让 relations 依赖 identity；
//! 两边形状一致（参数/列类型/值），平台适配器分别实现即可。

use rusqlite::Connection;

use crate::{RelError, Result};

/// SQL 参数。
#[derive(Clone, Debug)]
pub enum RelParam {
    Text(String),
    Int(i64),
    /// 显式 NULL（`group_meta` 的冻结/解散时间）。
    Null,
}

impl From<RelParam> for rusqlite::types::Value {
    fn from(p: RelParam) -> Self {
        match p {
            RelParam::Text(v) => Self::Text(v),
            RelParam::Int(v) => Self::Integer(v),
            RelParam::Null => Self::Null,
        }
    }
}

/// 期望的列类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelKind {
    Text,
    Int,
}

/// 从端口读回的值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelValue {
    Text(String),
    Int(i64),
    Null,
}

impl RelValue {
    pub fn into_text(self) -> Result<String> {
        match self {
            Self::Text(v) => Ok(v),
            _ => Err(RelError::Corrupt("relations row type")),
        }
    }

    pub fn into_int(self) -> Result<i64> {
        match self {
            Self::Int(v) => Ok(v),
            _ => Err(RelError::Corrupt("relations row type")),
        }
    }

    pub fn opt_text(self) -> Result<Option<String>> {
        match self {
            Self::Text(v) => Ok(Some(v)),
            Self::Null => Ok(None),
            Self::Int(_) => Err(RelError::Corrupt("relations row type")),
        }
    }

    pub fn opt_int(self) -> Result<Option<i64>> {
        match self {
            Self::Int(v) => Ok(Some(v)),
            Self::Null => Ok(None),
            Self::Text(_) => Err(RelError::Corrupt("relations row type")),
        }
    }
}

/// 同步 SQL 端口：关系治理的全部 SQL 都走这里。
pub trait RelExec {
    fn exec(&self, sql: &str, params: &[RelParam]) -> Result<usize>;
    fn query(
        &self,
        sql: &str,
        params: &[RelParam],
        kinds: &[RelKind],
    ) -> Result<Option<Vec<RelValue>>>;
    fn query_many(
        &self,
        sql: &str,
        params: &[RelParam],
        kinds: &[RelKind],
    ) -> Result<Vec<Vec<RelValue>>>;
}

impl RelExec for Connection {
    fn exec(&self, sql: &str, params: &[RelParam]) -> Result<usize> {
        let values: Vec<rusqlite::types::Value> = params.iter().cloned().map(Into::into).collect();
        Ok(Connection::execute(
            self,
            sql,
            rusqlite::params_from_iter(values.iter()),
        )?)
    }

    fn query(
        &self,
        sql: &str,
        params: &[RelParam],
        kinds: &[RelKind],
    ) -> Result<Option<Vec<RelValue>>> {
        let values: Vec<rusqlite::types::Value> = params.iter().cloned().map(Into::into).collect();
        let mut stmt = self.prepare(sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(values.iter()))?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(read_row(row, kinds)?))
    }

    fn query_many(
        &self,
        sql: &str,
        params: &[RelParam],
        kinds: &[RelKind],
    ) -> Result<Vec<Vec<RelValue>>> {
        let values: Vec<rusqlite::types::Value> = params.iter().cloned().map(Into::into).collect();
        let mut stmt = self.prepare(sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(values.iter()))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(read_row(row, kinds)?);
        }
        Ok(out)
    }
}

fn read_row(row: &rusqlite::Row<'_>, kinds: &[RelKind]) -> Result<Vec<RelValue>> {
    let mut out = Vec::with_capacity(kinds.len());
    for (i, kind) in kinds.iter().enumerate() {
        let raw: rusqlite::types::Value = row.get(i)?;
        let value = match raw {
            rusqlite::types::Value::Text(v) => RelValue::Text(v),
            rusqlite::types::Value::Integer(v) => RelValue::Int(v),
            rusqlite::types::Value::Null => RelValue::Null,
            _ => return Err(RelError::Corrupt("relations row type")),
        };
        match (kind, &value) {
            (RelKind::Text, RelValue::Text(_))
            | (RelKind::Text, RelValue::Null)
            | (RelKind::Int, RelValue::Int(_))
            | (RelKind::Int, RelValue::Null) => {}
            _ => return Err(RelError::Corrupt("relations row type")),
        }
        out.push(value);
    }
    Ok(out)
}
