//! D3 验收：RBAC + MFA + 每次写都留痕。
//!
//! 卡的验收栏：**"没有 MFA 的管理员不能执行任何写操作；每次写都留痕"**。
//! 这两句各有一个"看起来能过"的假实现，所以都做了反向证明：
//!
//! * 闸门不是"adminx 返回了错误"就算过——还要证明**平台根本没被碰到**
//!   （对账跑没跑，看 `reconcile_runs` 有没有新行；工单状态没变）；
//! * 留痕不是"有一行"就算过——要看 actor / op / capability / target /
//!   上游路径 / 结果，被拒的尝试也得在。
//!
//! 还有一个只有"独立进程"才需要操心的问题，这里也钉死了：**内存态**。
//! 审核模式活在平台进程的 `Mutex<Pipeline>` 里。如果 adminx 绕过 HTTP 直接写库，
//! 库里根本没有这一列，面板会显示"已切换"而服务其实没切——正是本项目禁止的
//! 静默降级。`mode_switch_reaches_the_running_process` 就是这条的锚。

mod common;

use axum::http::StatusCode;
use serde_json::json;

use common::{
    call, current_totp_code, live, login, login_mfa, req, ADMIN_TOKEN, PLATFORM_TOKEN, PW,
};
use wxlike_admin::rbac::{Capability, Role, OPS};

// ---------------------------------------------------------------------------
// MFA 门（D3 的验收面）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_mfa_means_no_write_for_every_role() {
    let (_dir, _p, app, _base) = live("d3-no-mfa").await;
    let router = app.router();
    // 三个角色都试：没验码的会话一律 403，且平台完全没被动过
    for (name, role) in [
        ("v-no-mfa", Role::Viewer),
        ("m-no-mfa", Role::Moderator),
        ("s-no-mfa", Role::Superadmin),
    ] {
        let token = login(&app, name, role);
        for op in OPS {
            let body = op_body(op.id);
            let (status, resp) = call(
                &router,
                req(
                    "POST",
                    &format!("/api/op/{}", op.id),
                    Some(body),
                    Some(&token),
                ),
            )
            .await;
            match role {
                // viewer 先撞角色门（顺序是刻意的：换会话也解决不了权限）
                Role::Viewer => {
                    assert_eq!(status, StatusCode::FORBIDDEN, "{}: {resp}", op.id);
                    assert!(
                        resp["error"].as_str().unwrap().contains("viewer"),
                        "要说清是哪个角色不够: {resp}"
                    );
                    assert!(
                        !resp["error"].as_str().unwrap().contains("TOTP"),
                        "角色不够时不该把原因说成 MFA: {resp}"
                    );
                }
                _ => {
                    assert_eq!(status, StatusCode::FORBIDDEN, "{}: {resp}", op.id);
                    let msg = resp["error"].as_str().unwrap();
                    // 门的顺序是刻意的：moderator 碰 destructive 时先报角色，
                    // 因为"去开 MFA"根本解决不了权限不足。
                    if op.capability == Capability::Destructive && role == Role::Moderator {
                        assert!(
                            msg.contains("destructive") && msg.contains("superadmin"),
                            "{msg}"
                        );
                        assert!(!msg.contains("动态码"), "角色不够时不该提 MFA: {msg}");
                    } else {
                        assert!(msg.contains("动态码"), "{msg}");
                        assert!(
                            msg.contains("/api/login") || msg.contains("enroll"),
                            "要给出修的路: {msg}"
                        );
                    }
                }
            }
        }
    }
    // 证明"没碰到平台"：对账一行都没落，工单状态没动
    let (_, stats) = call(&router, req("GET", "/api/stats", None, Some(ADMIN_TOKEN))).await;
    assert_eq!(
        stats["last_reconcile"],
        serde_json::Value::Null,
        "被拒的对账不该在平台留下运行记录: {stats}"
    );
    let (_, queue) = call(&router, req("GET", "/api/reports", None, Some(ADMIN_TOKEN))).await;
    assert_eq!(queue["rows"][0]["ticket"]["state"], json!("open"));
    // 但被拒的尝试全部留痕
    let (_, log) = call(
        &router,
        req("GET", "/api/oplog?status=denied", None, Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(
        log["total"].as_i64().unwrap() as usize,
        OPS.len() * 3,
        "三个角色 × 目录里每条操作，被拒也要各记一行: {log}"
    );
}

fn op_body(op_id: &str) -> serde_json::Value {
    let op = common::rbac_get(op_id);
    let mut body = json!({"target": "abc123"});
    if op.needs_note {
        body["note"] = json!("测试依据");
    }
    if op.payload == "action+note" {
        body["action"] = json!("warn");
    }
    if op.payload == "mode" {
        body["mode"] = json!("post_review");
    }
    if !op.needs_target {
        body.as_object_mut().unwrap().remove("target");
    }
    body
}

#[tokio::test]
async fn static_token_cannot_write_at_all() {
    let (_dir, _p, app, _base) = live("d3-static").await;
    let router = app.router();
    for op in OPS {
        let (status, body) = call(
            &router,
            req(
                "POST",
                &format!("/api/op/{}", op.id),
                Some(op_body(op.id)),
                Some(ADMIN_TOKEN),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{}: {body}", op.id);
        assert!(
            body["error"].as_str().unwrap().contains("静态令牌"),
            "{body}"
        );
    }
    let (_, log) = call(&router, req("GET", "/api/oplog", None, Some(ADMIN_TOKEN))).await;
    assert_eq!(
        log["total"],
        json!(0),
        "匿名（静态令牌）尝试不进留痕，因为无法归因到人"
    );
}

// ---------------------------------------------------------------------------
// 有 MFA 的会话：写真的到平台
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mfa_triage_reaches_the_platform_and_is_attributed_to_the_operator() {
    let (_dir, _p, app, _base) = live("d3-triage").await;
    let router = app.router();
    let token = login_mfa(&app, "mod-primary", Role::Moderator);

    let (_, queue) = call(&router, req("GET", "/api/reports", None, Some(ADMIN_TOKEN))).await;
    let id = queue["rows"][0]["id"].as_str().unwrap().to_string();

    let (status, resp) = call(
        &router,
        req(
            "POST",
            "/api/op/report.triage",
            Some(json!({"target": id, "note": "先看是不是同一批广告号"})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["op"], json!("report.triage"));
    assert_eq!(resp["capability"], json!("write"));
    assert_eq!(resp["actor"], json!("mod-primary"));
    assert_eq!(resp["logged"], json!(true));
    assert_eq!(resp["result"]["state"], json!("triaged"));

    // 从只读面再看一次：改动确实在平台库里，而处理人是后台账号名
    let (_, one) = call(
        &router,
        req(
            "GET",
            &format!("/api/reports/{id}"),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(one["ticket"]["state"], json!("triaged"));
    assert_eq!(one["ticket"]["handler"], json!("mod-primary"));
    assert_eq!(
        one["ticket"]["history"][0]["note"],
        json!("先看是不是同一批广告号")
    );

    // 留痕：状态 ok，带上了游路径
    let (_, log) = call(
        &router,
        req(
            "GET",
            "/api/oplog?op=report.triage",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(log["total"], json!(1));
    let row = &log["rows"][0];
    assert_eq!(row["status"], json!("ok"));
    assert_eq!(row["actor"], json!("mod-primary"));
    assert_eq!(row["capability"], json!("write"));
    assert_eq!(row["target"], json!(id));
    assert_eq!(
        row["upstream"],
        json!(format!("POST /api/admin/reports/{id}/triage"))
    );
    assert_eq!(row["mfa"], json!(true));
    assert!(row["detail"].as_str().unwrap().contains("triaged"), "{row}");
}

#[tokio::test]
async fn role_matrix_who_can_do_what() {
    let (_dir, _p, app, _base) = live("d3-matrix").await;
    let router = app.router();
    let viewer = login_mfa(&app, "v1", Role::Viewer);
    let moderator = login_mfa(&app, "m1", Role::Moderator);
    let superadmin = login_mfa(&app, "s1", Role::Superadmin);

    let (_, queue) = call(&router, req("GET", "/api/reports", None, Some(ADMIN_TOKEN))).await;
    let id = queue["rows"][0]["id"].as_str().unwrap().to_string();
    let (_, held) = call(
        &router,
        req("GET", "/api/moderation", None, Some(ADMIN_TOKEN)),
    )
    .await;
    let held_id = held["rows"][0]["id"].as_str().unwrap().to_string();

    let post = |token: &str, op: &str, body: serde_json::Value| {
        let router = router.clone();
        let op = op.to_string();
        let token = token.to_string();
        tokio::spawn(async move {
            call(
                &router,
                req("POST", &format!("/api/op/{op}"), Some(body), Some(&token)),
            )
            .await
        })
    };

    // viewer：什么都不能写
    let (status, body) = post(&viewer, "report.triage", json!({"target": id, "note": "n"}))
        .await
        .unwrap();
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    // moderator：能分诊/结案/驳回/放行/对账，不能处罚、不能丢弃内容、不能切模式
    let (status, _) = post(
        &moderator,
        "report.triage",
        json!({"target": id, "note": "分诊"}),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post(&moderator, "moderation.release", json!({"target": held_id}))
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK, "放行是 write: {body}");
    let (status, body) = post(&moderator, "wallet.reconcile", json!({}))
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK, "对账是 write: {body}");
    assert_eq!(body["result"]["ok"], json!(true), "seed 的账本是守恒的");
    for (op, body) in [
        (
            "report.act",
            json!({"target": id, "note": "警告", "action": "warn"}),
        ),
        ("moderation.reject", json!({"target": held_id})),
        ("moderation.mode", json!({"mode": "post_review"})),
    ] {
        let (status, resp) = post(&moderator, op, body).await.unwrap();
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "moderator 不该能做 {op}: {resp}"
        );
        assert!(
            resp["error"].as_str().unwrap().contains("destructive"),
            "{resp}"
        );
    }

    // superadmin：全量
    let (status, resp) = post(
        &superadmin,
        "report.act",
        json!({"target": id, "note": "确认是广告号", "action": "warn"}),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["result"]["state"], json!("actioned"));
    assert_eq!(resp["result"]["action"], json!({"action": "warn"}));
    let (status, resp) = post(
        &superadmin,
        "moderation.mode",
        json!({"mode": "post_review"}),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK, "204 也算成功: {resp}");
    // 现在工单已经 actioned，只有 reject/close 可走；再确认非法流转会 502
    let (status, resp) = post(
        &superadmin,
        "report.triage",
        json!({"target": id, "note": "重复分诊"}),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::BAD_GATEWAY, "非法流转要转述: {resp}");
    assert!(
        resp["error"]
            .as_str()
            .unwrap()
            .contains("illegal state transition"),
        "{resp}"
    );
    let (_, log) = call(
        &router,
        req(
            "GET",
            "/api/oplog?status=upstream_error",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(log["total"], json!(1), "被平台拒绝也要留痕: {log}");
    assert_eq!(log["rows"][0]["op"], json!("report.triage"));
    assert!(log["rows"][0]["detail"].as_str().unwrap().contains("409"));
}

#[tokio::test]
async fn mute_action_carries_its_duration_and_ban_needs_superadmin() {
    let (_dir, p, app, _base) = live("d3-act").await;
    let router = app.router();
    let moderator = login_mfa(&app, "m2", Role::Moderator);
    let superadmin = login_mfa(&app, "s2", Role::Superadmin);
    let (_, queue) = call(&router, req("GET", "/api/reports", None, Some(ADMIN_TOKEN))).await;
    let id = queue["rows"][0]["id"].as_str().unwrap().to_string();
    call(
        &router,
        req(
            "POST",
            "/api/op/report.triage",
            Some(json!({"target": id, "note": "分诊"})),
            Some(&superadmin),
        ),
    )
    .await;

    // 缺时长的 mute：不许猜
    let (status, body) = call(
        &router,
        req(
            "POST",
            "/api/op/report.act",
            Some(json!({"target": id, "note": "禁言", "action": "mute"})),
            Some(&superadmin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("duration"),
        "{body}"
    );

    // 带时长的 mute（时长写在顶层，也接受写在 action 对象里）
    let (status, resp) = call(
        &router,
        req(
            "POST",
            "/api/op/report.act",
            Some(json!({"target": id, "note": "禁言一小时", "action": "mute", "duration_ms": 3_600_000})),
            Some(&superadmin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp["result"]["action"],
        json!({"action": "mute", "duration_ms": 3_600_000})
    );

    // ban 是 destructive：另开一张已分诊的工单，用 moderator 试，必须被角色门挡下
    let fresh = p
        .create_report(
            &p.user_id_by_name("carol").unwrap().unwrap(),
            "user",
            &p.user_id_by_name("dave").unwrap().unwrap(),
            wxlike_report::Reason::Fraud,
            "疑似诈骗号",
        )
        .unwrap();
    p.triage_report(&fresh.id, "seed", "先分诊").unwrap();
    let (status, body) = call(
        &router,
        req(
            "POST",
            "/api/op/report.act",
            Some(json!({"target": fresh.id, "note": "封号", "action": "ban"})),
            Some(&moderator),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "ban 要 superadmin: {body}");
    assert!(
        body["error"].as_str().unwrap().contains("moderator"),
        "{body}"
    );
    // 平台侧这张工单还停在 triaged：处置根本没发生过
    let (_, one) = call(
        &router,
        req(
            "GET",
            &format!("/api/reports/{}", fresh.id),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(one["ticket"]["state"], json!("triaged"), "{one}");
    assert_eq!(one["ticket"]["action"], serde_json::Value::Null);
    // superadmin 来就成
    let (status, resp) = call(
        &router,
        req(
            "POST",
            "/api/op/report.act",
            Some(json!({"target": fresh.id, "note": "确认诈骗", "action": "ban"})),
            Some(&superadmin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["result"]["action"], json!({"action": "ban"}));
    // 平台审计里也有这条（report.action 由平台落）
    let (_, audit) = call(
        &router,
        req(
            "GET",
            &format!("/api/audit?action=report.action&target={}", fresh.id),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(
        audit["total"],
        json!(1),
        "处置必须在平台审计里看得见: {audit}"
    );
    assert_eq!(audit["rows"][0]["actor"], json!("s2"));
}

#[tokio::test]
async fn target_shape_is_checked_before_the_socket_and_logged() {
    let (_dir, _p, app, _base) = live("d3-target").await;
    let router = app.router();
    let superadmin = login_mfa(&app, "s3", Role::Superadmin);
    for nasty in ["/../../etc/passwd", "a/b", "id\nX-Evil: 1", ""] {
        let (status, body) = call(
            &router,
            req(
                "POST",
                "/api/op/report.triage",
                Some(json!({"target": nasty, "note": "越权尝试"})),
                Some(&superadmin),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{nasty:?}: {body}"
        );
    }
    let (_, log) = call(
        &router,
        req(
            "GET",
            "/api/oplog?op=report.triage",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert!(
        log["total"].as_i64().unwrap() >= 3,
        "被形状校验挡下的尝试也要留痕: {log}"
    );
    for row in log["rows"].as_array().unwrap() {
        assert_eq!(row["status"], json!("denied"));
        assert_eq!(
            row["upstream"],
            json!(""),
            "没发出去的请求不该有路径: {row}"
        );
    }
}

#[tokio::test]
async fn unknown_op_is_422_and_recorded_as_unknown_capability() {
    let (_dir, _p, app, _base) = live("d3-unknown").await;
    let router = app.router();
    let superadmin = login_mfa(&app, "s4", Role::Superadmin);
    let (status, body) = call(
        &router,
        req(
            "POST",
            "/api/op/report.detele",
            Some(json!({"target": "abc", "note": "拼错"})),
            Some(&superadmin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("/api/ops"),
        "{body}"
    );
    let (_, log) = call(
        &router,
        req(
            "GET",
            "/api/oplog?op=report.detele",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(log["total"], json!(1));
    assert_eq!(log["rows"][0]["capability"], json!("unknown"));
    assert_eq!(log["rows"][0]["status"], json!("denied"));
}

// ---------------------------------------------------------------------------
// 独立进程的内存态：这条是"为什么不直接写库"的证据
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mode_switch_reaches_the_running_process_not_just_a_file() {
    let (_dir, _p, app, base) = live("d3-mode").await;
    let router = app.router();

    // 平台的当前模式只能从**活着的进程**问出来（它不在库里）
    let before = common::ask_platform(&base, "/api/admin/moderation/queue", PLATFORM_TOKEN).await;
    assert_eq!(
        before["mode"],
        json!("pre_review"),
        "平台默认先审后发: {before}"
    );

    let superadmin = login_mfa(&app, "s5", Role::Superadmin);
    let (status, resp) = call(
        &router,
        req(
            "POST",
            "/api/op/moderation.mode",
            Some(json!({"mode": "post_review"})),
            Some(&superadmin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");

    // 关键断言：运行中的平台进程确实换了模式。
    // 若 adminx 是"直接写库"，这一步一定是 pre_review（库里根本没有这一列）。
    let after = common::ask_platform(&base, "/api/admin/moderation/queue", PLATFORM_TOKEN).await;
    assert_eq!(
        after["mode"],
        json!("post_review"),
        "写操作必须落到活着的服务上"
    );
    // 平台侧的审计也归因到了后台账号（x-admin-actor 生效）
    let (_, audit) = call(
        &router,
        req(
            "GET",
            "/api/audit?action=moderation.mode",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(audit["total"], json!(1), "平台落了 mode 审计: {audit}");
    assert_eq!(audit["rows"][0]["actor"], json!("s5"));
    assert_eq!(audit["rows"][0]["detail"], json!("post_review"));
}

#[tokio::test]
async fn platform_refusing_the_token_is_reported_not_swallowed() {
    // 平台令牌配错：必须是清楚的 502 + 指路，不能"操作成功"
    let (_dir, _p, app, _base) = live("d3-bad-token").await;
    let mut cfg = app.config().clone();
    cfg.platform_token = "wrong-token".into();
    let read = wxlike_admin::PlatformRead::open(&cfg.platform_db).unwrap();
    let store = wxlike_admin::AccountStore::open(&cfg.accounts_db).unwrap();
    let broken = wxlike_admin::App::assemble(read, store, cfg);
    let router = broken.router();
    let superadmin = login_mfa(&broken, "s6", Role::Superadmin);
    let (status, body) = call(
        &router,
        req(
            "POST",
            "/api/op/wallet.reconcile",
            Some(json!({})),
            Some(&superadmin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("WXLIKE_ADMIN_PLATFORM_TOKEN"),
        "要告诉运维去查哪个变量: {body}"
    );
    let (_, log) = call(
        &router,
        req(
            "GET",
            "/api/oplog?status=upstream_error",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(log["total"], json!(1));
    assert!(log["rows"][0]["detail"].as_str().unwrap().contains("403"));
}

#[tokio::test]
async fn dead_platform_is_a_transport_error_with_a_trail() {
    let dir = common::TempDir::create("d3-dead");
    let db = dir.join("wxlike.db");
    let p = common::platform(&db);
    common::seed(&p);
    drop(p);
    let read = wxlike_admin::PlatformRead::open(&db).unwrap();
    let store = wxlike_admin::AccountStore::open(&dir.join("adminx.db")).unwrap();
    let app = wxlike_admin::App::assemble(
        read,
        store,
        common::cfg(
            db.clone(),
            dir.join("adminx.db"),
            "http://127.0.0.1:1".into(), // 一定连不上
        ),
    );
    let router = app.router();
    let superadmin = login_mfa(&app, "s7", Role::Superadmin);
    let (status, body) = call(
        &router,
        req(
            "POST",
            "/api/op/wallet.reconcile",
            Some(json!({})),
            Some(&superadmin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    let (_, log) = call(
        &router,
        req(
            "GET",
            "/api/oplog?status=transport_error",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(log["total"], json!(1), "连不上也要留痕: {log}");
    assert!(log["rows"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("连不上平台"));
}

// ---------------------------------------------------------------------------
// 账号与会话面
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_flow_and_totp_self_service() {
    let (_dir, _p, app, _base) = live("d3-login").await;
    let router = app.router();
    app.accounts()
        .add("ops-flow", PW, Role::Superadmin)
        .unwrap();

    // 口令错：401，且不区分"账号不存在"
    let (status, _) = call(
        &router,
        req(
            "POST",
            "/api/login",
            Some(json!({"username": "ops-flow", "password": "wrong-password-here"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &router,
        req(
            "POST",
            "/api/login",
            Some(json!({"username": "ghost-account", "password": PW})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 正常登录 → 会话没有写权限 → 自助绑码 → 激活 → 重新登录才有
    let (status, body) = call(
        &router,
        req(
            "POST",
            "/api/login",
            Some(json!({"username": "ops-flow", "password": PW})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["token"].as_str().unwrap().to_string();
    assert_eq!(body["session"]["mfa"], json!(false));
    let (_, id_before) = call(&router, req("GET", "/api/me", None, Some(&token))).await;

    let (status, enroll) = call(
        &router,
        req("POST", "/api/me/totp/enroll", None, Some(&token)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enroll}");
    let secret = enroll["secret"].as_str().unwrap().to_string();
    assert!(enroll["otpauth"]
        .as_str()
        .unwrap()
        .contains("otpauth://totp/"));
    // /api/me 不能把密钥再吐一次
    let (_, me) = call(&router, req("GET", "/api/me", None, Some(&token))).await;
    assert!(
        !me.to_string().contains(&secret),
        "/api/me 泄露了密钥: {me}"
    );
    assert_eq!(me["account"]["totp_provisioned"], json!(true));
    assert_eq!(me["account"]["mfa_enabled"], json!(false));

    // 错码不能激活
    let (status, _) = call(
        &router,
        req(
            "POST",
            "/api/me/totp/confirm",
            Some(json!({"code": "999999"})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let code = current_totp_code(&secret);
    let (status, resp) = call(
        &router,
        req(
            "POST",
            "/api/me/totp/confirm",
            Some(json!({"code": code.clone()})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert!(resp["next"].as_str().unwrap().contains("重新登录"));

    // 旧会话仍然没有写权限（mfa 是登录时定的）
    let (status, _) = call(
        &router,
        req(
            "POST",
            "/api/op/wallet.reconcile",
            Some(json!({})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "旧会话不该突然能写");

    // 现在登录必须带码
    let (status, body) = call(
        &router,
        req(
            "POST",
            "/api/login",
            Some(json!({"username": "ops-flow", "password": PW})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "缺码要明说: {body}");
    assert!(body["error"].as_str().unwrap().contains("动态码"), "{body}");
    let (status, body) = call(
        &router,
        req(
            "POST",
            "/api/login",
            Some(json!({"username": "ops-flow", "password": PW, "totp": code})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["session"]["mfa"], json!(true));
    let fresh = body["token"].as_str().unwrap().to_string();
    let (status, _) = call(
        &router,
        req(
            "POST",
            "/api/op/wallet.reconcile",
            Some(json!({})),
            Some(&fresh),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 退出后会话立刻失效
    let (status, _) = call(&router, req("POST", "/api/logout", None, Some(&fresh))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(
        &router,
        req(
            "POST",
            "/api/op/wallet.reconcile",
            Some(json!({})),
            Some(&fresh),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "退出后必须重新登录");
    let _ = id_before;
}

#[tokio::test]
async fn disabled_account_kills_its_sessions_and_cannot_log_in() {
    let (_dir, _p, app, _base) = live("d3-disable").await;
    let router = app.router();
    let token = login_mfa(&app, "ops-doomed", Role::Superadmin);
    let (status, _) = call(
        &router,
        req(
            "POST",
            "/api/op/wallet.reconcile",
            Some(json!({})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    app.accounts().set_disabled("ops-doomed", true).unwrap();
    let (status, body) = call(
        &router,
        req(
            "POST",
            "/api/op/wallet.reconcile",
            Some(json!({})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "停用必须当场生效: {body}");
    let (status, body) = call(
        &router,
        req(
            "POST",
            "/api/login",
            Some(json!({"username": "ops-doomed", "password": PW})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "停用账号不能登录: {body}");
    assert!(body["error"].as_str().unwrap().contains("停用"), "{body}");
}

#[tokio::test]
async fn password_rotation_revokes_sessions() {
    let (_dir, _p, app, _base) = live("d3-rotate").await;
    let router = app.router();
    let token = login(&app, "ops-rotate", Role::Moderator);
    let (status, _) = call(&router, req("GET", "/api/stats", None, Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
    app.accounts()
        .set_password("ops-rotate", "a-brand-new-long-pass")
        .unwrap();
    let (status, _) = call(&router, req("GET", "/api/stats", None, Some(&token))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "换口令必须下线全部会话");
    let (status, _) = call(
        &router,
        req(
            "POST",
            "/api/login",
            Some(json!({"username": "ops-rotate", "password": PW})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn account_name_rules_are_enforced_over_http_too() {
    let (_dir, _p, app, _base) = live("d3-names").await;
    // 名字会进 x-admin-actor 头：带换行的名字必须在建号时就挡住
    let read = wxlike_admin::PlatformRead::open(&app.config().platform_db).unwrap();
    assert_eq!(read.open_mode(), "read_only");
    for bad in ["has space", "带中文", "ops\r\nX-Evil: 1"] {
        let err = app.accounts().add(bad, PW, Role::Viewer).unwrap_err();
        assert!(
            err.to_string().contains("ASCII") || err.to_string().contains("空白"),
            "{bad:?} 应该被拒: {err}"
        );
    }
    assert!(app.accounts().add("legit-name_1", PW, Role::Viewer).is_ok());
}
