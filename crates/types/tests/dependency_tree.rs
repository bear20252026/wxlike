//! 依赖树护栏：客户端契约层**不许**长进服务端。
//!
//! 验收要求："客户端 crate 依赖树不含 axum 或生产数据库"。这条规则靠人守
//! 是守不住的——某个域想复用一段服务端代码，顺手加一条 path 依赖，
//! `cargo build` 照样绿，只是手机的 .so 里多了一个 Web 框架和一套 PostgreSQL
//! 驱动（`crates/net` 的历史教训：QUIC 客户端与 hub 在同一个 crate，
//! 链接它等于把 axum 塞进 APK）。
//!
//! 这个测试不跑 `cargo tree`（会和 `cargo test` 抢 target 目录的锁），
//! 而是直接读 manifest：从 G01 的 crate 出发，沿 `path = "..."` 走闭包，
//! 断言闭包里没有任何被禁依赖。

use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

/// 禁止出现在客户端依赖闭包里的 crate。
///
/// 判据是"服务端专属"：HTTP 框架、ORM / 迁移工具、PostgreSQL 驱动、
/// 消息队列客户端。设备本地的 SQLite（`rusqlite`）不在其中——那是客户端
/// 自己的库，不是生产数据库。
const FORBIDDEN: [&str; 9] = [
    "axum",
    "sqlx",
    "diesel",
    "sea-orm",
    "tokio-postgres",
    "postgres",
    "deadpool-postgres",
    "rdkafka",
    "lapin",
];

/// G01 归属的 crate 目录（不存在的跳过：卡是按编号串行落地的）。
const CLIENT_CRATES: [&str; 5] = [
    "crates/types",
    "crates/client-core",
    "crates/client-native",
    "crates/client-web",
    "crates/ffi",
];

fn workspace_root() -> PathBuf {
    // crates/types/tests/xxx.rs → ../../..
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace 根目录")
        .to_path_buf()
}

/// manifest 里一条依赖声明。
#[derive(Debug, Default)]
struct Decl {
    name: String,
    path: Option<PathBuf>,
}

/// 从 manifest 文本里抽出 `[dependencies*]` 段的依赖（跳过 dev-dependencies：
/// 它们不进产物）。
fn parse_dependencies(text: &str) -> Vec<Decl> {
    let mut out = Vec::new();
    let mut in_deps = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = is_runtime_deps_section(trimmed);
            continue;
        }
        if !in_deps || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, rest)) = trimmed.split_once('=') else {
            continue;
        };
        out.push(Decl {
            name: name.trim().to_string(),
            path: extract_path(rest),
        });
    }
    out
}

fn is_runtime_deps_section(header: &str) -> bool {
    // 覆盖 `[dependencies]`、`[dependencies.foo]`、`[target.'cfg(unix)'.dependencies]`
    let lower = header.to_ascii_lowercase();
    lower.contains("dependencies") && !lower.contains("dev-dependencies")
}

fn extract_path(rest: &str) -> Option<PathBuf> {
    let marker = "path = \"";
    let start = rest.find(marker)? + marker.len();
    let tail = &rest[start..];
    let end = tail.find('"')?;
    Some(PathBuf::from(&tail[..end]))
}

/// 沿 path 依赖走闭包，返回（被禁依赖命中, 闭包里的 crate 目录）。
fn walk(roots: &[PathBuf]) -> (Vec<String>, BTreeSet<String>) {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<PathBuf> = roots.iter().cloned().collect();
    let mut hits = Vec::new();
    while let Some(dir) = queue.pop_front() {
        let key = dir.to_string_lossy().replace('\\', "/");
        if !seen.insert(key) {
            continue;
        }
        let manifest = dir.join("Cargo.toml");
        let Ok(text) = fs::read_to_string(&manifest) else {
            continue;
        };
        for decl in parse_dependencies(&text) {
            if FORBIDDEN.contains(&decl.name.as_str()) {
                hits.push(format!("{} → {}", dir.display(), decl.name));
            }
            if let Some(path) = decl.path {
                queue.push_back(normalize(&dir, &path));
            }
        }
    }
    (hits, seen)
}

fn normalize(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let joined = base.join(path);
    // 只做 `..` 的文本化简，不碰文件系统
    let mut parts: Vec<String> = Vec::new();
    for part in joined.to_string_lossy().replace('\\', "/").split('/') {
        match part {
            "." | "" => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other.to_string()),
        }
    }
    PathBuf::from(parts.join("/"))
}

#[test]
fn client_crates_do_not_pull_server_or_database_dependencies() {
    let root = workspace_root();
    let roots: Vec<PathBuf> = CLIENT_CRATES
        .iter()
        .map(|rel| root.join(rel))
        .filter(|dir| dir.join("Cargo.toml").is_file())
        .collect();
    assert!(!roots.is_empty(), "一个 G01 crate 都没找到，路径写错了？");
    let (hits, closure) = walk(&roots);
    assert!(
        hits.is_empty(),
        "客户端依赖闭包混进了服务端依赖：{hits:?}\n闭包：{closure:?}"
    );
    assert!(
        !closure
            .iter()
            .any(|dir| dir.ends_with("/server") || dir.contains("crates/api")),
        "客户端依赖闭包里出现了服务端 crate：{closure:?}"
    );
}

#[test]
fn types_stays_a_leaf_crate() {
    // 契约层是所有人共用的，它一旦依赖了某个域，所有域就都被它拖着走。
    let root = workspace_root();
    let text = fs::read_to_string(root.join("crates/types/Cargo.toml")).expect("读 manifest");
    let internal: Vec<String> = parse_dependencies(&text)
        .into_iter()
        .filter(|d| d.path.is_some())
        .map(|d| d.name)
        .collect();
    assert!(
        internal.is_empty(),
        "crates/types 的运行时依赖里出现了内部 crate：{internal:?}"
    );
}
