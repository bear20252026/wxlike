//! M009 的登记一致性：`schema/registry/migrations.json` 是服务端 SQLite 迁移
//! **唯一编号来源**（G00-03 #101 交付的机制）。media 域此前有 DDL、有建表执行点，
//! 但没编号 —— 也就是"表在跑，账上没有"（`docs/feature-reachability.md` 的
//! REQ-G00-migration-number，`integration-requests/G04/G04-01.yaml:26`）。
//!
//! 这几条测试把账与事实钉在一起：
//! 1. media 的 DDL 有且只有一个登记编号，且 `next_id` 已前移（不许有人复用）；
//! 2. 登记表点名的那份 SQL **就是** `db::SCHEMA_SQL` 实际执行的内容（防漂移）；
//! 3. 全新的盘上库开一次就有 9 张表，重开不报错（`IF NOT EXISTS` 可重入）。

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;
use wxlike_media::db::SCHEMA_SQL;

/// media 域的正式编号（G00-295 / issue #295）。
const MEDIA_ID: &str = "M009";
/// 登记后 `next_id` 应当指向的下一个编号：编号永不复用、永不重排。
const NEXT_ID: &str = "M010";
/// M009 应当建的 9 张表。
const MEDIA_TABLES: &[&str] = &[
    "media_object",
    "media_ref",
    "media_upload",
    "media_upload_chunk",
    "media_file",
    "media_share",
    "media_job",
    "media_backup",
    "media_backup_entry",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .to_path_buf()
}

fn registry() -> Value {
    let path = repo_root().join("schema/registry/migrations.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("读不到 {}：{err}", path.display()));
    serde_json::from_str(&raw).expect("migrations.json 必须是合法 JSON")
}

/// 登记表里指向 media 域的那一条。
fn media_entry(registry: &Value) -> &Value {
    let entries = registry["migrations"]
        .as_array()
        .expect("migrations 是数组");
    let hits: Vec<&Value> = entries
        .iter()
        .filter(|m| {
            m["module"]
                .as_str()
                .unwrap_or_default()
                .contains("crates/features/media")
        })
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "media 域应当恰好登记一次（多个编号 = 两套事实）"
    );
    hits[0]
}

#[test]
fn media_schema_is_registered_with_one_real_migration_number() {
    let registry = registry();
    let entry = media_entry(&registry);
    assert_eq!(entry["id"], MEDIA_ID, "media 的编号");
    assert_eq!(registry["next_id"], NEXT_ID, "next_id 必须已前移");
    assert!(
        entry["description"]
            .as_str()
            .unwrap_or_default()
            .contains("IF NOT EXISTS"),
        "登记要写明可重入（registry policy.idempotency）"
    );
}

#[test]
fn registered_numbers_are_unique_and_monotonic() {
    let entries = registry()["migrations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    let mut parsed: Vec<u32> = entries
        .iter()
        .map(|id| {
            id.strip_prefix('M')
                .unwrap_or_else(|| panic!("编号形状应为 M###: {id}"))
                .parse()
                .unwrap_or_else(|_| panic!("编号应为数字: {id}"))
        })
        .collect();
    let mut sorted = parsed.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), parsed.len(), "编号不得重复: {entries:?}");
    parsed.sort_unstable();
    for (index, value) in parsed.iter().enumerate() {
        assert_eq!(
            *value,
            index as u32 + 1,
            "编号必须连续且从 M001 起（跳号=有人复用了空位）: {entries:?}"
        );
    }
    let next: u32 = registry()["next_id"]
        .as_str()
        .unwrap()
        .strip_prefix('M')
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(next, parsed.len() as u32 + 1, "next_id 指向下一位");
}

/// 登记表点名的 SQL 与实际执行的那份必须是同一份字节：
/// 谁把 DDL 复制进 crate（或反过来），这条立刻红。
#[test]
fn registered_ddl_is_the_ddl_that_actually_runs() {
    let registry = registry();
    let entry = media_entry(&registry);
    let module = entry["module"].as_str().unwrap();
    let file = module
        .split('=')
        .nth(1)
        .unwrap_or_else(|| panic!("module 应写成 '<执行点> = <DDL 文件>': {module}"))
        .trim();
    let path = repo_root().join(file);
    let on_disk = std::fs::read(&path)
        .unwrap_or_else(|err| panic!("读不到登记的 DDL {}：{err}", path.display()));
    assert_eq!(
        on_disk.as_slice(),
        SCHEMA_SQL.as_bytes(),
        "{} 与 embed 进 crate 的 SCHEMA_SQL 不一致",
        path.display()
    );
}

/// 全新盘上库开一次 = M009 落地；重开不报错（可重入）。
#[test]
fn a_fresh_database_gets_all_media_tables() {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-media-m009-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("media.db");
    let path = db.to_str().unwrap().to_string();

    // 1) 直接按登记的 SQL 建一遍，并复跑一次（重入必须安全）。
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(SCHEMA_SQL).unwrap();
    assert_tables(&conn);
    conn.execute_batch(SCHEMA_SQL).unwrap();
    assert_tables(&conn);
    drop(conn);

    // 2) 真正的执行点：`MediaService::open` 走的 `MediaDb::open`。
    //    表已经在，重开只能报"一切正常"，不能报"表已存在"。
    let service = wxlike_media::MediaService::open(
        wxlike_media::MediaConfig::new(dir.join("objects")),
        Some(&path),
        std::sync::Arc::new(DenyAll),
    )
    .expect("MediaService::open 必须在已有 M009 的库上跑通");
    // 服务能读写这张表 = 它连的就是同一个库（不是又一个内存库）。
    let session = service
        .upload_start(
            "u1",
            wxlike_media::UploadStart {
                purpose: wxlike_media::Purpose::File,
                name: "m009.txt".into(),
                size: 3,
                mime: "text/plain".into(),
                digest: wxlike_media::ids::sha256_hex(b"doc"),
            },
        )
        .unwrap();
    drop(session);

    let conn = Connection::open(&path).unwrap();
    let uploads: i64 = conn
        .query_row("SELECT COUNT(*) FROM media_upload", [], |r| r.get(0))
        .unwrap();
    assert_eq!(uploads, 1, "M009 的执行点要落在登记的那个库里");
    assert_tables(&conn);
    std::fs::remove_dir_all(&dir).ok();
}

fn assert_tables(conn: &Connection) {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    let tables: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for expected in MEDIA_TABLES {
        assert!(
            tables.iter().any(|t| t == expected),
            "缺表 {expected}；实际：{tables:?}"
        );
    }
}

/// 成员事实一律拒绝：本文件只关心建表，不关心归属判定。
struct DenyAll;

impl wxlike_media::AudiencePort for DenyAll {
    fn may_read_conversation(&self, _: &str, _: &str) -> bool {
        false
    }
    fn may_read_group(&self, _: &str, _: &str) -> bool {
        false
    }
}
