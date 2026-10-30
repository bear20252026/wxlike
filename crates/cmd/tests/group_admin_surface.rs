//! **#318 群管理命令面的"真面"用例**（群公告与全员禁言）。
//!
//! 与 `relations_surface.rs` / `relations_surface_317.rs` 同一套手法：
//! 把 `crates/platform/src/groups_admin.rs` 的路由表抽出来，与执行器
//! 分派逐段核对；标注、参数、覆盖表逐条钉死。本文件只读源码文本，
//! 不加依赖。
//!
//! 本卡有一处**刻意的缺口**：全员禁言没有状态查询命令——服务端没有
//! 读 `all_muted` 的端点（`GroupInfo` 未暴露该字段），按 #318 卡面
//! "如路由缺参数，先在 Issue 评论中写明"的规矩，缺口写在 issue 评论
//! 与覆盖表 note 里，命令面不编造。本文件最后一组用例守的就是这件事：
//! 谁把 `group.mute_all.get` 之类指向空气的命令加上，这里就红。

use wxlike_cmd::catalog::{
    self, Capability, Confirmation, Idempotency, ParamType, ScopeClass, CATALOG,
};
use wxlike_cmd::domains::{entries, EntryState};

fn workspace_file(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/cmd 的上两级必须是 workspace 根")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到 {}：{e}", path.display()))
}

/// #318 这批命令的 id，按 catalog 顺序。
const BATCH_IDS: [&str; 3] = [
    "group.announcement.get",
    "group.announcement.set",
    "group.mute_all.set",
];

// ---------------------------------------------------------------- 路由表

/// 抽 `groups_admin.rs` 的 `routes()`（含跨行写法，手法同 317 文件）。
fn admin_routes() -> Vec<(String, String)> {
    let src = workspace_file("crates/platform/src/groups_admin.rs");
    let start = src
        .find("pub fn routes() -> Router<PlatformState>")
        .expect("groups_admin 的 routes() 搬了家或改了签名：同步更新本测试");
    let seg = &src[start..start + 3072];
    let body = &seg[..seg.find("\n}").expect("routes() 找不到函数尾")];
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(pos) = rest.find(".route(") {
        let after = &rest[pos + ".route(".len()..];
        let q1 = after.find('"').expect("route 后没有路径字面量");
        let after_q1 = &after[q1 + 1..];
        let q2 = after_q1.find('"').expect("路径字面量没有收尾引号");
        let path = after_q1[..q2].to_string();
        assert!(path.starts_with("/api"), "抽出来的不是 API 路径：{path}");
        let tail = &after_q1[q2..];
        let upto = tail.find(')').unwrap_or(tail.len()) + 1;
        let verb = ["post", "get", "put", "delete", "patch"]
            .iter()
            .find(|m| tail[..upto.min(tail.len())].contains(&format!("{m}(")))
            .unwrap_or_else(|| panic!("{path} 后面没有认得的动词"));
        out.push((verb.to_uppercase(), path));
        rest = &after[q1 + 1 + q2..];
    }
    assert!(
        out.len() >= 11,
        "只抽到 {} 条 groups_admin 路由，抽取器该更新了",
        out.len()
    );
    out
}

fn normalize_concrete(path: &str) -> String {
    let mut out = String::new();
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            for c in chars.by_ref() {
                if c == '}' {
                    break;
                }
            }
            out.push('*');
        } else {
            out.push(c);
        }
    }
    out
}

fn matching_routes(routes: &[(String, String)], method: &str, path: &str) -> Vec<String> {
    let want: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    routes
        .iter()
        .filter(|(m, _)| m == method)
        .filter(|(_, pattern)| {
            let got: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
            got.len() == want.len()
                && got
                    .iter()
                    .zip(&want)
                    .all(|(p, w)| p.starts_with('{') || p == w)
        })
        .map(|(_, p)| p.clone())
        .collect()
}

fn handler_segment<'a>(handlers_src: &'a str, id: &str) -> &'a str {
    let marker = format!("(\"{id}\",");
    let start = handlers_src
        .find(&marker)
        .unwrap_or_else(|| panic!("执行器分派表里找不到 {id} 的条目，抽取器该更新了"));
    let rest = &handlers_src[start + marker.len()..];
    let next_entry = rest.find("\n    (\"");
    let table_end = rest.find("\n];");
    let end = match (next_entry, table_end) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => panic!("{id} 的 handler 之后既没有条目也没有 `];`，表形状变了"),
    };
    &rest[..end]
}

fn handler_target(handlers: &str, id: &str) -> (String, String) {
    let seg = handler_segment(handlers, id);
    let mut lit = seg
        .split('"')
        .find(|s| s.starts_with("/api"))
        .unwrap_or_else(|| panic!("{id} 的 handler 里没有 /api 开头的路径字面量：{seg}"))
        .to_string();
    if let Some((head, _)) = lit.split_once('?') {
        lit = head.to_string();
    }
    let method = ["POST", "PUT", "DELETE", "PATCH"]
        .iter()
        .find(|m| seg.contains(&format!("\"{m}\"")))
        .map(|m| m.to_string())
        .unwrap_or_else(|| "GET".to_string());
    (method, lit)
}

/// **每条 #318 命令都恰好打在一条已存在的路由上**，且逐条钉死。
#[test]
fn batch_commands_point_at_their_own_endpoints() {
    let handlers = workspace_file("crates/client/src/execute/handlers.rs");
    let routes = admin_routes();
    let want: [(&str, &str, &str); 3] = [
        (
            "group.announcement.get",
            "GET",
            "/api/groups/*/announcement",
        ),
        (
            "group.announcement.set",
            "PUT",
            "/api/groups/*/announcement",
        ),
        ("group.mute_all.set", "PUT", "/api/groups/*/mute-all"),
    ];
    for (id, method, path) in want {
        let (got_method, got_path) = handler_target(&handlers, id);
        let got_path = normalize_concrete(&got_path);
        assert_eq!(
            (got_method.as_str(), got_path.as_str()),
            (method, path),
            "{id}"
        );
        let hits = matching_routes(&routes, method, &got_path);
        assert_eq!(hits.len(), 1, "{id} 命中 {hits:?}（应当恰好一条）");
    }
}

/// 匹配器要有牙 + 范围要有界：单人禁言在本卡范围外，命令面不许
/// 顺手长出 `group.mute.set` 一类条目；`mute-all` 也没有 GET。
#[test]
fn matcher_has_teeth_and_scope_stays_put() {
    let routes = admin_routes();
    assert!(matching_routes(&routes, "GET", "/api/groups/x/mute-all").is_empty());
    assert!(matching_routes(&routes, "POST", "/api/groups/x/announcement").is_empty());
    assert_eq!(
        matching_routes(&routes, "PUT", "/api/groups/x/mute-all").len(),
        1,
        "正例：全员禁言端点确实存在"
    );
    for id in ["group.mute.set", "group.mute.clear", "group.mute_all.get"] {
        assert!(
            catalog::get(id).is_none(),
            "{id} 不在本卡范围/无服务端端点，不许出现在 catalog 里"
        );
    }
}

// ---------------------------------------------------------------- 请求体字段

/// 服务端两个请求体的字段：`AnnouncementBody{text, media_id}`、
/// `AllMuteBody{on}`。命令面发出的键必须都在里面。
#[test]
fn write_bodies_send_only_fields_the_server_accepts() {
    let src = workspace_file("crates/platform/src/groups_admin.rs");
    for (marker, want) in [
        ("struct AnnouncementBody {", vec!["text", "media_id"]),
        ("struct AllMuteBody {", vec!["on"]),
    ] {
        let start = src
            .find(marker)
            .unwrap_or_else(|| panic!("找不到 {marker}"));
        let after = &src[start + marker.len()..];
        let body = &after[..after.find('}').expect("结构体找不到尾")];
        let mut fields = Vec::new();
        for line in body.lines() {
            let t = line.trim().trim_start_matches("pub ");
            if let Some(f) = t.split(':').next() {
                let f = f.trim();
                if !f.is_empty() && f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    fields.push(f.to_string());
                }
            }
        }
        assert_eq!(fields, want, "{marker} 字段变了");
    }
    let handlers = workspace_file("crates/client/src/execute/handlers.rs");
    let seg = handler_segment(&handlers, "group.announcement.set");
    assert!(seg.contains("\"text\""), "announcement.set 没发 text 键");
    let seg = handler_segment(&handlers, "group.mute_all.set");
    assert!(seg.contains("\"on\""), "mute_all.set 没发 on 键");
}

// ---------------------------------------------------------------- 参数与标注

#[test]
fn batch_arguments_are_validated() {
    let set = catalog::get("group.announcement.set").expect("在 catalog 里");
    assert_eq!(
        catalog::validate_args(set, &serde_json::json!({"conv": "g1"})).expect_err("缺 text"),
        catalog::ArgError::Missing("text")
    );
    assert!(
        catalog::validate_args(set, &serde_json::json!({"conv": "g1", "text": "hi"})).is_ok(),
        "media_id 可选"
    );
    let mute = catalog::get("group.mute_all.set").expect("在 catalog 里");
    assert_eq!(
        catalog::validate_args(mute, &serde_json::json!({"conv": "g1", "on": "yes"}))
            .expect_err("字符串不是布尔"),
        catalog::ArgError::WrongType {
            name: "on",
            expected: "boolean"
        }
    );
}

/// 标注口径：读 Read、写 Write；两条写都是**状态赋值**天然幂等
/// （服务端 upsert / UPDATE 绝对值）⇒ `Idempotency::None` 不声明键；
/// 确认一律 `None`——门在服务端管理权限（`assert_manager`），
/// 不是确认框；三条都以会话为授权对象（`needs_conv` + Conversation）。
#[test]
fn batch_annotations_follow_the_rules() {
    let get = catalog::get("group.announcement.get").expect("在 catalog 里");
    assert_eq!(get.capability, Capability::Read);
    for id in BATCH_IDS {
        let spec = catalog::get(id).expect("在 catalog 里");
        assert_eq!(spec.confirmation, Confirmation::None, "{id}");
        assert_eq!(spec.idempotency, Idempotency::None, "{id}");
        assert_eq!(spec.scope, ScopeClass::Conversation, "{id}");
        assert!(spec.needs_conv, "{id} 必须走会话白名单");
        assert!(
            !spec.args.iter().any(|p| p.name == "idempotency_key"),
            "{id} 不许声明幂等键"
        );
        if id != "group.announcement.get" {
            assert_eq!(spec.capability, Capability::Write, "{id}");
        }
    }
    let on = catalog::get("group.mute_all.set")
        .expect("在 catalog 里")
        .args
        .iter()
        .find(|p| p.name == "on")
        .expect("声明了 on");
    assert_eq!(on.ty, ParamType::Bool);
    assert_eq!(CATALOG.len(), 42);
}

/// `params` 字符串与 `args` 双向一致。
#[test]
fn params_string_and_args_agree_both_ways() {
    for id in BATCH_IDS {
        let spec = catalog::get(id).expect("在 catalog 里");
        let declared: Vec<(&str, bool)> = spec.args.iter().map(|p| (p.name, p.required)).collect();
        assert_eq!(parse_params(spec.params), declared, "{id}");
    }
}

fn parse_params(raw: &str) -> Vec<(&str, bool)> {
    if raw == "无" {
        return Vec::new();
    }
    let inner = raw
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or_else(|| panic!("params 说明必须是 {{…}} 形状：{raw}"));
    inner
        .split(',')
        .map(|item| {
            let name = item.split(':').next().expect("参数名").trim();
            let optional = name.ends_with('?');
            (name.trim_end_matches('?'), !optional)
        })
        .collect()
}

// ---------------------------------------------------------------- 覆盖表与缺口

/// 覆盖表：原 blocked 条目拆成 3 条 automated + 1 条 blocked（状态
/// 查询的缺口）；缺口条目的 note 必须点名"无读取端点"，不许悄悄翻绿，
/// 也不许把缺口删掉装作不存在。
#[test]
fn coverage_table_flips_only_what_has_endpoints() {
    let want: [(&str, &str); 3] = [
        ("群公告查询", "group.announcement.get"),
        ("群公告设置", "group.announcement.set"),
        ("全员禁言设置", "group.mute_all.set"),
    ];
    for (label, cmd) in want {
        let entry = entries()
            .iter()
            .find(|e| e.domain == "messaging" && e.label == label)
            .unwrap_or_else(|| panic!("覆盖表里没有 {label} 这条入口"));
        match &entry.state {
            EntryState::Command(id) => assert_eq!(id, cmd, "{label} 登记的命令不对"),
            other => panic!("{label} 本该是 automated（#318），现在是 {other:?}"),
        }
    }
    assert!(
        !entries()
            .iter()
            .any(|e| e.domain == "messaging" && e.label == "群公告与全员禁言"),
        "旧的合并条目已被拆分，不应还留在覆盖表里"
    );
    let gap = entries()
        .iter()
        .find(|e| e.domain == "messaging" && e.label == "全员禁言状态查询")
        .expect("缺口条目必须还在，不许删掉装作不存在");
    match &gap.state {
        EntryState::Blocked { owner, issue } => {
            assert_eq!(owner, "G03");
            assert_eq!(*issue, 121);
        }
        other => panic!("状态查询无服务端端点，必须还是 blocked，现在是 {other:?}"),
    }
    assert!(
        gap.note
            .as_deref()
            .unwrap_or_default()
            .contains("无读取端点"),
        "缺口 note 必须点名原因：{:?}",
        gap.note
    );
}

/// CLI / MCP 两面：friend 域之外，group 域帮助与工具定义同步出现。
#[test]
fn cli_and_mcp_surfaces_carry_the_batch() {
    let text = catalog::cli_help(Some("group"));
    for verb in ["announcement.get", "announcement.set", "mute_all.set"] {
        assert!(
            text.contains(&format!("wxlike group {verb}")),
            "{verb} 不在帮助里：\n{text}"
        );
    }
    for id in BATCH_IDS {
        let t = catalog::tool_definition(catalog::get(id).expect("在 catalog 里"));
        assert_eq!(t["_meta"]["wxlike/scope"], "conversation", "{id}");
        assert_eq!(t["_meta"]["wxlike/confirmation"], "none", "{id}");
        assert_eq!(
            t["annotations"]["destructiveHint"], false,
            "{id} 不是破坏性操作"
        );
    }
}
