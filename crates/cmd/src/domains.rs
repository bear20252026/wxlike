//! 业务域覆盖表：**入口总账 → 命令 → 例外**，一张表回答"哪些入口能自动化"。
//!
//! 为什么需要它：`catalog` 只列"已经能执行的命令"，回答不了"某业务还没做"。
//! 没有这张表，"支持全业务"就只能靠嘴说；有了它，缺口必须写成
//! `Blocked { owner, issue }`，或者明确标成 `Host`（人机物理例外）。
//!
//! 数据在 `crates/cmd/data/domains.json`（`include_str!` 编译进二进制）：
//! 覆盖表是**数据**不是逻辑，放 Rust 源码里改一次就要重编译，
//! 而且 issue 编号会被静态检查当成"魔法数字"。
//!
//! 三条硬规则（由测试钉住）：
//! 1. catalog 里每条命令**恰好**出现在一个业务条目上——漏登记等于偷偷藏入口；
//! 2. `Blocked` 必须带负责组与 issue 编号——"以后再说"不是状态；
//! 3. `Host` 必须是真实的宿主能力（相机/扫码/文件选择）——不伪造远程硬件。

use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::catalog::{self, HostCapability, Platforms};

/// 编译进二进制的覆盖表原文。改数据不需要改代码结构。
const RAW: &str = include_str!("../data/domains.json");

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
pub fn table() -> &'static Table {
    TABLE.get_or_init(|| parse(RAW).unwrap_or_else(|e| panic!("覆盖表数据损坏：{e}")))
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
