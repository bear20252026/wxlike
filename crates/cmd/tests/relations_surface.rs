//! **#316 relations 第一批的"真面"用例**。
//!
//! 卡片要求每条新命令都能被证明三件事：**打到对端点、参数不传就报错、
//! 没有权限拿不到别人数据**。这里走的是"命令面 ↔ 服务端路由表"的双向核对，
//! 而不是把 handler 的返回值再信一遍：
//!
//! * **端点**：把 `crates/platform/src/friends.rs` 的 `routes()` 读成
//!   `(方法, 路径模式)` 清单，再把执行器分派表里这条命令实际打出去的路径抽出来，
//!   两者逐段比对，**必须恰好命中一条路由**。服务端用 `tower::oneshot` 打的
//!   那批用例（`crates/platform/tests/friends.rs`）证明的是"路由活着且鉴权对"；
//!   本文件证明的是"**命令面指的就是那一条**"——名字写错一个字母这里就红。
//!   匹配器本身也有反面用例（`the_route_matcher_rejects_paths_that_do_not_exist`），
//!   免得它宽到什么都算命中。
//! * **参数**：`catalog::validate_args` 是 MCP 与 CLI 共用的那道校验，
//!   这里直接对它断言"缺必填 / 拼错 / 类型不符"各回哪种错误。
//! * **权限**：三条读命令**不声明**任何身份参数（服务端只按 `authed.user_id`
//!   取数），所以传 `user_id` 只会得到"未知参数"；`req_id` 还要过执行器侧的
//!   `path_id`（见 `crates/client/src/execute.rs` 的 `friend_*` 用例）。
//!
//! 为什么不直接 `tower::oneshot`：`crates/cmd` 不依赖 axum/tower/platform
//! （本卡**不加依赖**，`Cargo.lock` 不在改动范围），跨面核对只能读源码事实——
//! 这与 `param_dto_parity.rs`、`idempotency_annotations.rs` 同一个手法：
//! 真相在文本里，就把文本切一遍。读的是 `crates/platform/**`，**只读**。

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

/// #316 这批命令的 id，按 catalog 顺序。
const FRIEND_IDS: [&str; 6] = [
    "friend.list",
    "friend.requests.incoming",
    "friend.requests.outgoing",
    "friend.request.create",
    "friend.request.accept",
    "friend.request.decline",
];

// ---------------------------------------------------------------- 路由表

/// `crates/platform/src/friends.rs` 的 `routes()` → `[(方法, 路径模式)]`。
///
/// 形状变了（不再是 `.route("路径", 动词(handler))`）就 panic：
/// 宁可这里红，也不要"抽到空清单 → 什么都不比对 → 常绿"。
fn platform_friend_routes() -> Vec<(String, String)> {
    let src = workspace_file("crates/platform/src/friends.rs");
    let start = src
        .find("pub fn routes() -> Router<PlatformState>")
        .expect("platform 的 friends routes() 搬了家或改了签名：同步更新本测试");
    let seg = &src[start..start + 2048];
    let body = &seg[..seg.find("\n}").expect("routes() 找不到函数尾")];
    let mut out = Vec::new();
    for line in body.lines() {
        let Some(rest) = line.split(".route(\"").nth(1) else {
            continue;
        };
        let path = rest.split('"').next().expect("路径字面量").to_string();
        assert!(
            path.starts_with("/api/friends"),
            "抽出来的不是 friends 路由：{line}"
        );
        let verb = ["post", "get", "put", "delete"]
            .iter()
            .find(|m| rest.contains(&format!("{m}(")))
            .unwrap_or_else(|| panic!("这一行没有认得的动词：{line}"));
        out.push((verb.to_uppercase(), path));
    }
    assert!(
        out.len() >= 9,
        "只抽到 {} 条 friends 路由，抽取器该更新了",
        out.len()
    );
    out
}

/// 把执行器路径里的内插段（`{id}`、`{conv}`）换成占位符 `*`。
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

/// 具体路径（含 `*` 占位段）命中了哪些路由。
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

/// 分派表里一条命令的 handler 原文（与 `idempotency_annotations.rs` 同一口径）。
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

/// handler 实际打出去的 `(方法, 路径)`。只认 `expect_json` / `expect_empty` /
/// `post_idempotent` 三种通道里的那条路径字面量。
fn handler_target(handlers: &str, id: &str) -> (String, String) {
    let seg = handler_segment(handlers, id);
    let mut lit = seg
        .split('"')
        // 引号分隔后，偶数下标是字面量之间的那段——直接找以 /api 开头的那段即可
        .find(|s| s.starts_with("/api"))
        .unwrap_or_else(|| panic!("{id} 的 handler 里没有 /api 开头的路径字面量：{seg}"))
        .to_string();
    // 查询串不算路径的一部分（路由比对只看 path）
    if let Some((head, _)) = lit.split_once('?') {
        lit = head.to_string();
    }
    let method = ["POST", "PUT", "DELETE", "PATCH"]
        .iter()
        .find(|m| seg.contains(&format!("\"{m}\"")))
        .map(|m| m.to_string())
        .unwrap_or_else(|| {
            if seg.contains("post_idempotent(") {
                "POST".to_string()
            } else {
                "GET".to_string()
            }
        });
    (method, lit)
}

/// **每条 #316 命令都恰好打在一条已存在的路由上**。
#[test]
fn every_friend_command_hits_exactly_one_registered_route() {
    let handlers = workspace_file("crates/client/src/execute/handlers.rs");
    let routes = platform_friend_routes();
    for id in FRIEND_IDS {
        let (method, path) = handler_target(&handlers, id);
        let hits = matching_routes(&routes, &method, &normalize_concrete(&path));
        assert_eq!(
            hits.len(),
            1,
            "{id} 打的是 {method} {path}，命中 {hits:?}（应当恰好一条）"
        );
    }
}

/// 端点逐个钉死：光"命中一条"不够，`incoming` 与 `outgoing` 互换也是各命中一条。
#[test]
fn each_friend_command_points_at_its_own_endpoint() {
    let handlers = workspace_file("crates/client/src/execute/handlers.rs");
    let want: [(&str, &str, &str); 6] = [
        ("friend.list", "GET", "/api/friends"),
        (
            "friend.requests.incoming",
            "GET",
            "/api/friends/requests/incoming",
        ),
        (
            "friend.requests.outgoing",
            "GET",
            "/api/friends/requests/outgoing",
        ),
        ("friend.request.create", "POST", "/api/friends/requests"),
        (
            "friend.request.accept",
            "POST",
            "/api/friends/requests/*/accept",
        ),
        (
            "friend.request.decline",
            "POST",
            "/api/friends/requests/*/decline",
        ),
    ];
    for (id, method, path) in want {
        let (got_method, got_path) = handler_target(&handlers, id);
        let got_path = normalize_concrete(&got_path);
        assert_eq!(
            (got_method.as_str(), got_path.as_str()),
            (method, path),
            "{id}"
        );
    }
}

/// 匹配器自己要有牙：**不存在的端点必须一条都不命中**。
/// 没有这条反面用例，上面两个测试可能只是"匹配器什么都放行"。
#[test]
fn the_route_matcher_rejects_paths_that_do_not_exist() {
    let routes = platform_friend_routes();
    // 服务端没有 approve / 没有 GET 单条好友 / 没有第二层路径
    assert!(matching_routes(&routes, "POST", "/api/friends/requests/x/approve").is_empty());
    assert!(matching_routes(&routes, "GET", "/api/friends/requests/incoming/extra").is_empty());
    assert!(
        matching_routes(&routes, "GET", "/api/friends/x").is_empty(),
        "只有 DELETE /api/friends/{{user_id}}"
    );
    // 方法也要比对：同一条路径换个动词就不该算命中
    assert!(matching_routes(&routes, "GET", "/api/friends/requests").is_empty());
    assert_eq!(
        matching_routes(&routes, "POST", "/api/friends/requests").len(),
        1,
        "正例：create 的端点确实存在"
    );
}

// ---------------------------------------------------------------- 请求体字段

/// 服务端 `RequestBody` 的字段名（`crates/platform/src/friends.rs:38-43`）。
fn request_body_fields() -> Vec<String> {
    let src = workspace_file("crates/platform/src/friends.rs");
    let start = src
        .find("pub struct RequestBody {")
        .expect("platform 的 RequestBody 改了名：同步更新本测试");
    let after = &src[start + "pub struct RequestBody {".len()..];
    let body = &after[..after.find('}').expect("RequestBody 找不到结构体尾")];
    let mut out = Vec::new();
    for line in body.lines() {
        let Some(rest) = line.trim().strip_prefix("pub ") else {
            continue;
        };
        if let Some(field) = rest.split(':').next() {
            let field = field.trim();
            if !field.is_empty() {
                out.push(field.to_string());
            }
        }
    }
    assert_eq!(out, vec!["to_user_id", "message"], "RequestBody 字段变了");
    out
}

/// **发出去的键必须是服务端收得下的键**。
///
/// `friend.request.create` 声明的每个参数，既要真的出现在 handler 发出的字面量里
/// （不是装饰），又要是服务端 `RequestBody` 的字段（不是打空气）。
/// 任一侧改名都会在这里红——这是 `param_dto_parity`（catalog↔执行器）之外
/// 的另一半：执行器↔服务端。
#[test]
fn create_request_sends_only_fields_the_server_accepts() {
    let handlers = workspace_file("crates/client/src/execute/handlers.rs");
    let seg = handler_segment(&handlers, "friend.request.create");
    let fields = request_body_fields();
    for p in catalog::get("friend.request.create")
        .expect("在 catalog 里")
        .args
    {
        assert!(
            seg.contains(&format!("\"{}\"", p.name)),
            "handler 没有把 {:?} 拼进请求体——命令面发了对方收不到",
            p.name
        );
        assert!(
            fields.iter().any(|f| f == p.name),
            "{} 不是服务端 RequestBody 的字段（服务端字段：{fields:?}）",
            p.name
        );
    }
}

// ---------------------------------------------------------------- 参数校验

/// 缺必填、拼错、类型不符：`validate_args` 是 MCP 与 CLI 共用的那道门。
#[test]
fn friend_command_arguments_are_validated_before_anything_is_sent() {
    let accept = catalog::get("friend.request.accept").expect("在 catalog 里");
    assert_eq!(
        catalog::validate_args(accept, &serde_json::json!({})).expect_err("缺 req_id 必须报错"),
        catalog::ArgError::Missing("req_id")
    );
    assert_eq!(
        catalog::validate_args(accept, &serde_json::json!({"reqId": "a1"}))
            .expect_err("驼峰拼写不能当成没传"),
        catalog::ArgError::Unknown("reqId".to_string())
    );
    assert_eq!(
        catalog::validate_args(accept, &serde_json::json!({"req_id": 7}))
            .expect_err("数字不是字符串"),
        catalog::ArgError::WrongType {
            name: "req_id",
            expected: "string"
        }
    );

    let create = catalog::get("friend.request.create").expect("在 catalog 里");
    assert_eq!(
        catalog::validate_args(create, &serde_json::json!({"message": "hi"}))
            .expect_err("缺 to_user_id 必须报错"),
        catalog::ArgError::Missing("to_user_id")
    );
    // 附言可选，但给了就得是字符串
    assert!(
        catalog::validate_args(create, &serde_json::json!({"to_user_id": "u2"})).is_ok(),
        "不给附言是合法调用"
    );
    assert!(catalog::validate_args(
        create,
        &serde_json::json!({"to_user_id": "u2", "message": 3})
    )
    .is_err());
}

/// **读命令递不出别人的身份**：三条读命令参数为空，传 `user_id` 只会得到
/// "未知参数"。服务端的名单查询只吃 `authed.user_id`
/// （`crates/platform/src/friends.rs:299-318`，SQL `WHERE user_id = ?1`），
/// 所以命令面没有"看别人通讯录"的入口，也就没有越权可读的口子。
#[test]
fn friend_reads_carry_no_identity_argument() {
    for id in [
        "friend.list",
        "friend.requests.incoming",
        "friend.requests.outgoing",
    ] {
        let spec = catalog::get(id).expect("在 catalog 里");
        assert!(spec.args.is_empty(), "{id} 声明了参数：{:?}", spec.args);
        let err = catalog::validate_args(spec, &serde_json::json!({"user_id": "someone"}))
            .expect_err("读命令不许接受别人的 id");
        assert_eq!(
            err,
            catalog::ArgError::Unknown("user_id".to_string()),
            "{id}"
        );
    }
    // 写命令里也只有"要加谁"与"哪条申请"，没有"我是谁"
    for id in [
        "friend.request.create",
        "friend.request.accept",
        "friend.request.decline",
    ] {
        let spec = catalog::get(id).expect("在 catalog 里");
        for p in spec.args {
            assert!(
                !matches!(p.name, "user_id" | "from_uid" | "to_uid" | "actor"),
                "{id} 接受了调用方身份参数 {}",
                p.name
            );
        }
    }
}

// ---------------------------------------------------------------- 标注依据

/// #315 规则的落点：读类 `None/None`；三条写命令**天然幂等**（唯一键 /
/// 状态机门，依据见 catalog 注释），服务端不读 `Idempotency-Key` 头，
/// 所以连 `idempotency_key` 都不许声明；确认一律 `None`（只增不减的关系写入
/// 套确认只会让人无脑点确认）。
#[test]
fn friend_annotations_follow_the_number315_rules() {
    let reads = [
        "friend.list",
        "friend.requests.incoming",
        "friend.requests.outgoing",
    ];
    for id in reads {
        let spec = catalog::get(id).expect("在 catalog 里");
        assert_eq!(spec.capability, Capability::Read, "{id}");
        assert_eq!(spec.idempotency, Idempotency::None, "{id}");
        assert_eq!(spec.confirmation, Confirmation::None, "{id}");
        assert_eq!(
            spec.scope,
            ScopeClass::Account,
            "{id} 的授权对象是调用者自己，不是会话"
        );
        assert!(!spec.needs_conv, "{id} 与会话白名单无关");
    }
    let writes = [
        "friend.request.create",
        "friend.request.accept",
        "friend.request.decline",
    ];
    for id in writes {
        let spec = catalog::get(id).expect("在 catalog 里");
        assert_eq!(spec.capability, Capability::Write, "{id}");
        assert_eq!(spec.idempotency, Idempotency::None, "{id}");
        assert_eq!(spec.confirmation, Confirmation::None, "{id}");
        assert_eq!(
            spec.scope,
            ScopeClass::Account,
            "{id} 动的是我自己的关系，不是某个会话"
        );
        assert!(!spec.needs_conv, "{id}");
        assert!(
            !spec.args.iter().any(|p| p.name == "idempotency_key"),
            "{id} 不许声明幂等键：服务端没有消费它的地方"
        );
        assert!(
            !spec.args.iter().any(|p| p.name == "confirmation_token"),
            "{id} 是 Confirmation::None，令牌根本签发不出来"
        );
    }
    // 面不能空转：这批必须真的在 catalog 里，且只多不少
    assert_eq!(CATALOG.len(), 32);
    for id in FRIEND_IDS {
        assert!(catalog::get(id).is_some(), "{id} 不在 catalog 里");
    }
}

/// `params` 字符串与 `args` 是**同一份声明**的两副面孔：键集合、必选性都要一致。
/// `catalog_is_well_formed` 只查了"args 里的名字出现在 params 里"，
/// 这里补反方向与 `?` 标记——多写一个没人实现的参数、或漏写 `?`，都在这红。
#[test]
fn params_string_and_args_agree_both_ways() {
    for id in FRIEND_IDS {
        let spec = catalog::get(id).expect("在 catalog 里");
        let declared: Vec<(&str, bool)> = spec.args.iter().map(|p| (p.name, p.required)).collect();
        let written = parse_params(spec.params);
        assert_eq!(written, declared, "{id}：params 与 args 不是同一份声明");
    }
}

/// 把 `"{a: string, b?: number}"` 切成 `[("a", 必填), ("b", 可选)]`；`"无"` → 空。
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

// ---------------------------------------------------------------- 覆盖表

/// 覆盖表：六条命令各登记一次；`#317` 的两条（备注与删除好友 / 黑名单与隐私规则）
/// **必须还是 blocked**——本卡不许顺手把它们改成 automated。
#[test]
fn coverage_table_flips_only_the_number316_entries() {
    let mut mapped: Vec<&str> = Vec::new();
    for entry in entries() {
        if entry.domain != "relations" {
            continue;
        }
        if let EntryState::Command(id) = &entry.state {
            if id.starts_with("friend.") {
                mapped.push(id);
            }
        }
    }
    mapped.sort_unstable();
    let mut want = FRIEND_IDS.to_vec();
    want.sort_unstable();
    assert_eq!(mapped, want, "relations 里登记的好友命令与 catalog 不一致");

    for label in [
        "备注与删除好友",
        "黑名单与隐私规则",
        "资料与偏好设置",
        "登录会话与设备",
    ] {
        let entry = entries()
            .iter()
            .find(|e| e.domain == "relations" && e.label == label)
            .unwrap_or_else(|| panic!("覆盖表里没有 {label} 这条入口"));
        match &entry.state {
            EntryState::Blocked { owner, issue } => {
                assert_eq!(owner, "G02", "{label} 的负责组");
                assert!(*issue > 0, "{label} 没有 issue 编号");
            }
            other => panic!("{label} 本该是 blocked（属 #317），现在是 {other:?}"),
        }
    }
}

/// CLI 侧的面：`wxlike friend --help` 由 catalog 生成，六条都在；
/// 域名列表多出 `friend`（这是 `domains()` 的顺序，也进生成面摘要）。
#[test]
fn cli_help_generates_the_friend_domain() {
    let text = catalog::cli_help(Some("friend"));
    assert!(text.starts_with("域 friend："), "{text}");
    for verb in [
        "list",
        "requests.incoming",
        "requests.outgoing",
        "request.create",
        "request.accept",
        "request.decline",
    ] {
        assert!(
            text.contains(&format!("wxlike friend {verb}")),
            "{verb} 不在帮助里：\n{text}"
        );
    }
    assert!(text.contains("--param to_user_id=<string> 必填"), "{text}");
    assert!(text.contains("--param message=<string>\t"), "{text}");
    let overview = catalog::cli_help(None);
    assert!(catalog::domains().contains(&"friend"));
    for id in FRIEND_IDS {
        assert!(overview.contains(id), "总表漏了 {id}");
    }
}

/// MCP 工具面：工具名 = `.` → `-`，读命令 `readOnlyHint`、六条都 `idempotentHint`
/// （读天然幂等 + 写由服务端去重），没有一条要确认。
#[test]
fn mcp_tool_definitions_carry_the_right_annotations() {
    for id in FRIEND_IDS {
        let spec = catalog::get(id).expect("在 catalog 里");
        let t = catalog::tool_definition(spec);
        let tool_name = id.replace('.', "-");
        assert_eq!(t["name"], serde_json::json!(tool_name), "{id}");
        assert_eq!(t["_meta"]["wxlike/actionId"], id, "{id}");
        assert_eq!(t["_meta"]["wxlike/scope"], "account", "{id}");
        assert_eq!(t["_meta"]["wxlike/confirmation"], "none", "{id}");
        assert_eq!(
            t["annotations"]["readOnlyHint"],
            serde_json::json!(spec.capability == Capability::Read),
            "{id} 的 readOnlyHint"
        );
        assert_eq!(t["annotations"]["destructiveHint"], false, "{id}");
        // `idempotentHint` 的规则在 `catalog::tool_definition`：读天然幂等算真，
        // 写只有 `Idempotency::Required`（调用方带键）才算真。#316 这三条写命令
        // 服务端靠唯一键/状态机去重，但**没有键可带**，所以提示位是 false——
        // 这是刻意的：hint 不是授权依据，也不许把"没接线的幂等"报成"幂等"。
        assert_eq!(
            t["annotations"]["idempotentHint"],
            serde_json::json!(spec.capability == Capability::Read),
            "{id} 的 idempotentHint"
        );
        assert_eq!(t["inputSchema"]["additionalProperties"], false, "{id}");
    }
    assert_eq!(
        catalog::tool_definition(catalog::get("friend.request.create").expect("在"))
            .pointer("/inputSchema/required")
            .and_then(|v| v.as_array())
            .map(|a| a.len()),
        Some(1),
        "只有 to_user_id 是必填"
    );
}

/// 参数类型不许悄悄漂移：`to_user_id`/`req_id` 是字符串，不是数字也不是数组。
#[test]
fn friend_params_are_the_declared_types() {
    let spec = catalog::get("friend.request.create").expect("在 catalog 里");
    let to = spec
        .args
        .iter()
        .find(|p| p.name == "to_user_id")
        .expect("声明了");
    assert_eq!(to.ty, ParamType::Str);
    assert!(to.required);
    let msg = spec
        .args
        .iter()
        .find(|p| p.name == "message")
        .expect("声明了");
    assert_eq!(msg.ty, ParamType::Str);
    assert!(!msg.required);
    assert_eq!(msg.ty.json_type(), "string");
}
