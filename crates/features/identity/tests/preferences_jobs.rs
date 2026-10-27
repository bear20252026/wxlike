//! G02-05 作业行的**不许假装**契约（#114）。
//!
//! 审计在 `preferences.rs` 抓到两处把坏数据折成好状态的写法：
//!
//! * `head_seq` 的 `unwrap_or(0)`——库读不动 = "这个账号没有偏好"；
//! * `job_row` 的 `unwrap_or(JobKind::Export)` / `JobStatus::parse` 的
//!   `_ => Queued`——一条解析不出来的作业 = "一个正在排队的导出"。
//!
//! 后者最要命的地方：注销作业被读成导出作业之后，`export_link` 的
//! `kind != Export` 守卫就形同虚设，平台会给一份根本没产生的产物签发下载地址。
//! 这一组测试全部走公开 API，用真 SQL 造出损坏行，断言**必须报错**。

use rusqlite::Connection;
use wxlike_identity::preferences::{self, JobKind, JobStatus};

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory sqlite");
    wxlike_identity::migrate(&conn).expect("identity migrate");
    conn
}

/// 直接写原文：这些行模拟"迁移/人工/别的写入方留下的坏数据"，
/// 不是任何公开 API 能产生的形状——正是必须失败的那一类。
fn insert_raw_job(conn: &Connection, id: &str, kind: &str, status: &str) {
    conn.execute(
        "INSERT INTO identity_job(id, user_id, kind, status, requested_at, artifact, bytes, error) \
         VALUES(?1, 'u1', ?2, ?3, 1000, '', 0, '')",
        rusqlite::params![id, kind, status],
    )
    .expect("insert raw job");
}

#[test]
fn unknown_kind_is_an_error_not_an_export() {
    let conn = db();
    insert_raw_job(&conn, "j1", "deleet", "queued");

    let err = preferences::job(&conn, "j1").expect_err("坏 kind 必须失败，不能折成 Export");
    assert!(
        err.to_string().contains("identity_job.kind"),
        "错误要指到具体列：{err}"
    );

    // list 也不能放过它（作业列表页读的就是这个）
    let err = preferences::list(&conn, "u1").expect_err("list 同样不得折默认值");
    assert!(err.to_string().contains("identity_job.kind"), "{err}");
}

#[test]
fn unknown_status_is_an_error_not_a_queue() {
    let conn = db();
    // 'don'：一个字母的 typo 在旧实现里读成 Queued，进度条从此永不推进
    insert_raw_job(&conn, "j1", "export", "don");
    let err = preferences::job(&conn, "j1").expect_err("坏 status 必须失败");
    assert!(err.to_string().contains("identity_job.status"), "{err}");
}

#[test]
fn deleting_a_corrupt_job_cannot_be_mistaken_for_a_downloadable_export() {
    let conn = db();
    // 旧代码的真实事故形状：kind 写成 'deleet' 的注销作业，读出来是 Export，
    // 于是 claim→finish→export_link 一路畅通，给一份不存在的产物签了下载链接。
    insert_raw_job(&conn, "j1", "deleet", "queued");

    let claimed = preferences::claim(&conn, 1_100);
    assert!(
        claimed.is_err(),
        "worker 不能把坏行领走：{:?}",
        claimed.map(|j| j.map(|j| (j.id, j.kind, j.status)))
    );

    // 没被改坏：它仍然是 queued，等人工处置，而不是"正在跑的导出"
    let raw: String = conn
        .query_row("SELECT status FROM identity_job WHERE id = 'j1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(raw, "queued", "验原文失败时不得先改状态");
}

#[test]
fn well_known_values_still_round_trip() {
    let conn = db();
    let job = preferences::request_export(&conn, "u1", 1_000).unwrap();
    assert_eq!(job.kind, JobKind::Export);
    assert_eq!(job.status, JobStatus::Queued);
    preferences::claim(&conn, 1_100).unwrap().expect("能领走");
    preferences::finish(&conn, &job.id, "/tmp/a.zip", 10, 1_200).unwrap();
    let done = preferences::job(&conn, &job.id).unwrap().unwrap();
    assert_eq!(done.status, JobStatus::Done);
    assert_eq!(done.artifact, "/tmp/a.zip");

    let del = preferences::request_delete(&conn, "u2", 1_300).unwrap();
    assert_eq!(del.kind, JobKind::Delete);
    assert_eq!(
        preferences::job(&conn, &del.id).unwrap().unwrap().kind,
        JobKind::Delete
    );
}

#[test]
fn head_seq_reports_a_broken_database_instead_of_zero() {
    let conn = db();
    // 正常的空账号：0 是真答案
    assert_eq!(preferences::head_seq(&conn, "u1").unwrap(), 0);

    // 表没了：旧实现 .unwrap_or(0) 与"没有偏好"完全同形，客户端会把游标
    // 重置到 0 并静默漏掉之后所有增量
    conn.execute("DROP TABLE identity_pref", []).unwrap();
    let err = preferences::head_seq(&conn, "u1").expect_err("读库失败必须报错");
    assert!(
        matches!(err, wxlike_identity::IdError::Db(_)),
        "应当是库错误而不是编造的状态：{err:?}"
    );
}

#[test]
fn patch_and_read_survive_a_head_seq_failure() {
    // 写路径本来就会传播 `?`；这里守的是"写进去的值立刻读得出来"，
    // 让 GET /api/preferences 的 head_seq 有真实游标可用。
    let conn = db();
    let mut patch = std::collections::BTreeMap::new();
    patch.insert("notify.dm".to_string(), "off".to_string());
    preferences::patch(&conn, "u1", &patch, "dev-a", 1_000).unwrap();
    assert_eq!(preferences::head_seq(&conn, "u1").unwrap(), 1);
    assert_eq!(
        preferences::get(&conn, "u1")
            .unwrap()
            .get_bool("notify.dm", true),
        false,
        "关掉的开关不能被读成开"
    );
}
