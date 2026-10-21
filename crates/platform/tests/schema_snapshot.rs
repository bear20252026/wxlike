//! 表结构快照：**改表必须同步改快照**。
//!
//! 为什么需要它：库里 40+ 张表，加一列、改个默认值，没有任何机制拦得住。
//! 迁移代码是 `IF NOT EXISTS` 的，悄悄加列在生产上不会报错，只会在某天
//! 有人按老结构写 SQL 时炸。快照是一份可 diff 的事实：
//!
//! - 改了表却没改快照 → CI 红；
//! - 改了快照 → 评审时一眼看到"这次动了哪张表、哪一列"。
//!
//! 更新方式（确认改动无误后）：
//!
//! ```bash
//! WXLIKE_UPDATE_SCHEMA=1 cargo test -p wxlike-platform --test schema_snapshot
//! ```
//!
//! 只记录**结构**（列名/类型/非空/主键），不记录索引与触发器——那些由
//! `IF NOT EXISTS` 保证幂等，且改动频率低；把它们也纳入快照会让 diff 变得
//! 难读，反而没人看。

use serde_json::{json, Map, Value};

fn snapshot_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/schema_snapshot.json")
}

/// 读出 `表名 → 列定义`，全部排序，保证跨平台/跨次运行可比。
fn dump_schema(path: &str) -> Value {
    let conn = rusqlite::Connection::open(path).expect("open platform db");
    let mut tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    tables.sort();

    let mut out = Map::new();
    for table in tables {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let cols: Vec<Value> = stmt
            .query_map([], |r| {
                Ok(json!({
                    "name": r.get::<_, String>(1)?,
                    "type": r.get::<_, String>(2)?,
                    "notnull": r.get::<_, i64>(3)?,
                    "pk": r.get::<_, i64>(5)?,
                }))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        out.insert(table, Value::Array(cols));
    }
    Value::Object(out)
}

#[test]
fn schema_matches_the_committed_snapshot() {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-schema-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("platform.db");
    let db_str = db.to_str().unwrap().to_string();

    // 打开一次就会跑完全部迁移；连接保持到 dump 结束，避免 WAL 收尾的时序问题。
    let platform = wxlike_platform::Platform::open_with(
        Some(&db_str),
        wxlike_platform::PlatformOptions {
            pbkdf2_rounds: 1_000,
            ..Default::default()
        },
    )
    .expect("platform opens and migrates");

    let actual = dump_schema(&db_str);
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&actual).expect("serialise schema")
    );

    if std::env::var("WXLIKE_UPDATE_SCHEMA").is_ok() {
        std::fs::write(snapshot_path(), &text).expect("write snapshot");
        eprintln!("快照已更新：{}", snapshot_path().display());
        drop(platform);
        return;
    }

    let expected = std::fs::read_to_string(snapshot_path()).unwrap_or_else(|_| {
        panic!(
            "缺少快照 {}；用 WXLIKE_UPDATE_SCHEMA=1 生成",
            snapshot_path().display()
        )
    });
    assert_eq!(
        text, expected,
        "表结构变了：确认改动无误后跑 `WXLIKE_UPDATE_SCHEMA=1 cargo test -p wxlike-platform --test schema_snapshot`，\
         并把快照一起提交"
    );

    drop(platform);
    std::fs::remove_dir_all(&dir).ok();
}
