//! G14-03 验收：内容审核、小程序发布（签名与发布分离）、服务卡片配置。

#[path = "common/mod.rs"]
mod common;
#[path = "g14_common/mod.rs"]
mod g14;

use axum::http::StatusCode;
use g14::Env;
use serde_json::{json, Value};

async fn submit(env: &Env, token: &str, idem: &str) -> Value {
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/publishing/submissions",
            Some(token),
            Some(json!({"kind":"content","subject_id":"post_1","version":"v1","idem":idem})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn queue_is_empty_at_first_and_reports_rule_versions() {
    let mut env = Env::new("g14-03-queue");
    let mod_token = env.token("mod");
    let (st, body) = env
        .call("GET", "/api/g14/publishing/queue", Some(&mod_token), None)
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["page"]["total"], 0, "{body}");

    let (st, body) = env
        .call("GET", "/api/g14/publishing/rules", Some(&mod_token), None)
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["rules"].as_array().unwrap().len(), 3, "{body}");
}

#[tokio::test]
async fn duplicate_submission_keeps_one_review_item() {
    let mut env = Env::new("g14-03-idem");
    let mod_token = env.token("mod");
    let a = submit(&env, &mod_token, "same-idem").await;
    let b = submit(&env, &mod_token, "same-idem").await;
    assert_eq!(
        a["item"]["id"], b["item"]["id"],
        "同一 idem 不能产生第二个版本"
    );
    let (_, queue) = env
        .call("GET", "/api/g14/publishing/queue", Some(&mod_token), None)
        .await;
    assert_eq!(queue["page"]["total"], 1, "{queue}");
}

#[tokio::test]
async fn takedown_and_restore_change_client_visible_state() {
    let mut env = Env::new("g14-03-takedown");
    let mod_token = env.ready("mod").await;
    let item = submit(&env, &mod_token, "td-1").await;
    let id = item["item"]["id"].as_str().unwrap().to_string();

    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/publishing/items/{id}/decide"),
            Some(&mod_token),
            Some(json!({"decision":"approve","reason":"无违规"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["item"]["state"], "approved");

    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/publishing/items/{id}/decide"),
            Some(&mod_token),
            Some(json!({"decision":"take_down","reason":"事后发现违规"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["item"]["state"], "taken_down");
    // 客户端可见状态由域侧接口改写（Feed/目录/启动授权都读它）
    let calls = env.content.calls();
    assert!(
        calls
            .iter()
            .any(|(op, d)| op == "set_visibility" && d["state"] == "taken_down"),
        "{calls:?}"
    );

    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/publishing/items/{id}/decide"),
            Some(&mod_token),
            Some(json!({"decision":"restore","reason":"申诉通过"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["item"]["state"], "approved");
    assert!(env
        .content
        .calls()
        .iter()
        .any(|(op, d)| op == "set_visibility" && d["state"] == "approved"));
}

#[tokio::test]
async fn reviewer_cannot_sign_and_signer_cannot_review() {
    let mut env = Env::new("g14-03-separation");
    let mod_token = env.token("mod");
    let pub_token = env.ready("pub").await;

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/publishing/mini/app_1/releases",
            Some(&pub_token),
            Some(json!({"version":"1.0.0","idem":"rel-1"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let id = body["release"]["id"].as_str().unwrap().to_string();

    // 审核岗签不了
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/publishing/mini/releases/{id}/sign"),
            Some(&mod_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("publisher"),
        "{body}"
    );

    // 发布岗能签（窗口已开）
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/publishing/mini/releases/{id}/sign"),
            Some(&pub_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["release"]["state"], "signed");

    // 发布岗不能自己下架（Destructive 归 superadmin）
    let (st, _) = env
        .call(
            "POST",
            &format!("/api/g14/publishing/mini/releases/{id}/revoke"),
            Some(&pub_token),
            Some(json!({"reason":"坏了"})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn publish_failure_is_retryable_without_new_version() {
    let mut env = Env::new("g14-03-publish");
    let pub_token = env.ready("pub").await;
    let (_, body) = env
        .call(
            "POST",
            "/api/g14/publishing/mini/app_2/releases",
            Some(&pub_token),
            Some(json!({"version":"2.0.0","idem":"rel-2"})),
        )
        .await;
    let id = body["release"]["id"].as_str().unwrap().to_string();
    let (st, _) = env
        .call(
            "POST",
            &format!("/api/g14/publishing/mini/releases/{id}/sign"),
            Some(&pub_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    env.content.fail_next(wxlike_admin::AdminError::upstream(
        503,
        "publish service down",
    ));
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/publishing/mini/releases/{id}/publish"),
            Some(&pub_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_GATEWAY, "{body}");

    // 状态还在 signed，可以重试
    let (st, body) = env
        .call(
            "GET",
            "/api/g14/publishing/mini/app_2/releases",
            Some(&pub_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["releases"][0]["state"], "signed", "{body}");

    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/publishing/mini/releases/{id}/publish"),
            Some(&pub_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["release"]["state"], "published");

    // 重复发布：幂等，不打上游、不多版本
    let calls_before = env.content.calls().len();
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/publishing/mini/releases/{id}/publish"),
            Some(&pub_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(env.content.calls().len(), calls_before);
    let (_, list) = env
        .call(
            "GET",
            "/api/g14/publishing/mini/app_2/releases",
            Some(&pub_token),
            None,
        )
        .await;
    assert_eq!(list["releases"].as_array().unwrap().len(), 1, "{list}");
}

#[tokio::test]
async fn service_config_refuses_plaintext_secrets_and_versions_changes() {
    let mut env = Env::new("g14-03-service");
    let pub_token = env.token("pub");

    let (st, body) = env
        .call(
            "PUT",
            "/api/g14/publishing/services/svc_x",
            Some(&pub_token),
            Some(json!({
                "title":"翻译","category":"ai","regions":["CN"],"enabled":true,
                "key_ref":"sk_live_abcdef"
            })),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("vault://"),
        "{body}"
    );

    let (st, body) = env
        .call(
            "PUT",
            "/api/g14/publishing/services/svc_x",
            Some(&pub_token),
            Some(json!({
                "title":"翻译","category":"ai","regions":["CN","SG"],"enabled":true,
                "key_ref":"vault://wxlike/translation#api_key"
            })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["service"]["version"], 1);
    assert_eq!(body["service"]["secret_configured"], true);
    assert!(
        !body.to_string().contains("sk_live"),
        "响应里不能出现密钥: {body}"
    );

    let (st, body) = env
        .call(
            "PUT",
            "/api/g14/publishing/services/svc_x",
            Some(&pub_token),
            Some(json!({
                "title":"翻译 v2","category":"ai","regions":["CN"],"enabled":false,
                "key_ref":"vault://wxlike/translation#api_key"
            })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["service"]["version"], 2, "{body}");

    let (st, body) = env
        .call(
            "GET",
            "/api/g14/publishing/services",
            Some(&pub_token),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["services"].as_array().unwrap().len(), 1);
    assert!(!body.to_string().contains("sk_live"));
}

#[tokio::test]
async fn viewer_can_read_queue_but_not_decide() {
    let mut env = Env::new("g14-03-viewer");
    let mod_token = env.token("mod");
    let view_token = env.token("view");
    let item = submit(&env, &mod_token, "v-1").await;
    let id = item["item"]["id"].as_str().unwrap().to_string();

    let (st, _) = env
        .call("GET", "/api/g14/publishing/queue", Some(&view_token), None)
        .await;
    assert_eq!(st, StatusCode::OK);

    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/publishing/items/{id}/decide"),
            Some(&view_token),
            Some(json!({"decision":"approve","reason":"看着没问题"})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("moderator"),
        "{body}"
    );
}
