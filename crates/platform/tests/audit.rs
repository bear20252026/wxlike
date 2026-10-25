//! 命令审计：带 `X-Wxlike-Command` 的请求必须留痕。
//!
//! 这条链路的三个环节都容易被写漏，所以测试盯的是**端到端**：
//! 请求头 → 中间件解析 token → 落 `audit_log` → 用户能从 `/api/audit/self` 读回。
//!
//! 为什么审计要在服务端：agent 可能通过 CLI、MCP、UI 任意入口重试，
//! 客户端自报的"我执行过什么"不可信；只有服务端在请求路径上记的那一条算数。

mod common;

use common::{app, req, signup};
use serde_json::json;

#[tokio::test]
async fn command_header_is_audited_and_readable() {
    let router = app(common::platform());
    let (alice, ta) = signup(&router, "alice").await;

    // 一条只读命令：平台层不判断命令权限（那是 policy 的事），只看请求。
    // conv 必须是 alice 参与的房间——AUTHZ-012 之后非参与者一律 403，
    // 审计测试要验的是"命令头落审计"，别把它写在别人的房间里。
    let conv = format!("d-{alice}~b");
    let mut r = req("GET", &format!("/api/conv/{conv}/unread"), None, Some(&ta));
    r.headers_mut()
        .insert("x-wxlike-command", "conv.unread".parse().unwrap());
    let (status, _) = common::call(&router, r).await;
    assert_eq!(status, 200);

    let r = req("GET", "/api/audit/self", None, Some(&ta));
    let (status, body) = common::call(&router, r).await;
    assert_eq!(status, 200, "{body}");
    let entries = body["entries"].as_array().expect("entries");
    let hit = entries
        .iter()
        .find(|e| e["action"] == json!("cmd.conv.unread"))
        .unwrap_or_else(|| panic!("没有 cmd.conv.unread 审计：{body}"));
    assert_eq!(hit["target"], json!(format!("/api/conv/{conv}/unread")));
    assert!(
        hit["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("status=200"),
        "{hit}"
    );
}

/// 没有命令头的普通请求不该产生审计——否则日志会被健康检查淹掉。
#[tokio::test]
async fn requests_without_the_header_are_not_audited() {
    let router = app(common::platform());
    let (_alice, ta) = signup(&router, "alice").await;

    let r = req("GET", "/api/me", None, Some(&ta));
    let (status, _) = common::call(&router, r).await;
    assert_eq!(status, 200);

    let r = req("GET", "/api/audit/self", None, Some(&ta));
    let (_, body) = common::call(&router, r).await;
    let entries = body["entries"].as_array().cloned().unwrap_or_default();
    assert!(
        !entries.iter().any(|e| e["action"] == json!("cmd.me.info")),
        "没带命令头却记了审计：{body}"
    );
}

/// 失败的写命令同样要留痕——审计的价值恰恰在于"它试过但被拒了"。
#[tokio::test]
async fn failed_commands_are_audited_too() {
    let router = app(common::platform());
    let (_alice, ta) = signup(&router, "alice").await;

    // 建群缺 name → 400；但审计必须已经落下
    let mut r = req(
        "POST",
        "/api/groups",
        Some(json!({"name": "", "member_ids": []})),
        Some(&ta),
    );
    r.headers_mut()
        .insert("x-wxlike-command", "group.create".parse().unwrap());
    let (status, _) = common::call(&router, r).await;
    assert!(status.is_client_error(), "应当被拒: {status}");

    let r = req("GET", "/api/audit/self", None, Some(&ta));
    let (_, body) = common::call(&router, r).await;
    let hit = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["action"] == json!("cmd.group.create"))
        .cloned();
    let hit = hit.unwrap_or_else(|| panic!("失败的命令没留痕：{body}"));
    assert!(
        hit["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("status=4"),
        "{hit}"
    );
}
