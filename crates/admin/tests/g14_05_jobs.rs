//! G14-05 验收：作业重试/取消、隐私请求令牌、可观测面。

#[path = "common/mod.rs"]
mod common;
#[path = "g14_common/mod.rs"]
mod g14;

use axum::http::StatusCode;
use serde_json::json;

use g14::Env;

#[tokio::test]
async fn job_list_is_empty_then_reflects_a_batch() {
    let mut env = Env::new("g14-05-list");
    let mod_token = env.ready("mod").await;
    let (st, body) = env
        .call("GET", "/api/g14/jobs", Some(&mod_token), None)
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["page"]["total"], 0, "{body}");

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/batch",
            Some(&mod_token),
            Some(json!({"reason":"批量处置","targets":["u_31","u_32"]})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let job_id = body["job"]["id"].as_str().unwrap().to_string();

    let (st, body) = env
        .call(
            "GET",
            "/api/g14/jobs?kind=batch_identity",
            Some(&mod_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["page"]["total"], 1, "{body}");
    assert_eq!(body["page"]["rows"][0]["id"], job_id.as_str(), "{body}");
}

#[tokio::test]
async fn retry_revives_the_same_job_and_duplicate_clicks_are_harmless() {
    let mut env = Env::new("g14-05-retry");
    let mod_token = env.ready("mod").await;

    // 让第一条失败，制造一个 failed 作业
    env.identity
        .fail_next(wxlike_admin::AdminError::upstream(500, "identity exploded"));
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/batch",
            Some(&mod_token),
            Some(json!({"reason":"批量处置","targets":["u_41","u_42"]})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["job"]["state"], "failed", "{body}");
    let job_id = body["job"]["id"].as_str().unwrap().to_string();

    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/jobs/{job_id}/retry"),
            Some(&mod_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["job"]["id"], job_id.as_str(), "重试必须恢复原作业");
    assert_eq!(body["job"]["state"], "queued");

    // 连点两次：幂等，attempts 不再涨
    let (_, again) = env
        .call(
            "POST",
            &format!("/api/g14/jobs/{job_id}/retry"),
            Some(&mod_token),
            None,
        )
        .await;
    assert_eq!(again["job"]["attempts"], body["job"]["attempts"]);

    // 取消 → 终态
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/jobs/{job_id}/cancel"),
            Some(&mod_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["job"]["state"], "cancelled");
    assert_eq!(body["job"]["finished_at"].is_null(), false, "{body}");

    // 成功作业不能重试
    let (_, ok_job) = env
        .call(
            "POST",
            "/api/g14/identity/batch",
            Some(&mod_token),
            Some(json!({"reason":"批量处置","targets":["u_43"]})),
        )
        .await;
    let ok_id = ok_job["job"]["id"].as_str().unwrap().to_string();
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/jobs/{ok_id}/retry"),
            Some(&mod_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("failed/cancelled"),
        "{body}"
    );
}

#[tokio::test]
async fn privacy_export_token_is_the_only_way_to_fetch() {
    let mut env = Env::new("g14-05-privacy");
    let root = env.ready("root").await;
    let mod_token = env.token("mod");

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/privacy/requests",
            Some(&root),
            Some(json!({"user_id":"u_51","kind":"export","reason":"用户本人申请"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let id = body["request"]["id"].as_str().unwrap().to_string();
    let token = body["download_token"].as_str().unwrap().to_string();
    assert!(token.starts_with("prv_"), "{body}");

    // 列表里没有令牌明文
    let (st, list) = env
        .call("GET", "/api/g14/privacy/requests", Some(&mod_token), None)
        .await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert!(!list.to_string().contains(&token), "列表泄漏令牌: {list}");

    // 带令牌取件（不需要会话）
    let (st, body) = env
        .call(
            "GET",
            &format!("/api/g14/privacy/requests/{id}/download?token={token}"),
            None,
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["request"]["user_id"], "u_51");

    // 错令牌
    let (st, _) = env
        .call(
            "GET",
            &format!("/api/g14/privacy/requests/{id}/download?token=prv_wrong"),
            None,
            None,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // 没有令牌
    let (st, _) = env
        .call(
            "GET",
            &format!("/api/g14/privacy/requests/{id}/download"),
            None,
            None,
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // 标记交付后不能重复交付
    let (st, _) = env
        .call(
            "POST",
            &format!("/api/g14/privacy/requests/{id}/fulfill"),
            Some(&mod_token),
            Some(json!({"reason":"已交付"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/privacy/requests/{id}/fulfill"),
            Some(&mod_token),
            Some(json!({"reason":"再交付"})),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test]
async fn privacy_requires_step_up_and_the_right_role() {
    let mut env = Env::new("g14-05-privacy-rbac");
    let root = env.token("root"); // 没有步进验证
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/privacy/requests",
            Some(&root),
            Some(json!({"user_id":"u_52","kind":"export","reason":"申请"})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert!(body["error"].as_str().unwrap().contains("stepup"), "{body}");

    // 只读岗看不到隐私请求列表
    let view = env.token("view");
    let (st, _) = env
        .call("GET", "/api/g14/privacy/requests", Some(&view), None)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn health_summary_checks_the_audit_chain() {
    let mut env = Env::new("g14-05-health");
    let root = env.ready("root").await;
    // 造一条审计
    let (st, _) = env
        .call(
            "POST",
            "/api/g14/privacy/requests",
            Some(&root),
            Some(json!({"user_id":"u_61","kind":"delete","reason":"用户注销申请"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    let (st, body) = env
        .call("GET", "/api/g14/health/summary", Some(&root), None)
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["health"]["ok"], true, "{body}");
    assert_eq!(body["health"]["audit_chain_ok"], true, "{body}");
    assert!(
        body["health"]["audit_entries"].as_i64().unwrap() >= 1,
        "{body}"
    );
    assert_eq!(body["health"]["counts"]["privacy_requests"], 1, "{body}");
    assert!(
        body["health"]["withheld"]
            .as_str()
            .unwrap()
            .contains("密钥"),
        "{body}"
    );
}
