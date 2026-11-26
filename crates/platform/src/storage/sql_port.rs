//! `wxlike_identity::sqlport::SqlExec` 的平台实现：把 `StorageConnection` 适配成
//! 文本/整数参数 + 按列类型取一行的最小执行面。
//!
//! 只做参数与行的翻译，不含 SQL；SQL 与状态机留在各自域里。

use crate::error::PlatformError;
use crate::storage::port::{Param, Params};
use crate::storage::{StorageConnection, StorageDialect, StorageRow};
use wxlike_identity::qr_login::{QrExec, QrKind, QrParam, QrValue};

pub(crate) struct StorageSqlExec<'a, 'b>(pub(crate) &'b StorageConnection<'a>);

fn param(p: &QrParam) -> Param {
    match p {
        QrParam::Text(v) => Param::Text(v.clone()),
        QrParam::Int(v) => Param::Int(*v),
        QrParam::Null => Param::NullInt,
    }
}

fn db_err(e: PlatformError) -> wxlike_identity::IdError {
    wxlike_identity::IdError::Storage(e.to_string())
}

fn opt_text(r: &StorageRow<'_>, i: usize) -> Result<QrValue, PlatformError> {
    match r.get::<_, Option<String>>(i)? {
        Some(v) => Ok(QrValue::Text(v)),
        None => Ok(QrValue::Null),
    }
}

fn opt_int(r: &StorageRow<'_>, i: usize) -> Result<QrValue, PlatformError> {
    match r.get::<_, Option<i64>>(i)? {
        Some(v) => Ok(QrValue::Int(v)),
        None => Ok(QrValue::Null),
    }
}

fn cell(r: &StorageRow<'_>, i: usize, kind: &QrKind) -> Result<QrValue, PlatformError> {
    match kind {
        QrKind::Text => Ok(QrValue::Text(r.get::<_, String>(i)?)),
        QrKind::Int => Ok(QrValue::Int(r.get::<_, i64>(i)?)),
        QrKind::OptText => opt_text(r, i),
        QrKind::OptInt => opt_int(r, i),
    }
}

impl QrExec for StorageSqlExec<'_, '_> {
    fn exec(&self, sql: &str, params: &[QrParam]) -> Result<usize, wxlike_identity::IdError> {
        let bound = Params::new(params.iter().map(param).collect());
        self.0.execute(sql, bound).map_err(db_err)
    }

    fn query(
        &self,
        sql: &str,
        params: &[QrParam],
        kinds: &[QrKind],
    ) -> Result<Option<Vec<QrValue>>, wxlike_identity::IdError> {
        let bound = Params::new(params.iter().map(param).collect());
        let row = self.0.query_row(sql, bound, |r: &StorageRow<'_>| {
            let mut out = Vec::with_capacity(kinds.len());
            for (i, kind) in kinds.iter().enumerate() {
                out.push(cell(r, i, kind)?);
            }
            Ok(out)
        });
        match row {
            Ok(values) => Ok(Some(values)),
            Err(PlatformError::NotFound) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    fn query_many(
        &self,
        sql: &str,
        params: &[QrParam],
        kinds: &[QrKind],
    ) -> Result<Vec<Vec<QrValue>>, wxlike_identity::IdError> {
        let bound = Params::new(params.iter().map(param).collect());
        let stmt = self.0.prepare(sql).map_err(db_err)?;
        let rows = stmt.query_map(bound, |r: &StorageRow<'_>| {
            let mut out = Vec::with_capacity(kinds.len());
            for (i, kind) in kinds.iter().enumerate() {
                out.push(cell(r, i, kind)?);
            }
            Ok(out)
        });
        let mut out = Vec::new();
        for row in rows.map_err(db_err)? {
            out.push(row.map_err(db_err)?);
        }
        Ok(out)
    }

    fn insert_returning(
        &self,
        sql: &str,
        params: &[QrParam],
    ) -> Result<i64, wxlike_identity::IdError> {
        match self.0.dialect() {
            StorageDialect::Sqlite => {
                <Self as QrExec>::exec(self, sql, params)?;
                let value = <Self as QrExec>::query(
                    self,
                    "SELECT last_insert_rowid()",
                    &[],
                    &[QrKind::Int],
                )?
                .and_then(|mut row| row.pop())
                .ok_or_else(|| wxlike_identity::IdError::Corrupt("insert id missing"))?;
                match value {
                    QrValue::Int(v) => Ok(v),
                    _ => Err(wxlike_identity::IdError::Corrupt("insert id type")),
                }
            }
            StorageDialect::Postgres => {
                let sql = format!("{sql} RETURNING seq");
                let value = <Self as QrExec>::query(self, &sql, params, &[QrKind::Int])?
                    .and_then(|mut row| row.pop())
                    .ok_or_else(|| wxlike_identity::IdError::Corrupt("insert id missing"))?;
                match value {
                    QrValue::Int(v) => Ok(v),
                    _ => Err(wxlike_identity::IdError::Corrupt("insert id type")),
                }
            }
        }
    }

    fn table_exists(&self, name: &str) -> Result<bool, wxlike_identity::IdError> {
        match self.0.dialect() {
            StorageDialect::Sqlite => {
                let value = <Self as QrExec>::query(
                    self,
                    "SELECT COUNT(*) FROM sqlite_master \
                     WHERE type IN ('table', 'view') AND name = ?1",
                    &[QrParam::Text(name.to_string())],
                    &[QrKind::Int],
                )?
                .and_then(|mut row| row.pop())
                .ok_or_else(|| wxlike_identity::IdError::Corrupt("table probe missing"))?;
                match value {
                    QrValue::Int(v) => Ok(v > 0),
                    _ => Err(wxlike_identity::IdError::Corrupt("table probe type")),
                }
            }
            StorageDialect::Postgres => {
                let value = <Self as QrExec>::query(
                    self,
                    "SELECT COUNT(*) FROM information_schema.tables WHERE table_name = ?1",
                    &[QrParam::Text(name.to_string())],
                    &[QrKind::Int],
                )?
                .and_then(|mut row| row.pop())
                .ok_or_else(|| wxlike_identity::IdError::Corrupt("table probe missing"))?;
                match value {
                    QrValue::Int(v) => Ok(v > 0),
                    _ => Err(wxlike_identity::IdError::Corrupt("table probe type")),
                }
            }
        }
    }
}
