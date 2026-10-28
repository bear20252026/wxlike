//! **写命令标注不变式**（#315）。
//!
//! 这个文件兑现 `crates/cmd/src/catalog.rs` 顶部"写命令的标注规则"里那两条
//! 被注释引用的不变式——注释点名了这个文件，文件就必须真的守这两条：
//!
//! 1. **声明 `confirmation_token` ⇔ `Confirmation::Required`**（双向）。
//!    多声明：读命令的令牌根本签发不出来（`ConfirmationStore::prepare` 对
//!    非 Required 直接回 `NotRequired`），MCP 的 `confirm_step` 也只在
//!    `spec.confirmation.is_required()` 时才走（`crates/mcp/src/server.rs:278`），
//!    留着等于让调用方以为"要先确认"；少声明：Required 的命令没有带回令牌的
//!    参数，链路走不通。`msg.get` 就栽在第一条上（#315 撤销）。
//!
//! 2. **声明 `idempotency_key` ⇒ 执行器真的把它发出去**（外加 ⇔ 的另半：
//!    只有 `Idempotency::Required` 才许声明）。规则要求服务端有**真的消费**
//!    （读 `Idempotency-Key` 头重放，或把键派生成业务 id），而执行器侧必须
//!    经 `Http::post_idempotent` 把键带上（头 + body），否则声明就是装饰。
//!    `param_dto_parity` 抓的是"handler 取没取用"；这里抓得更狠一层：
//!    **取用了还得真的走上发幂等头的通道**。取用与发送都只认源码事实：
//!    执行器没有 per-command 的 DTO，真相写在 `execute/handlers.rs` 的文本里。
//!
//! 两条都是**双向断言**：将来加新写命令时，标错方向立刻红；
//! 撤掉声明后条目失效同样红（见 `idempotency_key_params_always_use_post_idempotent`
//! 对"零命中"的显式拒绝）。

use wxlike_cmd::CATALOG;

fn workspace_file(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/cmd 的上两级必须是 workspace 根")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读不到 {}：{e}", path.display()))
}

fn declares(spec: &wxlike_cmd::CommandSpec, name: &str) -> bool {
    spec.args.iter().any(|p| p.name == name)
}

/// 不变式 1：`confirmation_token` 参数与 `Confirmation::Required` 互为充要。
#[test]
fn confirmation_token_param_iff_confirmation_required() {
    for spec in CATALOG {
        assert_eq!(
            declares(spec, "confirmation_token"),
            spec.confirmation.is_required(),
            "{}：confirmation_token 声明了 {}，标注是 {:?}",
            spec.id,
            declares(spec, "confirmation_token"),
            spec.confirmation
        );
    }
    // 面不能空转：确认链路当前确实有用户（全为 None 说明规则被整体绕过）。
    assert!(
        CATALOG
            .iter()
            .any(|s| s.confirmation == wxlike_cmd::Confirmation::Required),
        "catalog 里没有任何 Confirmation::Required，本测试的前提变了"
    );
}

/// 不变式 2a：`idempotency_key` 参数与 `Idempotency::Required` 互为充要。
/// 规则（catalog 顶部）：标注 Required 的两条件缺一条就**连参数都不许声明**；
/// 反过来标了 Required 就必须让调用方能递键。
#[test]
fn idempotency_key_param_iff_idempotency_required() {
    for spec in CATALOG {
        assert_eq!(
            declares(spec, "idempotency_key"),
            spec.idempotency == wxlike_cmd::Idempotency::Required,
            "{}：idempotency_key 声明了 {}，标注是 {:?}",
            spec.id,
            declares(spec, "idempotency_key"),
            spec.idempotency
        );
    }
}

/// 分派表里一条命令的 handler 原文：从 `("id",` 起，到下一条 `("…` 或表尾 `];` 止。
/// 表条目形态变了（不再是 `    ("id", |ex, http, p| {`）就 panic，
/// 不许让扫描静默扫空变成常绿。
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

/// 不变式 2b：声明了 `idempotency_key` 的命令，handler 必须**取用键**并走
/// `Http::post_idempotent`（它才把 `Idempotency-Key` 头和 body 一起发出去）。
#[test]
fn idempotency_key_params_always_use_post_idempotent() {
    let handlers = workspace_file("crates/client/src/execute/handlers.rs");
    let mut checked = 0usize;
    for spec in CATALOG {
        if !declares(spec, "idempotency_key") {
            continue;
        }
        checked += 1;
        let seg = handler_segment(&handlers, spec.id);
        assert!(
            seg.contains("idempotency_key"),
            "{}：handler 一次也没取用 idempotency_key，声明是装饰",
            spec.id
        );
        assert!(
            seg.contains("post_idempotent("),
            "{}：取用了键却没走 post_idempotent，头不会带上，声明仍是装饰",
            spec.id
        );
    }
    assert!(
        checked > 0,
        "没有任何命令声明 idempotency_key——键链路被整体拆除，本测试失去对象，\
         必须连同 catalog 顶部的规则注释一起重新审视后再删它"
    );
}

/// 不变式 2c：`post_idempotent` 本身确实把键放上**头**与**body**之外的通道——
/// 头。这里只钉"头真的被设置"这一处事实（body 由各 handler 自己拼，2b 已查取用）。
#[test]
fn post_idempotent_actually_sets_the_header() {
    let lib = workspace_file("crates/client/src/lib.rs");
    let start = lib
        .find("pub fn post_idempotent")
        .expect("client 的 post_idempotent 搬了家？同步更新本测试");
    let seg = &lib[start..start + 1200];
    assert!(
        seg.contains(".header(\"Idempotency-Key\""),
        "post_idempotent 不再设置 Idempotency-Key 头：所有键声明同时变成装饰"
    );
}

/// 反面兜底：**没**标 Required 的命令不许偷偷用 `post_idempotent`。
/// 混用会让"这条命令要不要键"在命令面与服务端之间各说一套。
#[test]
fn optional_key_commands_never_use_post_idempotent() {
    let handlers = workspace_file("crates/client/src/execute/handlers.rs");
    for spec in CATALOG {
        if spec.idempotency == wxlike_cmd::Idempotency::Required {
            continue;
        }
        let seg = handler_segment(&handlers, spec.id);
        assert!(
            !seg.contains("post_idempotent("),
            "{}：标注是 {:?} 却走了 post_idempotent，命令面与实现脱节",
            spec.id,
            spec.idempotency
        );
    }
}
