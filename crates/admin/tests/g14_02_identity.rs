//! G14-02 验收：用户/设备/群处置与申诉工单。

#[path = "common/mod.rs"]
mod common;
#[path = "g14_common/mod.rs"]
mod g14;

use axum::http::StatusCode;
use g14::Env;
use serde_json::json;

#[tokio::test]
async fn ban_revokes_sessions_and_unban_restores_with_trail() {
    let mut env = Env::new("g14-02-ban");
    let token = env.ready("mod").await;
    env.identity.set_sessions_revoked(4);

    let (_, preview) = env
        .call(
            "POST",
            "/api/g14/identity/users/u_7/suspend",
            Some(&token),
            Some(json!({"reason":"冒充客服诈骗"})),
        )
        .await;
    let confirm = preview["result"]["confirmation"]["token"]
        .as_str()
        .unwrap()
        .to_string();

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/users/u_7/suspend",
            Some(&token),
            Some(json!({"reason":"冒充客服诈骗","confirm":confirm})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body["result"]["sessions_revoked"], 4,
        "封禁必须连带撤销会话: {body}"
    );

    // 误封恢复
    let (_, preview) = env
        .call(
            "POST",
            "/api/g14/identity/users/u_7/unsuspend",
            Some(&token),
            Some(json!({"reason":"误封：证据不足"})),
        )
        .await;
    let confirm = preview["result"]["confirmation"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/users/u_7/unsuspend",
            Some(&token),
            Some(json!({"reason":"误封：证据不足","confirm":confirm})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");

    // 两次动作都留痕，且理由可读
    let root = env.token("root");
    let (_, body) = env
        .call("GET", "/api/g14/audit?object_id=u_7", Some(&root), None)
        .await;
    let rows = body["page"]["rows"].as_array().unwrap();
    let actions: Vec<&str> = rows.iter().map(|r| r["action"].as_str().unwrap()).collect();
    assert!(
        actions.contains(&"suspend") && actions.contains(&"unsuspend"),
        "{actions:?}"
    );
    assert!(rows.iter().all(|r| r["outcome"] == "ok"));
    assert!(rows
        .iter()
        .any(|r| r["reason"].as_str().unwrap().contains("误封")));
}

#[tokio::test]
async fn confirmation_token_is_single_use() {
    let mut env = Env::new("g14-02-replay");
    let token = env.ready("mod").await;
    let (_, preview) = env
        .call(
            "POST",
            "/api/g14/identity/users/u_8/suspend",
            Some(&token),
            Some(json!({"reason":"刷屏"})),
        )
        .await;
    let confirm = preview["result"]["confirmation"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, _) = env
        .call(
            "POST",
            "/api/g14/identity/users/u_8/suspend",
            Some(&token),
            Some(json!({"reason":"刷屏","confirm":confirm})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let calls_before = env.identity.calls().len();

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/users/u_8/suspend",
            Some(&token),
            Some(json!({"reason":"刷屏","confirm":confirm})),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("无效或已使用"),
        "{body}"
    );
    assert_eq!(
        env.identity.calls().len(),
        calls_before,
        "重放不能真的再发一次命令"
    );
}

#[tokio::test]
async fn case_can_be_reviewed_end_to_end_with_reasons() {
    let mut env = Env::new("g14-02-case");
    let token = env.ready("mod").await;

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/cases",
            Some(&token),
            Some(json!({
                "kind":"report","subject_kind":"user","subject_id":"u_9",
                "reporter":"u_10","reason":"持续骚扰"
            })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let id = body["case"]["id"].as_str().unwrap().to_string();

    // 认领
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/identity/cases/{id}/transition"),
            Some(&token),
            Some(json!({"state":"triaged","reason":"已认领","assignee":"mod"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["case"]["assignee"], "mod");

    // 处置（破坏性，但窗口已开）
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/identity/cases/{id}/transition"),
            Some(&token),
            Some(json!({"state":"actioned","reason":"成立：连续 3 天骚扰"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["case"]["state"], "actioned");
    assert!(body["case"]["decision_reason"]
        .as_str()
        .unwrap()
        .contains("成立"));

    // 结案
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/identity/cases/{id}/transition"),
            Some(&token),
            Some(json!({"state":"closed","reason":"已通知举报人"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["case"]["state"], "closed");

    // 复核：审计里三步都有理由
    let root = env.token("root");
    let (_, body) = env
        .call(
            "GET",
            &format!("/api/g14/audit?object_id={id}"),
            Some(&root),
            None,
        )
        .await;
    let rows = body["page"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4, "创建 + 三次流转 = 4 条审计: {body}");
    assert!(rows
        .iter()
        .all(|r| !r["reason"].as_str().unwrap().is_empty()));
}

#[tokio::test]
async fn admin_never_sees_message_plaintext() {
    let mut env = Env::new("g14-02-e2ee");
    let token = env.ready("mod").await;
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/users/u_11/suspend",
            Some(&token),
            Some(json!({
                "reason":"传播违规内容",
                "current":{
                    "user_id":"u_11",
                    "state":"active",
                    "message":"明天下午三点见面",
                    "ciphertext":"QUJDREVGRw==",
                    "session_key":"aabbccdd"
                }
            })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let preview = &body["result"]["current"];
    assert_eq!(preview["user_id"], "u_11");
    assert_eq!(preview["dropped_fields"], 3, "{preview}");
    assert!(
        preview["content_visible"]
            .as_str()
            .unwrap()
            .contains("none"),
        "{preview}"
    );
    let dump = body.to_string();
    for leak in ["明天下午三点见面", "QUJDREVGRw==", "aabbccdd"] {
        assert!(!dump.contains(leak), "响应泄漏内容 {leak}: {dump}");
    }
}

#[tokio::test]
async fn device_revoke_and_group_govern_hit_the_domain_once() {
    let mut env = Env::new("g14-02-device");
    let token = env.ready("mod").await;

    let (_, preview) = env
        .call(
            "POST",
            "/api/g14/identity/devices/d_42/revoke",
            Some(&token),
            Some(json!({"reason":"设备丢失"})),
        )
        .await;
    let confirm = preview["result"]["confirmation"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, _) = env
        .call(
            "POST",
            "/api/g14/identity/devices/d_42/revoke",
            Some(&token),
            Some(json!({"reason":"设备丢失","confirm":confirm})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    let (_, preview) = env
        .call(
            "POST",
            "/api/g14/identity/groups/g_5/govern",
            Some(&token),
            Some(json!({"action":"freeze","reason":"群体违规"})),
        )
        .await;
    let confirm = preview["result"]["confirmation"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/groups/g_5/govern",
            Some(&token),
            Some(json!({"action":"freeze","reason":"群体违规","confirm":confirm})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");

    let calls = env.identity.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "revoke_device");
    assert_eq!(calls[1].0, "govern_group");
    assert_eq!(
        calls[1].1.verb.as_deref(),
        Some("freeze"),
        "动词必须送到域侧"
    );
}

#[tokio::test]
async fn invalid_group_verb_is_refused_before_any_call() {
    let mut env = Env::new("g14-02-verb");
    let token = env.ready("mod").await;
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/groups/g_5/govern",
            Some(&token),
            Some(json!({"action":"nuke","reason":"x"})),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(env.identity.calls().is_empty());
}

#[tokio::test]
async fn batch_suspend_reports_per_item_and_audits_each_target() {
    let mut env = Env::new("g14-02-batch");
    let token = env.ready("mod").await;
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/identity/batch",
            Some(&token),
            Some(json!({"reason":"同批刷屏账号","targets":["u_21","u_22","u_23"]})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let receipt = &body["job"]["payload"]["receipt"];
    assert_eq!(receipt["total"], 3, "{body}");
    assert_eq!(receipt["succeeded"], 3, "{body}");
    assert_eq!(body["job"]["state"], "succeeded", "{body}");

    // 逐条回执
    assert_eq!(receipt["items"].as_array().unwrap().len(), 3);
    // 逐条命令
    assert_eq!(env.identity.calls().len(), 3);

    // 逐条鉴权/审计：每个目标都要有自己的审计行（不是只记一条"批量"）
    let root = env.token("root");
    let (_, audit) = env
        .call(
            "GET",
            "/api/g14/audit?action=identity.suspend",
            Some(&root),
            None,
        )
        .await;
    let ids: Vec<&str> = audit["page"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["object_id"].as_str().unwrap())
        .collect();
    for target in ["u_21", "u_22", "u_23"] {
        assert!(
            ids.contains(&target),
            "批量里 {target} 没有独立审计: {ids:?}"
        );
    }
}
