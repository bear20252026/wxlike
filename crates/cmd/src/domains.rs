//! 业务域覆盖表：**入口总账 → 命令 → 例外**，一张表回答"哪些入口能自动化"。
//!
//! 为什么需要它：`catalog` 只列"已经能执行的命令"，回答不了"某业务还没做"。
//! 没有这张表，"支持全业务"就只能靠嘴说；有了它，缺口必须写成
//! `Blocked { owner, issue }`，或者明确标成 `Host`（人机物理例外）。
//!
//! 数据分两层（都在 `include_str!` 编译进二进制）：
//! * `crates/cmd/data/domains.json`：**索引**，8 个域与标题，顺序即报告顺序；
//! * `crates/cmd/data/entries/<域>.json`：该域的全部入口，一行一条。
//!
//! 为什么拆（#314 第 4 条）：后续卡片各占一个域的入口，拆成 per-domain 文件后
//! 两张卡不会抢同一份 JSON。拆的**硬约束是行为不变**——按索引顺序拼回来的
//! 条目序列与拆前一致，`report()` / `render()` / 生成的 schema 逐字节不变，
//! 由 `crates/cmd/tests/contract_v1_baseline.rs` 的 SHA-256 断言守着。
//!
//! 覆盖表是**数据**不是逻辑：放 Rust 源码里改一次就要重编译，
//! 而且 issue 编号会被静态检查当成"魔法数字"。
//!
//! 三条硬规则（由测试钉住）：
//! 1. catalog 里每条命令**恰好**出现在一个业务条目上——漏登记等于偷偷藏入口；
//! 2. `Blocked` 必须带负责组与 issue 编号——"以后再说"不是状态；
//! 3. `Host` 必须是真实的宿主能力（相机/扫码/文件选择）——不伪造远程硬件。

use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::catalog::{self, HostCapability, Platforms};

/// 编译进二进制的索引原文（只有域与标题）。
const RAW: &str = include_str!("../data/domains.json");

/// 每个域的入口清单，一个文件一份（顺序无关——按索引取）。
/// 加一个域：`domains.json` 加一行 + 这里加一行 `include_str!`。
/// 两边对不上由 [`load`] 直接报错，不会静默少一整域。
const ENTRY_FILES: [(&str, &str); 8] = [
    ("messaging", include_str!("../data/entries/messaging.json")),
    ("relations", include_str!("../data/entries/relations.json")),
    ("content", include_str!("../data/entries/content.json")),
    ("media", include_str!("../data/entries/media.json")),
    ("payment", include_str!("../data/entries/payment.json")),
    (
        "miniprogram",
        include_str!("../data/entries/miniprogram.json"),
    ),
    ("service", include_str!("../data/entries/service.json")),
    ("admin", include_str!("../data/entries/admin.json")),
];

/// 一个业务域。
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Domain {
    pub key: String,
    pub title: String,
}

/// 一个业务条目的状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryState {
    /// 已有命令可自动化（值是 catalog id）。
    Command(String),
    /// 人机物理例外：只能在宿主上完成，MCP 不提供伪远程能力。
    Host(HostCapability),
    /// 还没做：必须写清是谁的活、在哪个 issue 上。
    Blocked { owner: String, issue: u32 },
}

/// 一条业务入口。
#[derive(Clone, Debug)]
pub struct Entry {
    pub domain: String,
    pub label: String,
    pub state: EntryState,
    /// 可选的补充说明（为什么阻塞 / 宿主能力的具体限制）。
    pub note: Option<String>,
}

#[derive(serde::Deserialize)]
struct RawEntry {
    domain: String,
    label: String,
    state: String,
    command: Option<String>,
    host: Option<HostCapability>,
    owner: Option<String>,
    issue: Option<u32>,
    note: Option<String>,
}

#[derive(serde::Deserialize)]
struct RawTable {
    domains: Vec<Domain>,
    /// 拆文件之后索引里不该再有入口；这里的默认空值让"还剩着 entries"能被
    /// [`load`] 明确拒绝，而不是解析失败看不出原因。
    #[serde(default)]
    entries: Vec<RawEntry>,
}

/// 解析后的整张表。
#[derive(Debug)]
pub struct Table {
    pub domains: Vec<Domain>,
    pub entries: Vec<Entry>,
}

static TABLE: OnceLock<Table> = OnceLock::new();

/// 覆盖表（首次使用时解析一次，之后只读）。
///
/// 数据坏了（索引或某个入口文件被改坏）不 panic：退回**空表**并打一行
/// stderr——库代码在坏数据上崩溃会带走宿主进程，而"没有覆盖表"
/// 只是把 `coverage` 报告变空，故障还能被发现与修复。测试里
/// `every_command_is_mapped_exactly_once` 会对着空表立刻红掉，
/// 坏数据不会静默通过 CI。
pub fn table() -> &'static Table {
    TABLE.get_or_init(|| {
        load().unwrap_or_else(|err| {
            eprintln!("wxlike-cmd: 业务域覆盖表数据损坏，退回空表：{err}");
            Table {
                domains: Vec::new(),
                entries: Vec::new(),
            }
        })
    })
}

/// 索引 + 每域一个入口文件 → 整张表。条目顺序 = 索引顺序 × 文件内顺序，
/// 与拆成单文件之前完全一致。
fn load() -> Result<Table, String> {
    let index: RawTable = serde_json::from_str(RAW).map_err(|e| e.to_string())?;
    if !index.entries.is_empty() {
        return Err(
            "domains.json 里还有 entries：入口已拆到 data/entries/<域>.json，索引只留域与标题"
                .to_string(),
        );
    }
    reject_stray_files(&index)?;
    let mut entries = Vec::new();
    for domain in &index.domains {
        entries.extend(load_domain(&domain.key)?);
    }
    Ok(Table {
        domains: index.domains,
        entries,
    })
}

/// 一个域的入口文件。条目自带的 `domain` 必须与文件名一致——
/// 复制粘贴到隔壁文件这种错，靠这里拦。
fn load_domain(key: &str) -> Result<Vec<Entry>, String> {
    let (_, raw) = ENTRY_FILES
        .iter()
        .find(|(name, _)| *name == key)
        .ok_or_else(|| format!("域 {key} 没有对应的 data/entries/{key}.json"))?;
    let rows: Vec<RawEntry> =
        serde_json::from_str(raw).map_err(|e| format!("data/entries/{key}.json：{e}"))?;
    if rows.is_empty() {
        return Err(format!(
            "data/entries/{key}.json 是空的：每个域至少一条入口"
        ));
    }
    rows.iter()
        .map(|row| {
            if row.domain != key {
                return Err(format!(
                    "{} 写在 data/entries/{key}.json 里，但 domain 是 {}",
                    row.label, row.domain
                ));
            }
            convert(row)
        })
        .collect()
}

/// 入口文件与索引必须一一对应：多出来的文件、缺掉的域，都是半拆状态。
fn reject_stray_files(index: &RawTable) -> Result<(), String> {
    for (key, _) in ENTRY_FILES {
        if !index.domains.iter().any(|d| d.key == *key) {
            return Err(format!("data/entries/{key}.json 在索引里没有对应的域"));
        }
    }
    Ok(())
}

/// 所有业务域。
pub fn domains() -> &'static [Domain] {
    &table().domains
}

/// 所有业务条目。
pub fn entries() -> &'static [Entry] {
    &table().entries
}

/// 解析并校验数据结构（测试直接调它，好在数据坏掉时给出准确原因）。
pub fn parse(raw: &str) -> Result<Table, String> {
    let table: RawTable = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let entries = table
        .entries
        .iter()
        .map(convert)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Table {
        domains: table.domains,
        entries,
    })
}

/// 一行 JSON → 一个有类型的条目。缺字段直接报错，不猜默认值。
fn convert(raw: &RawEntry) -> Result<Entry, String> {
    let state = match raw.state.as_str() {
        "automated" => EntryState::Command(need(&raw.command, raw, "command")?),
        "host_only" => EntryState::Host(need(&raw.host, raw, "host")?),
        "blocked" => EntryState::Blocked {
            owner: need(&raw.owner, raw, "owner")?,
            issue: need(&raw.issue, raw, "issue")?,
        },
        other => return Err(format!("{} 的状态 {other:?} 不认识", raw.label)),
    };
    Ok(Entry {
        domain: raw.domain.clone(),
        label: raw.label.clone(),
        state,
        note: raw.note.clone(),
    })
}

fn need<T: Clone>(value: &Option<T>, raw: &RawEntry, field: &str) -> Result<T, String> {
    value
        .clone()
        .ok_or_else(|| format!("{}（{}）缺字段 {field}", raw.label, raw.state))
}

/// 覆盖统计（`wxlike coverage` 与自查页共用）。
pub fn summary() -> Value {
    let items = entries();
    let automated = items
        .iter()
        .filter(|e| matches!(e.state, EntryState::Command(_)))
        .count();
    let host = items
        .iter()
        .filter(|e| matches!(e.state, EntryState::Host(_)))
        .count();
    let blocked = items
        .iter()
        .filter(|e| matches!(e.state, EntryState::Blocked { .. }))
        .count();
    json!({
        "commands": catalog::CATALOG.len(),
        "entries": items.len(),
        "automated": automated,
        "host_only": host,
        "blocked": blocked,
        "domains": domains().len(),
    })
}

/// 机器可读报告：按域分组，每条带状态、负责组与 issue。
pub fn report() -> Value {
    let domains_out: Vec<Value> = domains()
        .iter()
        .map(|d| {
            let items: Vec<Value> = entries()
                .iter()
                .filter(|e| e.domain == d.key)
                .map(entry_json)
                .collect();
            json!({"key": d.key, "title": d.title, "entries": items})
        })
        .collect();
    json!({"summary": summary(), "domains": domains_out})
}

fn entry_json(entry: &Entry) -> Value {
    let mut out = json!({"label": entry.label, "state": state_name(&entry.state)});
    if let Some(note) = &entry.note {
        out["note"] = json!(note);
    }
    match &entry.state {
        EntryState::Command(id) => fill_command(&mut out, id),
        EntryState::Host(cap) => out["host"] = json!(cap.as_str()),
        EntryState::Blocked { owner, issue } => {
            out["owner"] = json!(owner);
            out["issue"] = json!(issue);
        }
    }
    out
}

fn state_name(state: &EntryState) -> &'static str {
    match state {
        EntryState::Command(_) => "automated",
        EntryState::Host(_) => "host_only",
        EntryState::Blocked { .. } => "blocked",
    }
}

fn fill_command(out: &mut Value, id: &str) {
    out["command"] = json!(id);
    let Some(spec) = catalog::get(id) else {
        out["error"] = json!("catalog 里没有这个命令");
        return;
    };
    out["tool"] = json!(spec.tool_name());
    out["capability"] = json!(spec.capability.as_str());
    out["scope"] = json!(spec.scope.as_str());
    out["platforms"] = json!(spec.platforms.names());
    out["confirmation"] = json!(if spec.confirmation.is_required() {
        "required"
    } else {
        "none"
    });
}

/// 给人看的一张表：域 → 条目 → 状态。
pub fn render() -> String {
    let s = summary();
    let mut out = format!(
        "动作覆盖：{} 个命令 / {} 条业务入口（自动 {} · 宿主 {} · 未做 {}）\n\n",
        s["commands"], s["entries"], s["automated"], s["host_only"], s["blocked"]
    );
    for domain in domains() {
        out.push_str(&format!("{}（{}）\n", domain.title, domain.key));
        for entry in entries().iter().filter(|e| e.domain == domain.key) {
            out.push_str(&format!(
                "  {:<20} {}\n",
                entry.label,
                state_text(&entry.state)
            ));
        }
        out.push('\n');
    }
    out
}

fn state_text(state: &EntryState) -> String {
    match state {
        EntryState::Command(id) => match catalog::get(id) {
            Some(spec) => format!(
                "可自动化  {id}（{}，scope={}，平台={}）",
                spec.capability,
                spec.scope,
                platform_names(spec.platforms)
            ),
            None => format!("可自动化  {id}（catalog 里没有这个 id）"),
        },
        EntryState::Host(cap) => format!("宿主能力  需 {}（MCP 不提供伪远程能力）", cap.as_str()),
        EntryState::Blocked { owner, issue } => format!("未做      阻塞于 {owner} #{issue}"),
    }
}

fn platform_names(platforms: Platforms) -> String {
    let names = platforms.names();
    if names.is_empty() {
        "（无）".to_string()
    } else {
        names.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// **双向完整**：catalog 每条命令都被登记一次；每条的 id 都真实存在。
    #[test]
    fn every_command_is_mapped_exactly_once() {
        let mut seen: HashSet<&str> = HashSet::new();
        for entry in entries() {
            let EntryState::Command(id) = &entry.state else {
                continue;
            };
            assert!(
                catalog::get(id).is_some(),
                "{} 登记了不存在的命令 {id}",
                entry.label
            );
            assert!(seen.insert(id.as_str()), "命令 {id} 被登记了两次");
        }
        let missing: Vec<&str> = catalog::CATALOG
            .iter()
            .map(|s| s.id)
            .filter(|id| !seen.contains(id))
            .collect();
        assert!(missing.is_empty(), "这些命令没有业务归属：{missing:?}");
    }

    /// 每个域至少一条入口；条目的 domain 必须来自 domains 列表。
    #[test]
    fn every_domain_has_entries_and_no_stray_keys() {
        let keys: Vec<&str> = domains().iter().map(|d| d.key.as_str()).collect();
        for entry in entries() {
            assert!(
                keys.contains(&entry.domain.as_str()),
                "未知域 {}",
                entry.domain
            );
        }
        for key in keys {
            assert!(
                entries().iter().any(|e| e.domain == key),
                "域 {key} 没有任何入口"
            );
        }
    }

    /// 拆成 per-domain 文件之后（#314），索引与入口文件必须一一对应：
    /// 加一个域只改一边，就会有一半的表看不见。
    #[test]
    fn entry_files_cover_exactly_the_indexed_domains() {
        let keys: Vec<&str> = ENTRY_FILES.iter().map(|(k, _)| *k).collect();
        for domain in domains() {
            assert!(
                keys.contains(&domain.key.as_str()),
                "域 {} 没有对应的 data/entries/{}.json",
                domain.key,
                domain.key
            );
        }
        for key in &keys {
            assert!(
                domains().iter().any(|d| d.key == *key),
                "入口文件 {key} 在 domains.json 索引里没有对应的域"
            );
        }
        assert_eq!(
            keys.len(),
            domains().len(),
            "入口文件数与索引域数必须相等（改一处就要改另一处）"
        );
    }

    /// 找不到文件 / 文件里域对不上，都是**错误**不是"少几条"。
    #[test]
    fn a_missing_or_mismatched_entry_file_is_an_error_not_a_missing_domain() {
        let err = load_domain("nope").unwrap_err();
        assert!(err.contains("没有对应的"), "{err}");
        // 空表退化路径：load 失败 → entries 为空 → 上面的映射测试立刻红
        assert!(!entries().is_empty(), "覆盖表退化成空表了：数据坏了");
    }

    /// 未做必须写清负责组与 issue：这不是"待办清单"，是承诺。
    #[test]
    fn blocked_entries_name_an_owner_and_an_issue() {
        for entry in entries() {
            if let EntryState::Blocked { owner, issue } = &entry.state {
                assert!(!owner.is_empty(), "{} 没有负责组", entry.label);
                assert!(owner.starts_with('G'), "负责组要写组号：{owner}");
                assert!(*issue > 0, "{} 没有 issue 编号", entry.label);
            }
        }
    }

    /// 宿主能力必须是**真的硬件**；坏数据要在解析期就炸。
    #[test]
    fn host_entries_are_physical_capabilities_only() {
        for entry in entries() {
            if let EntryState::Host(cap) = &entry.state {
                assert_ne!(
                    *cap,
                    HostCapability::None,
                    "{} 标了宿主却没有能力",
                    entry.label
                );
            }
        }
        let broken = r#"{"domains":[{"key":"x","title":"X"}],
                        "entries":[{"domain":"x","label":"y","state":"host_only"}]}"#;
        assert!(
            parse(broken).unwrap_err().contains("host"),
            "缺 host 要报错"
        );
        let unknown = r#"{"domains":[{"key":"x","title":"X"}],
                          "entries":[{"domain":"x","label":"y","state":"maybe"}]}"#;
        assert!(parse(unknown).unwrap_err().contains("不认识"));
    }

    /// 管理域在 G14 合入前必须是零命令：普通 agent 没有管理动作可调。
    #[test]
    fn admin_domain_has_no_automated_commands_yet() {
        let automated: Vec<&str> = entries()
            .iter()
            .filter(|e| e.domain == "admin")
            .filter_map(|e| match &e.state {
                EntryState::Command(id) => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            automated.is_empty(),
            "管理动作要走 G14 的 RBAC 与专 scope，不能塞进通用命令面：{automated:?}"
        );
    }

    #[test]
    fn report_is_machine_readable_and_counts_add_up() {
        let report = report();
        let summary = &report["summary"];
        assert_eq!(summary["commands"], catalog::CATALOG.len());
        assert_eq!(summary["entries"], entries().len());
        let counted = summary["automated"].as_u64().unwrap_or(0)
            + summary["host_only"].as_u64().unwrap_or(0)
            + summary["blocked"].as_u64().unwrap_or(0);
        assert_eq!(counted as usize, entries().len(), "三类之和必须等于总数");
        assert_eq!(
            summary["automated"],
            catalog::CATALOG.len(),
            "命令与自动化入口一一对应"
        );
        assert_eq!(
            report["domains"].as_array().map(Vec::len),
            Some(domains().len())
        );
    }

    #[test]
    fn rendered_table_mentions_every_entry() {
        let text = render();
        for entry in entries() {
            assert!(text.contains(&entry.label), "表格漏了 {}", entry.label);
        }
        assert!(text.contains("动作覆盖"), "{text}");
    }
}
