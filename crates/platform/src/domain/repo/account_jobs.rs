//! **仓储层**（`account_jobs`）：账号导出作业生命周期的端口版。
//!
//! SQL 与状态规则在 identity `preferences` 的 `*_with` 函数里只有一份；这里只把
//! `StorageConnection` 适配成 `SqlExec`。注销申请仍走 `preferences::request_delete`
//! （它要撤销会话/刷新链/step-up，属于后续批次）。

use crate::error::Result;
use crate::storage::sql_port::StorageSqlExec;
use crate::storage::StorageConnection;
use wxlike_identity::preferences::{self, Job};

pub(in crate::domain) fn request_export(
    conn: &StorageConnection<'_>,
    user_id: &str,
    now: i64,
) -> Result<Job> {
    Ok(preferences::request_export_with(
        &StorageSqlExec(conn),
        user_id,
        now,
    )?)
}

pub(in crate::domain) fn request_delete(
    conn: &StorageConnection<'_>,
    user_id: &str,
    now: i64,
) -> Result<Job> {
    Ok(preferences::request_delete_with(
        &StorageSqlExec(conn),
        user_id,
        now,
    )?)
}

pub(in crate::domain) fn job(conn: &StorageConnection<'_>, job_id: &str) -> Result<Option<Job>> {
    Ok(preferences::job_with(&StorageSqlExec(conn), job_id)?)
}

pub(in crate::domain) fn list(conn: &StorageConnection<'_>, user_id: &str) -> Result<Vec<Job>> {
    Ok(preferences::list_with(&StorageSqlExec(conn), user_id)?)
}

pub(in crate::domain) fn cancel(
    conn: &StorageConnection<'_>,
    job_id: &str,
    user_id: &str,
    now: i64,
) -> Result<Job> {
    Ok(preferences::cancel_with(
        &StorageSqlExec(conn),
        job_id,
        user_id,
        now,
    )?)
}

pub(in crate::domain) fn claim(conn: &StorageConnection<'_>, now: i64) -> Result<Option<Job>> {
    Ok(preferences::claim_with(&StorageSqlExec(conn), now)?)
}

pub(in crate::domain) fn finish(
    conn: &StorageConnection<'_>,
    job_id: &str,
    artifact: &str,
    bytes: i64,
    now: i64,
) -> Result<()> {
    Ok(preferences::finish_with(
        &StorageSqlExec(conn),
        job_id,
        artifact,
        bytes,
        now,
    )?)
}

pub(in crate::domain) fn fail(
    conn: &StorageConnection<'_>,
    job_id: &str,
    reason: &str,
    now: i64,
) -> Result<()> {
    Ok(preferences::fail_with(
        &StorageSqlExec(conn),
        job_id,
        reason,
        now,
    )?)
}

pub(in crate::domain) fn redeem_link(
    conn: &StorageConnection<'_>,
    raw: &str,
    now: i64,
) -> Result<String> {
    Ok(preferences::redeem_link_with(
        &StorageSqlExec(conn),
        raw,
        now,
    )?)
}

pub(in crate::domain) fn export_link(
    conn: &StorageConnection<'_>,
    job_id: &str,
    user_id: &str,
    now: i64,
) -> Result<String> {
    Ok(preferences::export_link_with(
        &StorageSqlExec(conn),
        job_id,
        user_id,
        now,
    )?)
}

#[cfg(test)]
mod pg_tests {
    use super::{cancel, job, list, request_delete, request_export};
    use crate::storage::{params, StorageProfile};
    use crate::Db;
    use wxlike_identity::preferences::JobStatus;

    #[test]
    #[ignore = "requires WXLIKE_TEST_PG_URL and M014 applied"]
    fn postgres_account_export_job_is_created_listed_and_cancelled() {
        let Ok(url) = std::env::var("WXLIKE_TEST_PG_URL") else {
            return;
        };
        let db = Db::open_profile(StorageProfile::Postgres { url }, None).expect("pg db");
        let user = "pg-acct-jobs";
        let conn = db.acquire();
        conn.execute("DELETE FROM identity_job WHERE user_id = ?1", params![user])
            .expect("clean jobs");
        drop(conn);

        let created = db
            .with_tx(|c| request_export(c, user, 1_000))
            .expect("request export");
        assert_eq!(created.status, JobStatus::Queued);
        let listed = db.with_tx(|c| list(c, user)).expect("list");
        assert_eq!(listed.len(), 1);
        let found = db.with_tx(|c| job(c, &created.id)).expect("job");
        assert_eq!(found.expect("row").id, created.id);
        let cancelled = db
            .with_tx(|c| cancel(c, &created.id, user, 1_100))
            .expect("cancel");
        assert_eq!(cancelled.status, JobStatus::Cancelled);
        let delete_job = db
            .with_tx(|c| request_delete(c, user, 1_200))
            .expect("request delete");
        assert_ne!(delete_job.id, created.id);
        assert!(matches!(
            delete_job.kind,
            wxlike_identity::preferences::JobKind::Delete
        ));
    }
}
