//! **#317 relations 第二批的"真面"用例**（备注、删除好友、黑名单与隐私规则）。
//!
//! 与 `relations_surface.rs`（#316 第一批）同一套手法、同一个理由：
//! 把"命令面 ↔ 服务端路由表"双向核对——端点逐段钉死、参数缺失先于
//! 网络报错、危险操作的确认标注按 #315 规则落位。本批多跨一个文件：
//! 备注/删除在 `crates/platform/src/friends.rs`，黑名单/隐私在
//! `crates/platform/src/relations.rs`，两份路由表都要抽。
//!
//! 验收里还有一条"黑名单/隐私生效后被屏蔽方不泄露信息"——那是**服务端**
//! 语义（`is_blocked` 闸门在 platform 侧有自己的用例），命令面这一层
//! 能守的是：命令不带"以别人身份操作"的参数、读命令只按 `authed` 取数。
//! 本文件只读源码文本，不加依赖，与 `param_dto_parity.rs` 同口径。

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

/// #317 这批命令的 id，按 catalog 顺序。
const BATCH_IDS: [&str; 7] = [
    "friend.remark.set",
    "friend.remove",
    "friend.block.list",
    "friend.block.add",
    "friend.block.remove",
    "friend.privacy.get",
    "friend.privacy.set",
];

// ---------------------------------------------------------------- 路由表

/// 从某个 platform 文件的 `routes()` 抽 `(方法, 路径模式)` 清单。
/// 形状变了就 panic——宁可这里红，不要抽到空清单常绿。
fn platform_routes(file: &str, min: usize) -> Vec<(String, String)> {
    let src = workspace_file(file);
    let start = src
        .find("pub fn routes() -> Router<PlatformState>")
        .expect("platform 的 routes() 搬了家或改了签名：同步更新本测试");
    let seg = &src[start..start + 3072];
    let body = &seg[..seg.find("\n}").expect("routes() 找不到函数尾")];
    // relations.rs 有跨行写法（`.route(\n "/api/…",\n post(…)`），
    // 所以不能按行切：找每个 `.route(`，取其后第一个字符串字面量作路径、
    // 其后第一个动词作方法。
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
        let verb = ["post", "get", "put", "delete", "patch"]
            .iter()
            .find(|m| tail[..tail.find(')').unwrap_or(tail.len()) + 1].contains(&format!("{m}(")))
            .unwrap_or_else(|| panic!("{path} 后面没有认得的动词"));
        out.push((verb.to_uppercase(), path));
        rest = &after[q1 + 1 + q2..];
    }
    assert!(
        out.len() >= min,
        "{file} 只抽到 {} 条路由，抽取器该更新了",
        out.len()
    );
    out
}

fn all_routes() -> Vec<(String, String)> {
    let mut v = platform_routes("crates/platform/src/friends.rs", 9);
    v.extend(platform_routes("crates/platform/src/relations.rs", 10));
    v
}

/// 把执行器路径里的内插段换成占位符 `*`（与 #316 文件同口径）。
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

/// **每条 #317 命令都恰好打在一条已存在的路由上**。
#[test]
fn every_batch_command_hits_exactly_one_registered_route() {
    let handlers = workspace_file("crates/client/src/execute/handlers.rs");
    let routes = all_routes();
    for id in BATCH_IDS {
        let (method, path) = handler_target(&handlers, id);
        let hits = matching_routes(&routes, &method, &normalize_concrete(&path));
        assert_eq!(
            hits.len(),
            1,
            "{id} 打的是 {method} {path}，命中 {hits:?}（应当恰好一条）"
        );
    }
}

/// 端点逐个钉死：`PUT /api/blocks/{uid}` 与 `DELETE` 同路径不同动词，
/// 光"命中一条"分不开，必须逐条对。
#[test]
fn each_batch_command_points_at_its_own_endpoint() {
    let handlers = workspace_file("crates/client/src/execute/handlers.rs");
    let want: [(&str, &str, &str); 7] = [
        ("friend.remark.set", "PUT", "/api/friends/*/remark"),
        ("friend.remove", "DELETE", "/api/friends/*"),
        ("friend.block.list", "GET", "/api/blocks"),
        ("friend.block.add", "PUT", "/api/blocks/*"),
        ("friend.block.remove", "DELETE", "/api/blocks/*"),
        ("friend.privacy.get", "GET", "/api/settings/strangers"),
        ("friend.privacy.set", "PUT", "/api/settings/strangers"),
    ];
    for (id, method, path) in want {
        let (got_method, got_path) = handler_target(&handlers, id);
        assert_eq!(
            (got_method.as_str(), normalize_concrete(&got_path).as_str()),
            (method, path),
            "{id}"
        );
    }
}

/// 匹配器要有牙：不存在的端点与错动词都必须零命中。
#[test]
fn the_route_matcher_rejects_what_does_not_exist() {
    let routes = all_routes();
    assert!(matching_routes(&routes, "POST", "/api/blocks").is_empty());
    assert!(matching_routes(&routes, "GET", "/api/friends/x/remark").is_empty());
    assert!(matching_routes(&routes, "DELETE", "/api/settings/strangers").is_empty());
    assert_eq!(
        matching_routes(&routes, "PUT", "/api/blocks/x").len(),
        1,
        "正例：拉黑端点确实存在"
    );
}

// ---------------------------------------------------------------- 请求体字段

/// 抽某个结构体 `pub` 字段名清单。
fn struct_fields(src: &str, marker: &str) -> Vec<String> {
    let start = src
        .find(marker)
        .unwrap_or_else(|| panic!("找不到 {marker}"));
    let after = &src[start + marker.len()..];
    let body = &after[..after.find('}').expect("结构体找不到尾")];
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("pub ").or_else(|| {
            // relations.rs 的 StrangerSetting 字段没有 pub（同文件私有）
            if t.contains(':') && !t.starts_with("//") {
                Some(t)
            } else {
                None
            }
        }) else {
            continue;
        };
        if let Some(field) = rest.split(':').next() {
            let field = field.trim().trim_start_matches("pub ").trim();
            if !field.is_empty() && field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                out.push(field.to_string());
            }
        }
    }
    out
}

/// **发出去的键必须是服务端收得下的键**：备注只有 `remark`，
/// 隐私设置只有 `allow`（服务端 `StrangerSetting` 的键，不是响应里的
/// `allow_strangers`）。
#[test]
fn write_bodies_send_only_fields_the_server_accepts() {
    let handlers = workspace_file("crates/client/src/execute/handlers.rs");
    let friends_src = workspace_file("crates/platform/src/friends.rs");
    let remark_fields = struct_fields(&friends_src, "pub struct RemarkBody {");
    assert_eq!(remark_fields, vec!["remark"], "RemarkBody 字段变了");
    let seg = handler_segment(&handlers, "friend.remark.set");
    assert!(seg.contains("\"remark\""), "remark handler 没发 remark 键");

    let relations_src = workspace_file("crates/platform/src/relations.rs");
    let setting_fields = struct_fields(&relations_src, "struct StrangerSetting {");
    assert_eq!(setting_fields, vec!["allow"], "StrangerSetting 字段变了");
    let seg = handler_segment(&handlers, "friend.privacy.set");
    assert!(
        seg.contains("\"allow\""),
        "privacy.set handler 没发 allow 键"
    );
    let spec = catalog::get("friend.privacy.set").expect("在 catalog 里");
    assert!(
        spec.args.iter().any(|p| p.name == "allow"),
        "命令面参数名必须与服务端请求体同名（allow）"
    );
}

// ---------------------------------------------------------------- 参数校验

#[test]
fn batch_arguments_are_validated_before_anything_is_sent() {
    let remark = catalog::get("friend.remark.set").expect("在 catalog 里");
    assert_eq!(
        catalog::validate_args(remark, &serde_json::json!({"user_id": "u2"}))
            .expect_err("缺 remark 必须报错"),
        catalog::ArgError::Missing("remark")
    );
    let set = catalog::get("friend.privacy.set").expect("在 catalog 里");
    assert_eq!(
        catalog::validate_args(set, &serde_json::json!({"allow": "yes"}))
            .expect_err("字符串不是布尔"),
        catalog::ArgError::WrongType {
            name: "allow",
            expected: "boolean"
        }
    );
    assert!(catalog::validate_args(set, &serde_json::json!({"allow": false})).is_ok());
    // 拼错的参数名不能当成没传
    let remove = catalog::get("friend.remove").expect("在 catalog 里");
    assert_eq!(
        catalog::validate_args(remove, &serde_json::json!({"userId": "u2"}))
            .expect_err("驼峰拼写不能过"),
        catalog::ArgError::Unknown("userId".to_string())
    );
}

/// 两条读命令不带任何参数：黑名单与隐私设置都只按 `authed` 取数，
/// 命令面没有"看别人黑名单"的入口。
#[test]
fn batch_reads_carry_no_arguments_at_all() {
    for id in ["friend.block.list", "friend.privacy.get"] {
        let spec = catalog::get(id).expect("在 catalog 里");
        assert!(spec.args.is_empty(), "{id} 声明了参数：{:?}", spec.args);
        assert_eq!(
            catalog::validate_args(spec, &serde_json::json!({"user_id": "someone"}))
                .expect_err("读命令不许接受别人的 id"),
            catalog::ArgError::Unknown("user_id".to_string()),
            "{id}"
        );
    }
    // 全部七条都不许出现"调用方身份"参数：`user_id` 在本批里永远是
    // **操作对象**（好友/被拉黑者），我是谁只由 token 决定。
    for id in BATCH_IDS {
        let spec = catalog::get(id).expect("在 catalog 里");
        for p in spec.args {
            assert!(
                !matches!(
                    p.name,
                    "from_uid" | "to_uid" | "actor" | "me" | "blocked_by"
                ),
                "{id} 接受了调用方身份参数 {}",
                p.name
            );
        }
    }
}

// ---------------------------------------------------------------- 标注依据

/// #315 规则在本批的落点（结论与服务端依据写在 catalog 各条注释里）：
/// * 删除好友、拉黑：`Destructive` + `Confirmation::Required`，声明
///   `confirmation_token` 可选参数，令牌链路由 `confirmation_token.rs`
///   的全量遍历用例覆盖；
/// * 备注/解除拉黑/隐私设置：`Write` + 确认 `None`（自己的数据或恢复
///   方向），天然幂等（状态赋值 / 状态机门）⇒ `Idempotency::None`
///   且**不许**声明 `idempotency_key`（服务端不读那个头）；
/// * 两条读：`Read`/`None`/`None`，授权对象是账户自己。
#[test]
fn batch_annotations_follow_the_rules() {
    for id in ["friend.remove", "friend.block.add"] {
        let spec = catalog::get(id).expect("在 catalog 里");
        assert_eq!(spec.capability, Capability::Destructive, "{id}");
        assert_eq!(spec.confirmation, Confirmation::Required, "{id}");
        assert_eq!(spec.idempotency, Idempotency::None, "{id}");
        let token = spec
            .args
            .iter()
            .find(|p| p.name == "confirmation_token")
            .unwrap_or_else(|| panic!("{id} 必须声明 confirmation_token"));
        assert!(
            !token.required,
            "{id} 的令牌是 prepare 后才有的，声明为可选"
        );
        assert!(!spec.needs_conv, "{id}");
        assert_eq!(spec.scope, ScopeClass::Account, "{id}");
    }
    for id in [
        "friend.remark.set",
        "friend.block.remove",
        "friend.privacy.set",
    ] {
        let spec = catalog::get(id).expect("在 catalog 里");
        assert_eq!(spec.capability, Capability::Write, "{id}");
        assert_eq!(spec.confirmation, Confirmation::None, "{id}");
        assert_eq!(spec.idempotency, Idempotency::None, "{id}");
        assert_eq!(spec.scope, ScopeClass::Account, "{id}");
        assert!(!spec.needs_conv, "{id}");
    }
    for id in ["friend.block.list", "friend.privacy.get"] {
        let spec = catalog::get(id).expect("在 catalog 里");
        assert_eq!(spec.capability, Capability::Read, "{id}");
        assert_eq!(spec.confirmation, Confirmation::None, "{id}");
        assert_eq!(spec.idempotency, Idempotency::None, "{id}");
    }
    for id in BATCH_IDS {
        let spec = catalog::get(id).expect("在 catalog 里");
        assert!(
            !spec.args.iter().any(|p| p.name == "idempotency_key"),
            "{id} 不许声明幂等键：服务端没有消费它的地方"
        );
    }
    assert_eq!(CATALOG.len(), 39);
}

/// `params` 字符串与 `args` 双向一致（与 #316 文件同口径）。
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

/// 参数类型钉死：`allow` 是布尔、其余皆字符串。
#[test]
fn batch_param_types_are_the_declared_ones() {
    let set = catalog::get("friend.privacy.set").expect("在 catalog 里");
    let allow = set.args.iter().find(|p| p.name == "allow").expect("声明了");
    assert_eq!(allow.ty, ParamType::Bool);
    assert_eq!(allow.ty.json_type(), "boolean");
    let remark = catalog::get("friend.remark.set").expect("在 catalog 里");
    for name in ["user_id", "remark"] {
        let p = remark.args.iter().find(|p| p.name == name).expect("声明了");
        assert_eq!(p.ty, ParamType::Str, "{name}");
        assert!(p.required, "{name}");
    }
}

// ---------------------------------------------------------------- 覆盖表

/// 覆盖表：两条 blocked 拆成七条 automated，逐条 label 对命令；
/// 不属本批的两条（资料与偏好 #114 / 登录会话 #103）必须还是 blocked。
#[test]
fn coverage_table_flips_the_number317_entries() {
    let want: [(&str, &str); 7] = [
        ("好友备注", "friend.remark.set"),
        ("删除好友", "friend.remove"),
        ("黑名单列表", "friend.block.list"),
        ("拉黑", "friend.block.add"),
        ("解除拉黑", "friend.block.remove"),
        ("隐私规则查询", "friend.privacy.get"),
        ("隐私规则设置", "friend.privacy.set"),
    ];
    for (label, cmd) in want {
        let entry = entries()
            .iter()
            .find(|e| e.domain == "relations" && e.label == label)
            .unwrap_or_else(|| panic!("覆盖表里没有 {label} 这条入口"));
        match &entry.state {
            EntryState::Command(id) => assert_eq!(id, cmd, "{label} 登记的命令不对"),
            other => panic!("{label} 本该是 automated（#317），现在是 {other:?}"),
        }
    }
    // 旧的两条 blocked 标签必须消失（拆分而不是并存，避免双登记）
    for label in ["备注与删除好友", "黑名单与隐私规则"] {
        assert!(
            !entries()
                .iter()
                .any(|e| e.domain == "relations" && e.label == label),
            "{label} 已被拆分，不应还留在覆盖表里"
        );
    }
    for label in ["资料与偏好设置", "登录会话与设备"] {
        let entry = entries()
            .iter()
            .find(|e| e.domain == "relations" && e.label == label)
            .unwrap_or_else(|| panic!("覆盖表里没有 {label} 这条入口"));
        assert!(
            matches!(entry.state, EntryState::Blocked { .. }),
            "{label} 不属 #317，必须还是 blocked"
        );
    }
}

/// CLI 侧的面：friend 域帮助里七条新命令都在。
#[test]
fn cli_help_lists_the_batch_verbs() {
    let text = catalog::cli_help(Some("friend"));
    for verb in [
        "remark.set",
        "remove",
        "block.list",
        "block.add",
        "block.remove",
        "privacy.get",
        "privacy.set",
    ] {
        assert!(
            text.contains(&format!("wxlike friend {verb}")),
            "{verb} 不在帮助里：\n{text}"
        );
    }
}

/// MCP 工具面：两条破坏性命令的 hint 与确认元数据必须如实——
/// 界面靠它们提前警告，标错就是骗用户。
#[test]
fn mcp_definitions_mark_destructive_and_confirmation() {
    for id in ["friend.remove", "friend.block.add"] {
        let t = catalog::tool_definition(catalog::get(id).expect("在 catalog 里"));
        assert_eq!(t["annotations"]["destructiveHint"], true, "{id}");
        assert_eq!(t["annotations"]["readOnlyHint"], false, "{id}");
        assert_eq!(t["_meta"]["wxlike/confirmation"], "required", "{id}");
        assert_eq!(
            t["inputSchema"]["properties"]["confirmation_token"]["type"], "string",
            "{id} 的令牌参数要在 schema 里"
        );
    }
    for id in [
        "friend.remark.set",
        "friend.block.remove",
        "friend.privacy.set",
    ] {
        let t = catalog::tool_definition(catalog::get(id).expect("在 catalog 里"));
        assert_eq!(t["annotations"]["destructiveHint"], false, "{id}");
        assert_eq!(t["_meta"]["wxlike/confirmation"], "none", "{id}");
    }
    let t = catalog::tool_definition(catalog::get("friend.privacy.set").expect("在 catalog 里"));
    assert_eq!(
        t["inputSchema"]["properties"]["allow"]["type"], "boolean",
        "allow 在 schema 里必须是布尔"
    );
}
