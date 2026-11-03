//! **#319 媒体命令面的"真面"用例**（文件中心、上传会话与对象权限）。
//!
//! 手法与 relations / group_admin 两份真面用例相同，但服务端面不在
//! platform，而在 `crates/features/media/src/http.rs`（wxlike-media，
//! 由 server 的 media_mount 挂载，鉴权与 platform 共用会话令牌）。
//! 本文件把那张 `/api/v1` 路由表抽出来逐条核对——开工前曾有人只看
//! `crates/platform/src/media.rs` 就断言"没有端点"，这组用例守的
//! 正是别再犯同样的错：**每条 media 命令都必须恰好命中一条
//! features/media 的真实路由**。

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

/// #319 这批命令的 id，按 catalog 顺序。
const BATCH_IDS: [&str; 7] = [
    "media.files.list",
    "media.upload.get",
    "media.upload.start",
    "media.upload.complete",
    "media.upload.cancel",
    "media.object.access",
    "media.object.revoke",
];

// ---------------------------------------------------------------- 路由表

/// 抽 features/media `http.rs` 的 `router()` 路由表。
fn media_routes() -> Vec<(String, String)> {
    let src = workspace_file("crates/features/media/src/http.rs");
    let start = src
        .find("pub fn router(self) -> Router")
        .expect("media http 的 router() 搬了家或改了签名：同步更新本测试");
    let seg = &src[start..start + 6144];
    let body = &seg[..seg.find("\n    }").expect("router() 找不到函数尾")];
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
        out.len() >= 24,
        "只抽到 {} 条 media 路由，抽取器该更新了",
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

/// **每条 #319 命令都恰好打在一条已存在的路由上**，且逐条钉死。
#[test]
fn batch_commands_point_at_their_own_endpoints() {
    let handlers = workspace_file("crates/client/src/execute/handlers.rs");
    let routes = media_routes();
    let want: [(&str, &str, &str); 7] = [
        ("media.files.list", "GET", "/api/v1/files"),
        ("media.upload.get", "GET", "/api/v1/uploads/*"),
        ("media.upload.start", "POST", "/api/v1/uploads"),
        (
            "media.upload.complete",
            "POST",
            "/api/v1/uploads/*/complete",
        ),
        ("media.upload.cancel", "POST", "/api/v1/uploads/*/cancel"),
        ("media.object.access", "POST", "/api/v1/objects/*/access"),
        ("media.object.revoke", "POST", "/api/v1/objects/*/revoke"),
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

/// 匹配器要有牙 + 范围要有界：分片上传与下载是二进制数据面，
/// 命令面不许长出 `media.upload.chunk` / `media.object.download`；
/// 文件的改名/回收站/分享也不在本卡。
#[test]
fn matcher_has_teeth_and_scope_stays_put() {
    let routes = media_routes();
    assert!(matching_routes(&routes, "DELETE", "/api/v1/files").is_empty());
    assert!(matching_routes(&routes, "GET", "/api/v1/uploads").is_empty());
    assert_eq!(
        matching_routes(&routes, "PUT", "/api/v1/uploads/x/chunks").len(),
        1,
        "正例：分片端点确实存在（只是不进命令面）"
    );
    for id in [
        "media.upload.chunk",
        "media.object.download",
        "media.files.trash",
        "media.files.share",
        "media.object.get",
    ] {
        assert!(
            catalog::get(id).is_none(),
            "{id} 不在本卡范围，不许出现在 catalog 里"
        );
    }
}

// ---------------------------------------------------------------- 请求/查询形状

/// 服务端 DTO 字段钉死：`UploadStart{purpose,name,size,mime,digest}`、
/// `CompleteBody{digest}`、`FileListQuery{category,folder,sort,cursor,limit}`。
#[test]
fn dtos_are_pinned_to_the_server_shapes() {
    let http = workspace_file("crates/features/media/src/http.rs");
    let uploads = workspace_file("crates/features/media/src/uploads.rs");
    let files = workspace_file("crates/features/media/src/files.rs");
    for (src, marker, want) in [
        (
            &uploads,
            "pub struct UploadStart {",
            vec!["purpose", "name", "size", "mime", "digest"],
        ),
        (&http, "struct CompleteBody {", vec!["digest"]),
        (
            &files,
            "pub struct FileListQuery {",
            vec!["category", "folder", "sort", "cursor", "limit"],
        ),
    ] {
        let start = src
            .find(marker)
            .unwrap_or_else(|| panic!("找不到 {marker}"));
        let after = &src[start + marker.len()..];
        let body = &after[..after.find('}').expect("结构体找不到尾")];
        let mut fields = Vec::new();
        for line in body.lines() {
            let t = line
                .trim()
                .trim_start_matches("pub ")
                .trim_start_matches("pub(crate) ");
            if let Some(f) = t.split(':').next() {
                let f = f.trim();
                if !f.is_empty() && f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    fields.push(f.to_string());
                }
            }
        }
        assert_eq!(fields, want, "{marker} 字段变了");
    }
    // 命令面发出的键不许超出 DTO：start 的五个键、complete 的 digest。
    let handlers = workspace_file("crates/client/src/execute/handlers.rs");
    let seg = handler_segment(&handlers, "media.upload.start");
    for key in ["purpose", "name", "size", "mime", "digest"] {
        assert!(
            seg.contains(&format!("\"{key}\"")),
            "upload.start 没发 {key}"
        );
    }
    let seg = handler_segment(&handlers, "media.files.list");
    for key in ["category", "folder", "sort", "cursor", "limit"] {
        assert!(
            seg.contains(&format!("\"{key}\"")),
            "files.list 没带查询维度 {key}"
        );
    }
    assert!(
        !seg.contains("\"conv\""),
        "files.list 不许把 conv 当放行维度发出去（#319 红线）"
    );
}

// ---------------------------------------------------------------- 参数与标注

#[test]
fn batch_arguments_are_validated() {
    let start = catalog::get("media.upload.start").expect("在 catalog 里");
    assert_eq!(
        catalog::validate_args(start, &serde_json::json!({"purpose": "file"}))
            .expect_err("缺 name"),
        catalog::ArgError::Missing("name")
    );
    assert_eq!(
        catalog::validate_args(
            start,
            &serde_json::json!({"purpose": "file", "name": "a.pdf", "size": -1, "mime": "application/pdf"})
        )
        .expect_err("负数不是 NonNegativeInt"),
        catalog::ArgError::WrongType {
            name: "size",
            expected: "integer"
        }
    );
    assert!(catalog::validate_args(
        start,
        &serde_json::json!({"purpose": "file", "name": "a.pdf", "size": 12, "mime": "application/pdf"})
    )
    .is_ok());
    let list = catalog::get("media.files.list").expect("在 catalog 里");
    assert!(
        catalog::validate_args(list, &serde_json::json!({})).is_ok(),
        "查询全可选"
    );
}

/// 标注口径：两条读 + 五条写；全部 `ScopeClass::Media`、
/// `needs_conv: false`（对象授权不走会话白名单）；确认一律 `None`
/// （revoke 可被重新 access 抵消，不是破坏性）；幂等不声明键
/// （服务端不读键头，天然去重靠 digest 主键与会话状态机）。
#[test]
fn batch_annotations_follow_the_rules() {
    for id in BATCH_IDS {
        let spec = catalog::get(id).expect("在 catalog 里");
        assert_eq!(spec.scope, ScopeClass::Media, "{id}");
        assert!(!spec.needs_conv, "{id} 不许走会话白名单");
        assert_eq!(spec.confirmation, Confirmation::None, "{id}");
        assert_eq!(spec.idempotency, Idempotency::None, "{id}");
        assert!(
            !spec.args.iter().any(|p| p.name == "idempotency_key"),
            "{id} 不许声明幂等键"
        );
        let want_cap = if id == "media.files.list" || id == "media.upload.get" {
            Capability::Read
        } else {
            Capability::Write
        };
        assert_eq!(spec.capability, want_cap, "{id}");
    }
    assert!(CATALOG.len() >= 49);
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

// ---------------------------------------------------------------- 覆盖表

/// 覆盖表：media 域两条 blocked（#119 / #117）拆成 7 条 automated；
/// 迁移备份（#122）与拍照/录像（host_only）不在本卡，状态不许动。
#[test]
fn coverage_table_flips_and_keeps_the_rest() {
    let want: [(&str, &str); 7] = [
        ("文件中心查询", "media.files.list"),
        ("上传会话创建", "media.upload.start"),
        ("上传会话查询", "media.upload.get"),
        ("上传完成确认", "media.upload.complete"),
        ("上传会话取消", "media.upload.cancel"),
        ("对象访问授权", "media.object.access"),
        ("对象访问撤销", "media.object.revoke"),
    ];
    for (label, cmd) in want {
        let entry = entries()
            .iter()
            .find(|e| e.domain == "media" && e.label == label)
            .unwrap_or_else(|| panic!("覆盖表里没有 {label} 这条入口"));
        match &entry.state {
            EntryState::Command(id) => assert_eq!(id, cmd, "{label} 登记的命令不对"),
            other => panic!("{label} 本该是 automated（#319），现在是 {other:?}"),
        }
    }
    for old in ["文件中心与消息附件", "上传会话与对象权限"] {
        assert!(
            !entries()
                .iter()
                .any(|e| e.domain == "media" && e.label == old),
            "旧的合并条目 {old} 已被拆分，不应还留在覆盖表里"
        );
    }
    let backup = entries()
        .iter()
        .find(|e| e.domain == "media" && e.label == "聊天记录迁移与备份")
        .expect("迁移备份条目必须还在");
    assert!(
        matches!(backup.state, EntryState::Blocked { .. }),
        "迁移备份（#122）不在本卡，必须仍是 blocked"
    );
    assert!(
        entries().iter().any(|e| e.domain == "media"
            && e.label == "拍照/录像采集"
            && matches!(e.state, EntryState::Host(_))),
        "拍照/录像采集必须仍是 host_only"
    );
    // 域计数分开：media.* 与 video.* 不混（开发顺序清单的明文要求）
    let media_cmds = CATALOG
        .iter()
        .filter(|c| c.id.starts_with("media."))
        .count();
    assert_eq!(media_cmds, 7);
}

/// CLI / MCP 两面：media 域帮助与工具定义同步出现，且 scope 是 media。
#[test]
fn cli_and_mcp_surfaces_carry_the_batch() {
    let text = catalog::cli_help(Some("media"));
    for verb in [
        "files.list",
        "upload.get",
        "upload.start",
        "upload.complete",
        "upload.cancel",
        "object.access",
        "object.revoke",
    ] {
        assert!(
            text.contains(&format!("wxlike media {verb}")),
            "{verb} 不在帮助里：\n{text}"
        );
    }
    for id in BATCH_IDS {
        let t = catalog::tool_definition(catalog::get(id).expect("在 catalog 里"));
        assert_eq!(t["_meta"]["wxlike/scope"], "media", "{id}");
        assert_eq!(t["_meta"]["wxlike/confirmation"], "none", "{id}");
    }
    let size = catalog::get("media.upload.start")
        .expect("在 catalog 里")
        .args
        .iter()
        .find(|p| p.name == "size")
        .expect("声明了 size");
    assert_eq!(size.ty, ParamType::NonNegativeInt);
}
