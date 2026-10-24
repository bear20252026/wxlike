//! G14-01 验收：管理身份（audience / 资源权限 / step-up）、审计骨架。
//!
//! 每个用例对应任务单验收面的一条，不用 mock 通过：会话是真的、审计真的落库、
//! 只有域侧端口是 fixture（上游卡未合入）。

#[path = "common/mod.rs"]
mod common;
#[path = "g14_common/mod.rs"]
mod g14;

use axum::http::StatusCode;
use g14::Env;
use serde_json::json;

#[tokio::test]
async fn end_user_tokens_and_unknown_tokens_are_refused() {
    let mut env = Env::new("g14-01-audience");
    let root = env.token("root");

    // 终端用户令牌：403，且理由说清"走错门"，不是"密码错"
    let (st, body) = env
        .call("GET", "/api/g14/audit", Some("wxu_deadbeef"), None)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("独立 audience"),
        "{body}"
    );

    // 认不出的串：401
    let (st, _) = env
        .call("GET", "/api/g14/audit", Some("nonsense-token"), None)
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // 完全不带凭据：401
    let (st, _) = env.call("GET", "/api/g14/audit", None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // 后台会话：200
    let (st, body) = env.call("GET", "/api/g14/audit", Some(&root), None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["page"]["total"], 0, "空结果也是 200，不是 404: {body}");
}

#[tokio::test]
async fn operator_static_token_reads_but_never_writes() {
    let env = Env::new("g14-01-operator");
    let op = env.app.config().token.clone();

    let (st, body) = env
        .call("GET", "/api/g14/auth/matrix", Some(&op), None)
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(body["grants"].as_array().unwrap().len() > 10, "{body}");

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/publishing/submissions",
            Some(&op),
            Some(json!({"kind":"content","subject_id":"p1","version":"v1","idem":"k"})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("静态令牌"),
        "静态令牌没有写权限，理由要说清: {body}"
    );
}

#[tokio::test]
async fn read_only_role_cannot_write_and_the_denial_is_audited() {
    let mut env = Env::new("g14-01-rbac");
    let view = env.token("view");
    let root = env.token("root");

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/cases",
            Some(&view),
            Some(json!({
                "kind":"report","subject_kind":"user","subject_id":"u_1","reason":"骚扰"
            })),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("moderator"),
        "拒绝理由要指到能做的角色: {body}"
    );

    // 越权尝试必须留痕
    let (st, body) = env
        .call("GET", "/api/g14/audit?action=g14.denied", Some(&root), None)
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let rows = body["page"]["rows"].as_array().unwrap();
    assert!(!rows.is_empty(), "越权尝试没有留痕: {body}");
    assert_eq!(rows[0]["actor"], "view");
    assert_eq!(rows[0]["outcome"], "denied");
}

#[tokio::test]
async fn sensitive_action_needs_step_up_and_records_before_after() {
    let mut env = Env::new("g14-01-stepup");
    let mod_token = env.token("mod");
    let root = env.token("root");

    // 没步进验证：403 + 说清怎么开窗口
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/users/u_100/suspend",
            Some(&mod_token),
            Some(json!({"reason":"冒充客服"})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"].as_str().unwrap().contains("stepup"), "{body}");

    // 开窗口后：第一次只给预览
    env.step_up("mod").await;
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/users/u_100/suspend",
            Some(&mod_token),
            Some(json!({
                "reason":"冒充客服",
                "current":{"user_id":"u_100","state":"active","message":"私聊原文不要进审计"}
            })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["stage"], "preview", "{body}");
    let confirm = body["result"]["confirmation"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    // 预览里不含正文
    assert!(
        !body.to_string().contains("私聊原文"),
        "预览泄漏正文: {body}"
    );

    // 带令牌执行
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/users/u_100/suspend",
            Some(&mod_token),
            Some(json!({"reason":"冒充客服","confirm":confirm})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["stage"], "done", "{body}");
    assert_eq!(body["result"]["sessions_revoked"], 0, "fixture 默认 0");

    // 审计：有 actor/理由/对象/前后摘要/trace，且没有正文
    let (st, body) = env
        .call("GET", "/api/g14/audit?action=suspend", Some(&root), None)
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let row = &body["page"]["rows"][0];
    assert_eq!(row["actor"], "mod");
    assert_eq!(row["object_kind"], "user");
    assert_eq!(row["object_id"], "u_100");
    assert_eq!(row["reason"], "冒充客服");
    assert!(!row["trace"].as_str().unwrap().is_empty());
    assert!(!row["after_digest"].as_str().unwrap().is_empty());
    assert!(
        !row.to_string().contains("私聊原文"),
        "审计里出现了正文: {row}"
    );
}

#[tokio::test]
async fn audit_chain_verifies_and_tampering_would_show_up() {
    let mut env = Env::new("g14-01-chain");
    let root = env.token("root");
    let mod_token = env.ready("mod").await;

    for id in ["u_1", "u_2", "u_3"] {
        let (_, body) = env
            .call(
                "POST",
                &format!("/api/g14/identity/users/{id}/suspend"),
                Some(&mod_token),
                Some(json!({"reason":"批量刷屏"})),
            )
            .await;
        let confirm = body["result"]["confirmation"]["token"]
            .as_str()
            .unwrap()
            .to_string();
        let (st, body) = env
            .call(
                "POST",
                &format!("/api/g14/identity/users/{id}/suspend"),
                Some(&mod_token),
                Some(json!({"reason":"批量刷屏","confirm":confirm})),
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{body}");
    }

    let (st, body) = env
        .call("GET", "/api/g14/audit/verify", Some(&root), None)
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["chain"]["entries"], 3);
    assert!(body["chain"]["broken_at"].is_null(), "{body}");
    assert!(!body["chain"]["head_hash"].as_str().unwrap().is_empty());

    // 过滤：按对象查只回一条
    let (_, body) = env
        .call("GET", "/api/g14/audit?object_id=u_2", Some(&root), None)
        .await;
    assert_eq!(body["page"]["total"], 1, "{body}");
    // 空结果
    let (_, body) = env
        .call("GET", "/api/g14/audit?object_id=nobody", Some(&root), None)
        .await;
    assert_eq!(body["page"]["total"], 0, "{body}");
}

#[tokio::test]
async fn parameter_errors_are_422_with_the_fix_in_the_message() {
    let mut env = Env::new("g14-01-params");
    let mod_token = env.token("mod");

    // 理由为空
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/cases",
            Some(&mod_token),
            Some(json!({"kind":"report","subject_kind":"user","subject_id":"u_1","reason":"  "})),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body["error"].as_str().unwrap().contains("理由"), "{body}");

    // 非法状态
    let (_, created) = env
        .call(
            "POST",
            "/api/g14/identity/cases",
            Some(&mod_token),
            Some(json!({"kind":"report","subject_kind":"user","subject_id":"u_1","reason":"骚扰"})),
        )
        .await;
    let id = created["case"]["id"].as_str().unwrap();
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/identity/cases/{id}/transition"),
            Some(&mod_token),
            Some(json!({"state":"bogus","reason":"x"})),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("triaged"),
        "{body}"
    );
}

#[tokio::test]
async fn upstream_timeout_is_502_and_leaves_a_failed_audit_row() {
    let mut env = Env::new("g14-01-upstream");
    let mod_token = env.ready("mod").await;
    let root = env.token("root");

    // 预览拿令牌
    let (_, body) = env
        .call(
            "POST",
            "/api/g14/identity/devices/d_1/revoke",
            Some(&mod_token),
            Some(json!({"reason":"设备丢失"})),
        )
        .await;
    let confirm = body["result"]["confirmation"]["token"]
        .as_str()
        .unwrap()
        .to_string();

    // 域侧超时（status 0 = 传输层失败）
    env.identity
        .fail_next(wxlike_admin::AdminError::upstream(0, "connect timeout"));
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/devices/d_1/revoke",
            Some(&mod_token),
            Some(json!({"reason":"设备丢失","confirm":confirm})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{body}");

    let (_, body) = env
        .call(
            "GET",
            "/api/g14/audit?action=revoke_device",
            Some(&root),
            None,
        )
        .await;
    let rows = body["page"]["rows"].as_array().unwrap();
    assert!(
        rows.iter().any(|r| r["outcome"] == "failed"),
        "上游失败必须留痕: {body}"
    );
}

#[tokio::test]
async fn health_summary_never_leaks_secrets() {
    let mut env = Env::new("g14-01-health");
    let root = env.token("root");
    let (st, body) = env
        .call("GET", "/api/g14/health/summary", Some(&root), None)
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["health"]["audit_chain_ok"], true, "{body}");
    assert_eq!(body["health"]["counts"]["cases"], 0, "{body}");
    let dump = body.to_string();
    for leak in ["read-token-0123456789abcdef", "password", "vault://"] {
        assert!(!dump.contains(leak), "健康面泄漏 {leak}: {dump}");
    }
}
