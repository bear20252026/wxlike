//! `wxlike_identity::sqlport::SqlExec` 的平台实现：把 `StorageConnection` 适配成
//! 文本/整数参数 + 按列类型取一行的最小执行面。
//!
//! 只做参数与行的翻译，不含 SQL；SQL 与状态机留在各自域里。

use crate::error::PlatformError;
use crate::storage::port::{Param, Params};
use crate::storage::{StorageConnection, StorageRow};
use wxlike_identity::qr_login::{QrExec, QrKind, QrParam, QrValue};

pub(crate) struct StorageSqlExec<'a, 'b>(pub(crate) &'b StorageConnection<'a>);

fn param(p: &QrParam) -> Param {
    match p {
        QrParam::Text(v) => Param::Text(v.clone()),
        QrParam::Int(v) => Param::Int(*v),
    }
}

fn db_err(e: PlatformError) -> wxlike_identity::IdError {
    wxlike_identity::IdError::Storage(e.to_string())
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
                out.push(match kind {
                    QrKind::Text => QrValue::Text(r.get::<_, String>(i)?),
                    QrKind::Int => QrValue::Int(r.get::<_, i64>(i)?),
                });
            }
            Ok(out)
        });
        match row {
            Ok(values) => Ok(Some(values)),
            Err(PlatformError::NotFound) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }
}
