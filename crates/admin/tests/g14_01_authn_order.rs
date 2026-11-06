//! G14-01 的**顺序契约**用例（#245 在 adminx 自己的管理面上的落地）。
//!
//! #245 的原始事实是：管理面鉴权写在 handler 体内第一行，而 axum 的参数提取
//! 发生在 handler 体**之前**，于是 `ApiJson` 先跑——未认证的人先拿到 422，
//! 422 的消息里带着请求体字段名（`act` 那条甚至把可选动作列全了）。
//!
//! 本文件钉住四件事：
//!
//! 1. **未认证 → 401/403，永远不是 422**：adminx 每一条会读 body 的管理端点，
//!    空凭据 + `{}` / 垃圾 body 都必须在凭据这一步被拦下，响应里不出现任何
//!    请求体契约信息（字段名、可选值、`missing field`）。
//! 2. **反过来也要成立**：带着合法会话时，缺字段仍然给 422——
//!    说明"先认证"不是把参数校验顺手删掉，运维照样看得懂怎么改。
//! 3. **公开面是精确的三条**（`/`、`/health`、`/api/login`）加一条按形状放行的
//!    隐私交付路由；`/healthz` 这种"前缀像"的路径必须仍在门后。
//! 4. **留痕写不上不等于闸门失效**：审计表被拆掉时越权尝试照旧 403，
//!    而失败次数在 `/api/g14/audit/verify` 的 `append_failures` 里看得见。
//!
//! 这里没有 mock：会话是真的、审计真的落库、路由就是 `App::router()` 本体。

#[path = "common/mod.rs"]
mod common;
#[path = "g14_common/mod.rs"]
mod g14;

use axum::http::StatusCode;
use g14::Env;
use serde_json::{json, Value};
use wxlike_admin::Store;

/// adminx 全部"会读请求体"的管理端点（`{}` 占位已换成具体 id）。
///
/// 新登记一条路由就要在这里加一行：`every_body_route_is_registered` 会用
/// **带凭据**的请求逐条确认"不是 404"，改名或漏登记都会红——
/// 否则这份表会和路由表悄悄分叉，而分叉的代价是没人发现的鉴权顺序问题。
const BODY_ROUTES: &[(&str, &str)] = &[
    // 会话与老操作目录（http.rs）
    ("POST", "/api/logout"),
    ("POST", "/api/me/totp/enroll"),
    ("POST", "/api/me/totp/confirm"),
    ("POST", "/api/me/totp/disable"),
    ("POST", "/api/op/report.triage"),
    ("POST", "/api/op/moderation.mode"),
    // 本卡：步进验证、审计
    ("POST", "/api/g14/session/stepup"),
    // G14-02：身份
    ("POST", "/api/g14/identity/cases"),
    ("POST", "/api/g14/identity/cases/c1/transition"),
    ("POST", "/api/g14/identity/users/u1/suspend"),
    ("POST", "/api/g14/identity/users/u1/unsuspend"),
    ("POST", "/api/g14/identity/devices/d1/revoke"),
    ("POST", "/api/g14/identity/groups/g1/govern"),
    ("POST", "/api/g14/identity/batch"),
    // G14-03：发布
    ("POST", "/api/g14/publishing/submissions"),
    ("POST", "/api/g14/publishing/items/i1/decide"),
    ("POST", "/api/g14/publishing/mini/a1/releases"),
    ("POST", "/api/g14/publishing/mini/releases/r1/sign"),
    ("POST", "/api/g14/publishing/mini/releases/r1/publish"),
    ("POST", "/api/g14/publishing/mini/releases/r1/revoke"),
    ("PUT", "/api/g14/publishing/services/s1"),
    // G14-04：商务
    ("POST", "/api/g14/commerce/orders/o1/refunds"),
    ("POST", "/api/g14/commerce/refunds/f1/approve"),
    ("POST", "/api/g14/commerce/recon/x1/resolve"),
    ("POST", "/api/g14/commerce/journal"),
    // G14-05：作业与隐私
    ("POST", "/api/g14/jobs/j1/retry"),
    ("POST", "/api/g14/jobs/j1/cancel"),
    ("POST", "/api/g14/privacy/requests"),
    ("POST", "/api/g14/privacy/requests/p1/fulfill"),
];

/// 请求体契约信息：出现在未认证响应里就是泄漏（#245 的原始危害形态）。
const CONTRACT_HINTS: &[&str] = &[
    "missing field",
    "code",
    "reason",
    "note",
    "targets",
    "decision",
    "state",
    "kind",
    "subject_id",
    "subject_kind",
    "version",
    "idem",
    "amount_cents",
    "entry_no",
    "user_id",
    "mode",
    "action",
    "target",
    "lines",
    "memo",
    "source",
    "confirm",
    "until_ms",
    "appeal_of",
    "reporter",
];

fn denial(status: StatusCode) -> bool {
    matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
}

/// 1 + 3：所有 body 端点在无凭据、垃圾 body 下都只会得到 401/403。
#[tokio::test]
async fn unauthenticated_body_never_reaches_the_422_path() {
    let mut env = Env::new("g14-01-order-unauth");
    let root = env.token("root");
    let mut checked = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (method, path) in BODY_ROUTES {
        for (label, body) in [
            ("空凭据+{}", None),
            ("空凭据+垃圾 body", Some(json!({"zzz": 1}))),
            ("空凭据+缺字段", Some(json!({"note": "x"}))),
        ] {
            let (status, resp) = env.call(method, path, None, body).await;
            checked += 1;
            if !denial(status) {
                failures.push(format!("{method} {path} {label} → {status} {resp}"));
                continue;
            }
            // 401/403 的正文只许说一句"你是谁/你不是后台的人"。
            if let Value::Object(map) = &resp {
                let text = map
                    .values()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ");
                if let Some(hint) = CONTRACT_HINTS.iter().find(|h| text.contains(*h)) {
                    failures.push(format!(
                        "{method} {path} {label} → {status} 的拒绝消息里出现了请求体字段 {hint:?}：{text}"
                    ));
                }
            }
        }
        // 终端用户令牌同样不许摸到参数层（403，理由已经由 audience 给出）
        let (status, resp) = env
            .call(method, path, Some("wxu_deadbeef"), Some(json!({})))
            .await;
        checked += 1;
        if !denial(status) {
            failures.push(format!("{method} {path} 用户令牌 → {status} {resp}"));
        }
    }

    assert!(
        failures.is_empty(),
        "以下管理端点在未认证时没有停在凭据这一步：\n{}",
        failures.join("\n")
    );
    assert!(
        checked >= BODY_ROUTES.len() * 4,
        "只扫到 {checked} 次：登记表不全或路由形状变了"
    );
    // 表本身不能和路由分叉：登记过的路径必须真实存在（404 = 改名了）。
    for (method, path) in BODY_ROUTES {
        let (status, resp) = env.call(method, path, Some(&root), Some(json!({}))).await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "{method} {path} 已不在路由表里，请把 BODY_ROUTES 一起改：{resp}"
        );
    }
}

/// 2：合法会话下缺字段仍然是 422，而且消息会说清该带什么——
/// "先认证"不能把参数校验一起砍掉。
#[tokio::test]
async fn authenticated_requests_still_get_actionable_parameter_errors() {
    let mut env = Env::new("g14-01-order-422");
    let root = env.token("root");
    let (status, resp) = env
        .call(
            "POST",
            "/api/g14/session/stepup",
            Some(&root),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
    let msg = resp["error"].as_str().unwrap_or_default();
    assert!(msg.contains("missing field"), "{msg}");
    assert!(msg.contains("code"), "缺字段的消息要说出字段名：{msg}");

    // 未认证时同一条路由的同一份错误必须看不见
    let (status, resp) = env
        .call("POST", "/api/g14/session/stepup", None, Some(json!({})))
        .await;
    assert!(denial(status), "{status} {resp}");
    assert!(
        !resp.to_string().contains("code"),
        "未认证拿到了请求体契约：{resp}"
    );
}

/// 3：门外的只有三条精确路径 + 一条按形状判定的隐私交付路由。
#[tokio::test]
async fn public_surface_is_exact_and_everything_else_is_behind_the_gate() {
    let env = Env::new("g14-01-order-public");

    // /health 与登录页：不需要凭据
    let (status, _) = env.call("GET", "/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = env.call("GET", "/", None, None).await;
    assert_eq!(status, StatusCode::OK);
    // 登录是换凭据的地方：错口令给 401（凭据错），不能被门变成"没带凭据"
    let (status, resp) = env
        .call(
            "POST",
            "/api/login",
            None,
            Some(json!({"username": "root", "password": "wrong-password"})),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{resp}");
    assert!(
        !resp.to_string().contains("独立 audience"),
        "登录面不该被 audience 判定拦：{resp}"
    );

    // 其余一律 401（没带凭据），而且不是 404（门在路由之前）
    for (method, path) in [
        ("GET", "/api/g14/audit"),
        ("GET", "/api/g14/auth/matrix"),
        ("GET", "/api/g14/health/summary"),
        ("GET", "/api/ops"),
        ("GET", "/api/me"),
        ("GET", "/healthz"),
        ("GET", "/api/login/other"),
        ("POST", "/api/g14/privacy/requests/p1/fulfill"),
        ("GET", "/api/g14/privacy/requests/p1"),
        ("GET", "/api/definitely-not-a-route"),
    ] {
        let (status, resp) = env.call(method, path, None, None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{method} {path} 应当在门后：{status} {resp}"
        );
    }
}

/// 3 的另一半：隐私交付路由在门外，但**不是裸奔**——它只认一次性令牌，
/// 带后台会话也不能代替它（否则"门外的路由"就成了绕过 step-up 的侧门）。
#[tokio::test]
async fn privacy_download_stays_token_bound() {
    let mut env = Env::new("g14-01-order-privacy");
    let root = env.ready("root").await;

    let (status, body) = env
        .call(
            "POST",
            "/api/g14/privacy/requests",
            Some(&root),
            Some(json!({"user_id": "u_51", "kind": "export", "reason": "用户本人申请"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["request"]["id"].as_str().unwrap().to_string();
    let token = body["download_token"].as_str().unwrap().to_string();

    // 缺令牌：422 说的是"这条路由自己的凭据"，不是后台会话
    let (status, resp) = env
        .call(
            "GET",
            &format!("/api/g14/privacy/requests/{id}/download"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
    assert!(resp["error"].as_str().unwrap().contains("token"), "{resp}");

    // 带着后台会话仍然不给数据：会话不是这个面的凭据
    let (status, resp) = env
        .call(
            "GET",
            &format!("/api/g14/privacy/requests/{id}/download"),
            Some(&root),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");

    // 错令牌：403，而且不回显调用方给的串
    let (status, resp) = env
        .call(
            "GET",
            &format!("/api/g14/privacy/requests/{id}/download?token=prv_wrong"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{resp}");
    assert!(
        !resp.to_string().contains("prv_wrong"),
        "响应不许回显调用方给的令牌：{resp}"
    );

    // 不存在的单子：404（先于令牌判定），同样给不出任何包内容
    let (status, resp) = env
        .call(
            "GET",
            "/api/g14/privacy/requests/p1/download?token=prv_wrong",
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{resp}");

    // 正确的令牌才放行——这正是"它在门外"的唯一理由：凭据换了人
    let (status, body) = env
        .call(
            "GET",
            &format!("/api/g14/privacy/requests/{id}/download?token={token}"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["request"]["user_id"], "u_51");
}

/// 4a：静态运营令牌的写尝试也是越权尝试，**必须**进审计链。
///
/// 之前这条路径直接 `return Err(403)`，一个审计行都不写——于是
/// "运维令牌被人捡到之后试过哪些动作"这条线索根本不存在。
#[tokio::test]
async fn operator_token_write_attempt_leaves_a_denied_audit_row() {
    let mut env = Env::new("g14-01-order-operator");
    let operator = env.app.config().token.clone();
    let root = env.token("root");

    let (status, resp) = env
        .call(
            "POST",
            "/api/g14/identity/cases",
            Some(&operator),
            Some(json!({
                "kind": "appeal",
                "subject_kind": "user",
                "subject_id": "u1",
                "reason": "静态令牌想开工单",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{resp}");

    let (status, page) = env
        .call(
            "GET",
            "/api/g14/audit?actor=operator-token&action=g14.denied",
            Some(&root),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["page"]["total"], 1, "{page}");
    let row = &page["page"]["rows"][0];
    assert_eq!(row["outcome"], "denied");
    assert_eq!(
        row["role"], "operator",
        "静态令牌没有角色，写清 audience：{row}"
    );
    assert_eq!(row["object_kind"], "case");
    assert_eq!(row["detail"]["action"], "write", "{row}");
    // 理由与 trace 都在：没有这两样就不是审计，是流水账
    assert_eq!(row["reason"], "权限不足");
    assert!(row["trace"].as_str().unwrap().starts_with("g14_"), "{row}");
}

/// 4b：审计表被拆掉时，**闸门照旧落下**（403 不会被留痕故障换掉），
/// 但掉了几条留痕必须能被运维看到（`/api/g14/audit/verify`）。
#[tokio::test]
async fn denial_stands_and_is_counted_when_the_audit_table_is_gone() {
    let mut env = Env::new("g14-01-order-broken-audit");
    let view = env.token("view");

    // 先证明计数是 0：否则下面的 >=1 可能一直为真（用例自己就成了空转）
    let (status, before) = env
        .call("GET", "/api/g14/audit/verify", Some(&view), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{before}");
    assert_eq!(before["chain"]["append_failures"], 0, "{before}");

    env.app.store().rollback_all().unwrap(); // 拆掉全部 g14_* 表

    let (status, resp) = env
        .call(
            "POST",
            "/api/g14/identity/cases",
            Some(&view),
            Some(json!({
                "kind": "appeal",
                "subject_kind": "user",
                "subject_id": "u1",
                "reason": "只读岗位想开工单",
            })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "留痕写不上不能把 403 变成别的：{resp}"
    );
    assert!(
        !resp.to_string().contains("g14_audit"),
        "拒绝消息里不许出现表名：{resp}"
    );

    // 重新迁移（同库文件，另一个连接），再问健康面
    Store::open(&env.accounts_db()).unwrap();
    let (status, after) = env
        .call("GET", "/api/g14/audit/verify", Some(&view), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(after["chain"]["entries"], 0, "表是重建的，链应当为空");
    assert!(
        after["chain"]["append_failures"].as_u64().unwrap_or(0) >= 1,
        "留痕失败必须被数出来：{after}"
    );
}
