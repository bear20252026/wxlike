//! G00-04 的 PG 覆盖守卫：SQLite 快照里的每张表/视图，都必须在
//! `schema/proposals/**/*.sql` 里有对应的 PG `CREATE TABLE` / `CREATE VIEW`。
//!
//! 这不能替代 PG 实跑（列类型、约束、触发器仍由 `pg-migration` 断言），
//! 但能在本地立刻抓住“新表只加进 SQLite，忘了写 PG 迁移”的漂移。
//!
//! `users` / `sessions` / `media` 是 M011/M012 的兼容视图，解析器同样接受
//! `CREATE OR REPLACE VIEW`。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const SQLITE_SNAPSHOT: &str = include_str!("schema_snapshot.txt");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn collect_sql(dir: &Path, out: &mut String) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|err| panic!("读不到 {}：{err}", dir.display()));
    for entry in entries {
        let entry = entry.expect("目录项");
        let path = entry.path();
        if path.is_dir() {
            collect_sql(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("sql") {
            out.push_str(
                &std::fs::read_to_string(&path)
                    .unwrap_or_else(|err| panic!("读不到 {}：{err}", path.display())),
            );
            out.push('\n');
        }
    }
}

/// 去掉语句开头连续的 `--` 注释行，避免把注释里的 `CREATE` 当成对象。
fn strip_leading_comments(mut sql: &str) -> &str {
    loop {
        let trimmed = sql.trim_start();
        if let Some(rest) = trimmed.strip_prefix("--") {
            sql = rest.split_once('\n').map_or("", |(_, rest)| rest);
        } else {
            return trimmed;
        }
    }
}

fn object_name(token: &str) -> Option<String> {
    let name = token
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
        .trim_end_matches('(');
    if name.is_empty() {
        return None;
    }
    Some(name.rsplit('.').next().unwrap_or(name).to_ascii_lowercase())
}

fn collect_pg_objects(sql: &str) -> BTreeSet<String> {
    let mut objects = BTreeSet::new();
    for raw in sql.split(';') {
        let statement = strip_leading_comments(raw);
        let mut words = statement.split_whitespace();
        if !words
            .next()
            .is_some_and(|word| word.eq_ignore_ascii_case("create"))
        {
            continue;
        }
        let mut kind = None;
        for word in words.by_ref() {
            let lower = word.to_ascii_lowercase();
            match lower.as_str() {
                "or" | "replace" | "unlogged" | "materialized" => continue,
                "table" | "view" => {
                    kind = Some(lower);
                    break;
                }
                _ => break,
            }
        }
        if kind.is_none() {
            continue;
        }
        let mut saw_if = false;
        for word in words.by_ref() {
            let lower = word.to_ascii_lowercase();
            if !saw_if && lower == "if" {
                saw_if = true;
                continue;
            }
            if saw_if && (lower == "not" || lower == "exists") {
                continue;
            }
            if let Some(name) = object_name(word) {
                objects.insert(name);
            }
            break;
        }
    }
    objects
}

fn sqlite_tables() -> BTreeSet<String> {
    SQLITE_SNAPSHOT
        .lines()
        .filter_map(|line| line.split_once('.').map(|(table, _)| table.to_string()))
        .filter(|table| !table.is_empty())
        .collect()
}

#[test]
fn every_sqlite_table_or_view_has_a_pg_object() {
    let root = repo_root();
    let mut sql = String::new();
    collect_sql(&root.join("schema/proposals"), &mut sql);
    let pg = collect_pg_objects(&sql);
    assert!(
        pg.len() > 100,
        "只解析出 {} 个 PG 对象，解析器可能漏了 CREATE 语句",
        pg.len()
    );

    let missing: Vec<String> = sqlite_tables()
        .into_iter()
        .filter(|table| !pg.contains(table))
        .collect();
    assert!(
        missing.is_empty(),
        "SQLite 快照里有、PG proposals 里没有对应 CREATE TABLE/VIEW：{missing:?}"
    );
}

#[test]
fn the_parser_understands_if_not_exists_and_or_replace() {
    let objects = collect_pg_objects(
        "-- 注释里的 CREATE TABLE 不算\n\
         CREATE TABLE IF NOT EXISTS plain_table (id TEXT);\n\
         CREATE OR REPLACE VIEW plain_view AS SELECT 1;\n\
         CREATE UNLOGGED TABLE unlogged_table (id TEXT);\n",
    );
    assert!(objects.contains("plain_table"));
    assert!(objects.contains("plain_view"));
    assert!(objects.contains("unlogged_table"));
}
