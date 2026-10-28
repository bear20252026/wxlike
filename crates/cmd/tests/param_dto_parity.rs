//! **contract-v1 参数比对**：catalog 声明的 `Param` 名 ↔ Executor handler 实际取用的字段名。
//!
//! 这条测试补的是 `integration-requests/G15/G15-01.yaml` 的
//! `REQ-G01-05-dto-parity` 里那句"**尚无自动比对**"：catalog 用 `Param { name }`
//! 声明参数，`crates/client` 的 handler 用 `str_param(p, "…")` / `p["…"]` /
//! `p.get("…")` 取参数，两边各写一处，改一边忘另一边就是
//! "schema 说支持、代码读不到"的漂移。
//!
//! ## 为什么是**读源码**而不是读类型
//! 执行器没有 per-command 的 Rust DTO——它直接在 `serde_json::Value` 上取字段。
//! 所以"DTO 字段名"这份真相只存在于 handler 的源码文本里。本测试把
//! `crates/client/src/execute.rs` 与 `crates/client/src/execute/handlers.rs`
//! **只读**地按词法切一遍（字符串/注释/换行都走 token，不是正则糊上去），
//! 再与 catalog 的声明逐条比对。这样任一侧改名都会在 CI 里红，
//! 而且不需要给 `crates/client` 加任何东西（那张写路径不归本卡）。
//!
//! 词法器的已知边界：不处理原始字符串 `r#"…"#` 与块注释。
//! 这两样出现在执行器源码里时会把条目抽少，由
//! [`every_catalog_command_has_exactly_one_handler`] 立刻判红，不会静默通过。
//!
//! ## 两个方向都必须成立
//! 1. handler 取用的字段，catalog 必须声明（否则调用方按 schema 传不进来）；
//! 2. catalog 声明的参数，必须真的到达 handler——除非落在下面两张**写明理由**的名单里。
//!
//! 名单是**双向断言**：条目失效（参数改名走了 / 已经接上线）同样红，
//! 免得豁免变成永久藏身处。
//!
//! ## 注入证明
//! 把 `msg.get` 的 `msg_id` 改名（含 `params` 说明）→ 方向 1 红；
//! 把 `group.members` 的 `after_user` 改名 → 方向 2 与名单失效同时红。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use wxlike_cmd::CATALOG;

/// 执行器侧的源文件（本卡只读不改）。历史上分派表在 `execute.rs`，
/// 现在在 `execute/handlers.rs`——两处都扫，搬家不会把比对扫空。
const EXECUTOR_SOURCES: [&str; 2] = [
    "crates/client/src/execute.rs",
    "crates/client/src/execute/handlers.rs",
];

/// 声明了、但**由确认层而非 handler 消费**的参数。
/// 令牌在 `wxlike_cmd::confirmation` 里就被摘掉（`strip_token`），MCP 侧走
/// `confirm_step`，CLI 侧走 `--confirm`：执行器永远看不到这个键。
const CONSUMED_ELSEWHERE: [(&str, &str); 3] = [
    ("msg.get", "confirmation_token"),
    ("group.kick", "confirmation_token"),
    ("msg.recall", "confirmation_token"),
];

/// 声明了、但 handler **还没接线**的参数：已知缺口，不是豁免的常态。
/// 服务端 `/api/groups/{conv}/members` 不接游标与条数；`group.add_member`
/// 的幂等键没进请求；`msg.send` 的 `reply_to` 也没进请求体——
/// 也就是说"引用回复"今天在命令面上是个空承诺。
/// 这三处都**归执行器/服务端**（#314 的边界不许改 Executor 分派），所以先登记；
/// 哪天接上了，名单条目失效 → 测试红 → 删条目即可；参数改名同样红，
/// 缺口不会静默扩大。
const NOT_WIRED_YET: [(&str, &str); 4] = [
    ("group.members", "after_user"),
    ("group.members", "limit"),
    ("group.add_member", "idempotency_key"),
    ("msg.send", "reply_to"),
];

/// 一条 handler 的比对素材。
#[derive(Debug)]
struct HandlerEntry {
    /// 表里的命令 id。
    id: String,
    /// catalog 侧声明的参数名（catalog 顺序）。
    declared: Vec<&'static str>,
    /// 源码里取用到的字段名。
    reads: BTreeSet<String>,
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/cmd 的上两级必须是 workspace 根")
        .to_path_buf()
}

fn read_executor_source(rel: &str) -> String {
    let path = workspace_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不到执行器源码 {}：{e}", path.display()))
}

// ---------------------------------------------------------------- 词法

/// 只分三类：字符串、标识符、单字符标点。字符串**整体**是一个 token，
/// 所以大括号配对不会被 `format!("…{conv}")` 里的内容骗到。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Str(String),
    Ident(String),
    Punct(char),
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

fn tokenize(src: &str) -> Vec<Tok> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '"' {
            let (s, next) = quoted(&b, i + 1);
            out.push(Tok::Str(s));
            i = next;
            continue;
        }
        if c == '\'' {
            // 闭引号在 2～3 位才是字符字面量，否则是生命周期标注
            let is_char = matches!(b.get(i + 2), Some('\''))
                || (b.get(i + 1) == Some(&'\\') && b.get(i + 3) == Some(&'\''));
            if is_char {
                let (s, next) = quoted(&b, i + 1);
                out.push(Tok::Ident(s));
                i = next;
                continue;
            }
            i += 1;
            continue;
        }
        if is_ident_start(c) {
            let start = i;
            while i < b.len() && is_ident_char(b[i]) {
                i += 1;
            }
            out.push(Tok::Ident(b[start..i].iter().collect()));
            continue;
        }
        if !c.is_whitespace() {
            out.push(Tok::Punct(c));
        }
        i += 1;
    }
    out
}

/// 从开引号之后扫到闭引号，处理 `\` 转义。
fn quoted(b: &[char], mut i: usize) -> (String, usize) {
    let mut s = String::new();
    while i < b.len() {
        match b[i] {
            '"' | '\'' => return (s, i + 1),
            '\\' => {
                if let Some(esc) = b.get(i + 1) {
                    s.push(*esc);
                }
                i += 2;
            }
            c => {
                s.push(c);
                i += 1;
            }
        }
    }
    (s, i)
}

// ---------------------------------------------------------------- token 访问

fn tok_str(toks: &[Tok], i: usize) -> Option<&str> {
    match toks.get(i)? {
        Tok::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

fn tok_ident(toks: &[Tok], i: usize) -> Option<&str> {
    match toks.get(i)? {
        Tok::Ident(s) => Some(s.as_str()),
        _ => None,
    }
}

fn is_punct(toks: &[Tok], i: usize, c: char) -> bool {
    matches!(toks.get(i), Some(Tok::Punct(p)) if *p == c)
}

// ---------------------------------------------------------------- 抽取

/// 从 `open`（指向 `(`）往后找配对的 `)`；返回闭括号的索引。
fn matching_close(toks: &[Tok], open: usize) -> usize {
    let mut depth = 0usize;
    for (i, t) in toks.iter().enumerate().skip(open) {
        if matches!(t, Tok::Punct('(') | Tok::Punct('{') | Tok::Punct('[')) {
            depth += 1;
        } else if matches!(t, Tok::Punct(')') | Tok::Punct('}') | Tok::Punct(']')) {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
    }
    toks.len().saturating_sub(1)
}

/// 闭包参数（`|ex, http, p|`）的第三个绑定名——参数对象的那个名字。
/// 签名不是三段就说明分派表换了形状，返回 `None` 让调用方跳过。
fn closure_binding(toks: &[Tok], from: usize) -> Option<(String, usize)> {
    if !is_punct(toks, from, '|') {
        return None;
    }
    let mut names = Vec::new();
    let mut i = from + 1;
    while i < toks.len() && !is_punct(toks, i, '|') {
        if is_punct(toks, i, ';') {
            return None;
        }
        if let Some(name) = tok_ident(toks, i) {
            names.push(name.to_string());
        }
        i += 1;
    }
    if names.len() != 3 {
        return None;
    }
    Some((names[2].clone(), i))
}

/// 一段 handler 里所有"从参数对象取字段"的键名。
/// 认三种写法：`str_param(p, "k")`、`p["k"]`、`p.get("k")`。
fn reads_in(toks: &[Tok], binding: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for i in 0..toks.len() {
        // str_param(p, "k")
        if tok_ident(toks, i) == Some("str_param")
            && is_punct(toks, i + 1, '(')
            && tok_ident(toks, i + 2) == Some(binding)
            && is_punct(toks, i + 3, ',')
        {
            if let Some(k) = tok_str(toks, i + 4) {
                out.insert(k.to_string());
            }
            continue;
        }
        if tok_ident(toks, i) != Some(binding) {
            continue;
        }
        // p["k"]
        if is_punct(toks, i + 1, '[') {
            if let Some(k) = tok_str(toks, i + 2) {
                out.insert(k.to_string());
            }
            continue;
        }
        // p.get("k")
        if is_punct(toks, i + 1, '.')
            && tok_ident(toks, i + 2) == Some("get")
            && is_punct(toks, i + 3, '(')
        {
            if let Some(k) = tok_str(toks, i + 4) {
                out.insert(k.to_string());
            }
        }
    }
    out
}

/// 扫执行器源码，抽出 `("id", |ex, http, p| { … })` 形态的分派表条目。
fn collect_entries(rel: &str) -> Vec<HandlerEntry> {
    let src = read_executor_source(rel);
    let toks = tokenize(&src);
    let mut out = Vec::new();
    for i in 0..toks.len() {
        if !is_punct(&toks, i, '(')
            || tok_str(&toks, i + 1).is_none()
            || !is_punct(&toks, i + 2, ',')
        {
            continue;
        }
        let id = tok_str(&toks, i + 1).expect("just checked").to_string();
        let Some((binding, bar)) = closure_binding(&toks, i + 3) else {
            continue;
        };
        let end = matching_close(&toks, i);
        let reads = reads_in(&toks[bar + 1..end], &binding);
        let declared = CATALOG
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.args.iter().map(|p| p.name).collect())
            .unwrap_or_default();
        out.push(HandlerEntry {
            id,
            declared,
            reads,
        });
    }
    out
}

fn executor_entries() -> Vec<HandlerEntry> {
    EXECUTOR_SOURCES
        .iter()
        .flat_map(|rel| collect_entries(rel))
        .collect()
}

fn waived(id: &str, name: &str) -> bool {
    waiver_pairs().any(|(a, b)| *a == id && *b == name)
}

/// `BTreeSet<String>` 上按 `&str` 查，写起来总绕；收成一个函数。
fn read_p(set: &BTreeSet<String>, name: &str) -> bool {
    set.iter().any(|r| r == name)
}

fn waiver_pairs() -> impl Iterator<Item = &'static (&'static str, &'static str)> {
    CONSUMED_ELSEWHERE.iter().chain(NOT_WIRED_YET.iter())
}

// ---------------------------------------------------------------- 断言

/// 比对的前提是"确实扫到了东西"：抽到 0 条等于把测试变成常绿。
#[test]
fn executor_dispatch_table_is_actually_found() {
    let entries = executor_entries();
    assert!(
        entries.len() >= CATALOG.len(),
        "从执行器源码里只抽到 {} 条分派条目（catalog 有 {} 条）——多半是 handler 的写法变了\
         （不再是 `(\"id\", |ex, http, p| …)`）或分派表搬了家，比对已经静默失效，\
         必须同步更新这个抽取器与 EXECUTOR_SOURCES",
        entries.len(),
        CATALOG.len()
    );
}

#[test]
fn every_catalog_command_has_exactly_one_handler() {
    let entries = executor_entries();
    for spec in CATALOG {
        let hits = entries.iter().filter(|e| e.id == spec.id).count();
        assert_eq!(hits, 1, "{} 在执行器分派表里出现 {hits} 次", spec.id);
    }
    let stray: Vec<&str> = entries
        .iter()
        .map(|e| e.id.as_str())
        .filter(|id| CATALOG.iter().all(|s| s.id != *id))
        .collect();
    assert!(stray.is_empty(), "执行器里有 catalog 不认的 id：{stray:?}");
}

/// 方向 1：**handler 取用的字段，catalog 必须声明**。
/// 这条严格到没有名单可躲——改任何一侧的名字都会在这里红。
#[test]
fn handler_reads_are_declared_in_the_catalog() {
    let mut drift = Vec::new();
    for e in executor_entries() {
        for read in &e.reads {
            if !e.declared.iter().any(|d| d == read) {
                drift.push(format!(
                    "{}: handler 取用了 {read:?}，catalog 声明的是 {:?}",
                    e.id, e.declared
                ));
            }
        }
    }
    assert!(drift.is_empty(), "参数漂移：\n{}", drift.join("\n"));
}

/// 方向 2：**catalog 声明的参数必须真的到达 handler**，除非写明理由。
#[test]
fn declared_params_reach_the_handler_or_are_waived() {
    let entries = executor_entries();
    let mut unwired = Vec::new();
    for spec in CATALOG {
        let Some(e) = entries.iter().find(|e| e.id == spec.id) else {
            continue;
        };
        for p in spec.args {
            if read_p(&e.reads, p.name) || waived(spec.id, p.name) {
                continue;
            }
            unwired.push(format!(
                "{}: catalog 声明了 {:?}，handler 一次也没取用",
                spec.id, p.name
            ));
        }
    }
    assert!(
        unwired.is_empty(),
        "声明没人用（schema 里有、代码读不到）：\n{}",
        unwired.join("\n")
    );
}

/// 豁免名单不许烂在里面：条目失效（改名走了 / 接上线了）即红。
#[test]
fn waiver_lists_are_never_stale() {
    let mut stale = Vec::new();
    for (id, name) in waiver_pairs() {
        let spec = CATALOG.iter().find(|s| s.id == *id);
        match spec {
            None => stale.push(format!("{id} 不在 catalog 里，名单条目 {name} 是残留")),
            Some(s) if !s.args.iter().any(|p| p.name == *name) => {
                stale.push(format!("{id} 已不再声明 {name}，名单条目是残留"))
            }
            Some(_) => {}
        }
    }
    assert!(stale.is_empty(), "名单需要更新：\n{}", stale.join("\n"));

    let mut covered = 0usize;
    for e in executor_entries() {
        covered += e
            .declared
            .iter()
            .filter(|d| !read_p(&e.reads, d) && waived(&e.id, d))
            .count();
    }
    let listed = CONSUMED_ELSEWHERE.len() + NOT_WIRED_YET.len();
    assert_eq!(
        covered, listed,
        "名单写了 {listed} 条，实际只有 {covered} 条对得上——接了线的请把条目删掉"
    );
}

/// 同一命令里重复的参数名会让 schema 与取用各判一套。
#[test]
fn param_names_are_unique_within_a_command() {
    for spec in CATALOG {
        let mut seen = BTreeSet::new();
        for p in spec.args {
            assert!(
                seen.insert(p.name),
                "{} 的参数 {} 声明了两次",
                spec.id,
                p.name
            );
        }
    }
}
