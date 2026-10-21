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
//! 格式刻意是**纯文本、一行一列**，不是 JSON：
//! `serde_json::Map` 的键序受 `preserve_order` feature 影响，而 feature 由
//! 整个依赖图统一决定——同一份代码在 Windows 与 Linux 上可能序列化出不同
//! 顺序（这次就踩了：Windows CI 红，Linux 绿）。快照的价值在"可比"，
//! 任何依赖序列化实现细节的格式都是陷阱。
//!
//! 只记录**结构**（列名/类型/非空/主键），不记录索引与触发器——那些由
//! `IF NOT EXISTS` 保证幂等、改动频率低；纳入快照只会让 diff 难读。

/// 读出 `表.列 类型 非空 主键` 的排序行，保证跨平台/跨次运行逐字节可比。
fn dump_schema(path: &str) -> String {
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

    let mut out = String::new();
    for table in tables {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let cols: Vec<(String, String, i64, i64)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(1)?, // name
                    r.get::<_, String>(2)?, // type
                    r.get::<_, i64>(3)?,    // notnull
                    r.get::<_, i64>(5)?,    // pk
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        // PRAGMA 的列序就是建表序，不改；表之间按名字排序。
        for (name, ty, notnull, pk) in cols {
            out.push_str(&format!("{table}.{name} {ty} notnull={notnull} pk={pk}\n"));
        }
    }
    out
}

fn snapshot_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/schema_snapshot.txt")
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

    if std::env::var("WXLIKE_UPDATE_SCHEMA").is_ok() {
        std::fs::write(snapshot_path(), &actual).expect("write snapshot");
        eprintln!("快照已更新：{}", snapshot_path().display());
        drop(platform);
        std::fs::remove_dir_all(&dir).ok();
        return;
    }

    // 换行必须归一化：Windows 的 git 检出会把快照转成 CRLF，而 `dump_schema`
    // 生成的是 LF。比较"结构"时不该被行尾符号判红。
    let expected = std::fs::read_to_string(snapshot_path())
        .unwrap_or_else(|_| {
            panic!(
                "缺少快照 {}；用 WXLIKE_UPDATE_SCHEMA=1 生成",
                snapshot_path().display()
            )
        })
        .replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "表结构变了：确认改动无误后跑 `WXLIKE_UPDATE_SCHEMA=1 cargo test -p wxlike-platform --test schema_snapshot`，\
         并把快照一起提交"
    );

    drop(platform);
    std::fs::remove_dir_all(&dir).ok();
}
