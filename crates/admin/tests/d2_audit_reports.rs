//! D2 验收：审计检索 + 举报工单。
//!
//! 卡的验收栏只有一句话：**"按命令 id 过滤能查到 agent 执行过哪些写操作"**。
//! 这句话有两个容易糊过去的地方，所以这个文件都测了：
//!
//! 1. 记录必须是**平台中间件真的落过**的那种（`action = "cmd.<id>"`），
//!    而不是测试自己 `audit()` 手写的漂亮数据 —— 所以有一个用例走真 HTTP：
//!    带 `X-Wxlike-Command` 头打平台，再从 adminx 查回来；
//! 2. "查得到"不等于"查得对"：还得证明时间窗、actor、分页与非法区间的行为，
//!    否则检索页会给运营一个假的"没有记录"。

mod common;

use axum::http::StatusCode;
use serde_json::json;

use common::{call, cfg, login, platform, req, seed, serve_platform, TempDir, ADMIN_TOKEN, PW};
use std::sync::Arc;
use wxlike_admin::rbac::Role;

/// 只读 app（平台库已由 seed 写好；写它的进程已经退出）。
fn app_for(db: &std::path::Path, accounts: &std::path::Path) -> Arc<wxlike_admin::App> {
    let read = wxlike_admin::PlatformRead::open(db).unwrap();
    let store = wxlike_admin::AccountStore::open(accounts).unwrap();
    wxlike_admin::App::assemble(
        read,
        store,
        cfg(
            db.to_path_buf(),
            accounts.to_path_buf(),
            "http://127.0.0.1:1".into(),
        ),
    )
}

// ---------------------------------------------------------------------------
// 审计检索
// ---------------------------------------------------------------------------

#[tokio::test]
async fn command_id_finds_what_the_agent_actually_wrote() {
    // 这一步是真的经平台中间件落审计：POST 带 X-Wxlike-Command: group.create
    let dir = TempDir::create("d2-agent-writes");
    let db = dir.join("wxlike.db");
    let p = platform(&db);
    let base = serve_platform(p.clone()).await;

    let (status, registered) = common::post_platform(
        &base,
        "/api/register",
        json!({"name": "agentuser", "password": "pw"}),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "{registered}");
    let token = registered["token"].as_str().unwrap().to_string();
    let uid = registered["user_id"].as_str().unwrap().to_string();
    // 两条写命令
    for (command, body) in [
        ("group.create", json!({"name": "运营群", "member_ids": []})),
        ("group.create", json!({"name": "第二个群"})),
    ] {
        let (status, resp) =
            common::post_platform(&base, "/api/groups", body, Some(&token), Some(command)).await;
        assert_eq!(status, 200, "{command} 应该成功: {resp}");
    }
    // 一次只读调用：不该被算成"写操作"
    let (status, _) = common::http_platform(
        "GET",
        &base,
        "/api/groups/mine",
        None,
        Some(&token),
        Some("group.list"),
        None,
    )
    .await;
    assert_eq!(status, 200);

    // adminx 后开：证明它读得到"打开之后才提交"的行（WAL 快照不是开库时冻结的）
    let app = app_for(&db, &dir.join("adminx.db"));
    let router = app.router();

    let (status, hits) = call(
        &router,
        req(
            "GET",
            "/api/audit?command=group.create",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        hits["total"],
        json!(2),
        "agent 用 group.create 写过两次，就该查到两次: {hits}"
    );
    for row in hits["rows"].as_array().unwrap() {
        assert_eq!(row["action"], json!("cmd.group.create"));
        assert_eq!(row["target"], json!("/api/groups"));
        assert_eq!(row["detail"], json!("status=200 method=POST"));
        assert_eq!(row["actor"], json!(uid));
    }
    // 读命令是另一条轨迹
    let (_, reads) = call(
        &router,
        req(
            "GET",
            "/api/audit?command=group.list",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(reads["total"], json!(1));
    // 没查过的命令：空，但不是 404、不是 500
    let (_, none) = call(
        &router,
        req(
            "GET",
            "/api/audit?command=msg.recall",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(none["total"], json!(0));
    assert_eq!(none["rows"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn audit_filters_combine_and_narrow() {
    let dir = TempDir::create("d2-filters");
    let db = dir.join("wxlike.db");
    let p = platform(&db);
    let s = seed(&p);
    let app = app_for(&db, &dir.join("adminx.db"));
    let router = app.router();

    // 基线：seed 落的审计条数（4 次 register + 4 次手工 + report.create）。
    // 不写死数字：后面所有断言都相对它，平台改落审计时这一条不会假红。
    let (_, all) = call(&router, req("GET", "/api/audit", None, Some(ADMIN_TOKEN))).await;
    let base = all["total"].as_i64().unwrap();
    assert!(base >= 9, "seed 至少该落 9 条: {all}");

    // actor 单独
    let (_, by_actor) = call(
        &router,
        req(
            "GET",
            &format!("/api/audit?actor={}", s.bob),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(by_actor["total"], json!(3), "register + 2 次 cmd.msg.send");
    assert!(by_actor["total"].as_i64().unwrap() < base, "过滤必须收窄");

    // actor + command：bob 的 msg.send
    let (_, combo) = call(
        &router,
        req(
            "GET",
            &format!("/api/audit?actor={}&command=msg.send", s.bob),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(combo["total"], json!(2));

    // actor + command 拼错人：空
    let (_, wrong) = call(
        &router,
        req(
            "GET",
            &format!("/api/audit?actor={}&command=msg.send", s.alice),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(wrong["total"], json!(0));

    // 原始 action 精确匹配（不是 cmd. 前缀）
    let (_, login) = call(
        &router,
        req(
            "GET",
            &format!("/api/audit?action=user.login&actor={}", s.alice),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(login["total"], json!(1));

    // target 维度：一次群操作发生过几次
    let (_, target) = call(
        &router,
        req(
            "GET",
            "/api/audit?target=/api/conv/g-1/send",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(target["total"], json!(2));

    // 时间窗：未来 → 空；包含现在 → 全量
    let now = wxlike_admin::read::now_ms();
    let (_, future) = call(
        &router,
        req(
            "GET",
            &format!("/api/audit?from={}", now + 60_000),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(future["total"], json!(0));
    let (_, window) = call(
        &router,
        req(
            "GET",
            &format!("/api/audit?from=0&to={}", now + 1_000),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(
        window["total"].as_i64().unwrap(),
        base,
        "包含现在的时间窗该是全量"
    );

    // 分页：不重不漏
    let (_, page1) = call(
        &router,
        req(
            "GET",
            "/api/audit?limit=3&offset=0",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    let (_, page2) = call(
        &router,
        req(
            "GET",
            "/api/audit?limit=3&offset=3",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    let ids = |v: &serde_json::Value| -> Vec<i64> {
        v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_i64().unwrap())
            .collect()
    };
    let (a, b) = (ids(&page1), ids(&page2));
    assert_eq!(a.len(), 3);
    assert_eq!(b.len(), 3);
    assert!(a[0] > a[2], "新的在前: {a:?}");
    assert!(a[2] > b[0], "两页之间不能重叠或跳号: {a:?} {b:?}");
    assert_eq!(page2["total"].as_i64().unwrap(), base);
}

#[tokio::test]
async fn audit_rejects_bad_parameters_instead_of_returning_empty() {
    let dir = TempDir::create("d2-bad-params");
    let db = dir.join("wxlike.db");
    let p = platform(&db);
    seed(&p);
    let app = app_for(&db, &dir.join("adminx.db"));
    let router = app.router();

    // 区间反了：这是用法错误，静默返回空列表会让人以为"没人干过这事"
    let (status, body) = call(
        &router,
        req(
            "GET",
            "/api/audit?from=2000&to=1000",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("时间区间是反的"),
        "{body}"
    );

    // 不是数字
    let (status, body) = call(
        &router,
        req("GET", "/api/audit?from=last-week", None, Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"].as_str().unwrap().contains("epoch 毫秒"),
        "要说清参数该长什么样: {body}"
    );

    // 负数时间点：按"没填"处理（不是错误，但也不能变成"全表"）
    let (_, base_rows) = call(&router, req("GET", "/api/audit", None, Some(ADMIN_TOKEN))).await;
    let base = base_rows["total"].as_i64().unwrap();
    let (_, neg) = call(
        &router,
        req("GET", "/api/audit?from=-5", None, Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(neg["total"].as_i64().unwrap(), base, "负数时间点等价于没填");

    // 超长筛选值
    let long = "x".repeat(200);
    let (status, _) = call(
        &router,
        req(
            "GET",
            &format!("/api/audit?actor={long}"),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn audit_sql_injection_shaped_input_stays_inert() {
    let dir = TempDir::create("d2-injection");
    let db = dir.join("wxlike.db");
    let p = platform(&db);
    seed(&p);
    let app = app_for(&db, &dir.join("adminx.db"));
    let router = app.router();
    // 值只会进绑定，不会进 SQL 形状
    let nasty = "' OR 1=1 --";
    let (status, body) = call(
        &router,
        req(
            "GET",
            &format!("/api/audit?actor={}", urlencode(nasty)),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], json!(0), "注入形状必须什么都查不到: {body}");
    // 表还在
    let (_, after) = call(&router, req("GET", "/api/stats", None, Some(ADMIN_TOKEN))).await;
    assert_eq!(after["users_total"], json!(4));
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 举报工单
// ---------------------------------------------------------------------------

#[tokio::test]
async fn report_queue_defaults_to_unfinished_and_carries_the_state_machine() {
    let dir = TempDir::create("d2-reports");
    let db = dir.join("wxlike.db");
    let p = platform(&db);
    let s = seed(&p);
    // 再制造一张已结案的：open → triage → close
    let closed = p
        .create_report(
            &s.alice,
            "group",
            &s.group,
            wxlike_report::Reason::Abuse,
            "骂人",
        )
        .unwrap();
    p.triage_report(&closed.id, "ops", "看过了").unwrap();
    p.close_report(&closed.id, "ops", "已处理").unwrap();

    let app = app_for(&db, &dir.join("adminx.db"));
    let router = app.router();

    // 默认只看未终结
    let (_, queue) = call(&router, req("GET", "/api/reports", None, Some(ADMIN_TOKEN))).await;
    assert_eq!(
        queue["total"],
        json!(1),
        "已结案的不该出现在处置队列: {queue}"
    );
    assert_eq!(queue["rows"][0]["id"], json!(s.report_id));
    assert_eq!(queue["rows"][0]["ticket"]["state"], json!("open"));
    assert_eq!(queue["rows"][0]["ticket"]["reason"], json!("spam"));
    assert_eq!(queue["rows"][0]["ticket"]["target_id"], json!(s.bob));
    assert_eq!(queue["rows"][0]["ticket"]["detail"], json!("广告轰炸"));

    // 显式要 closed
    let (_, done) = call(
        &router,
        req("GET", "/api/reports?state=closed", None, Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(done["total"], json!(1));
    assert_eq!(done["rows"][0]["id"], json!(closed.id));
    // 结案工单的流转历史完整（3 条：open→triaged→closed 有两次 move）
    assert_eq!(
        done["rows"][0]["ticket"]["history"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // 全部
    let (_, all) = call(
        &router,
        req(
            "GET",
            "/api/reports?state=closed&open_only=0",
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(all["total"], json!(1));
    let (_, both) = call(
        &router,
        req("GET", "/api/reports?open_only=0", None, Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(both["total"], json!(2), "open_only=0 才是归档浏览器");

    // 未知状态：422 + 把可选项说出来
    let (status, body) = call(
        &router,
        req("GET", "/api/reports?state=doing", None, Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let msg = body["error"].as_str().unwrap();
    for want in ["open", "triaged", "actioned", "closed", "rejected"] {
        assert!(msg.contains(want), "错误里该列出可选状态: {msg}");
    }
}

#[tokio::test]
async fn overdue_follows_the_configured_sla_not_a_hardcoded_number() {
    // SLA = 0：任何未终结工单都算超时。SLA = 1 年：都不算。
    let dir = TempDir::create("d2-sla");
    let db = dir.join("wxlike.db");
    let p = platform(&db);
    let s = seed(&p);
    drop(p);

    for (sla, expect) in [(0u64, true), (365 * 86_400_000, false)] {
        let read = wxlike_admin::PlatformRead::open(&db).unwrap();
        let accounts_db = dir.join(&format!("adminx-{sla}.db"));
        let store = wxlike_admin::AccountStore::open(&accounts_db).unwrap();
        let mut c = cfg(db.clone(), accounts_db.clone(), "http://127.0.0.1:1".into());
        c.report_sla_ms = sla;
        let app = wxlike_admin::App::assemble(read, store, c);
        let (_, queue) = call(
            &app.router(),
            req("GET", "/api/reports", None, Some(ADMIN_TOKEN)),
        )
        .await;
        assert_eq!(queue["total"], json!(1));
        assert_eq!(
            queue["rows"][0]["overdue"],
            json!(expect),
            "SLA={sla} 时的 overdue 不对: {queue}"
        );
        // 单张详情也算 overdue
        let (status, one) = call(
            &app.router(),
            req(
                "GET",
                &format!("/api/reports/{}", s.report_id),
                None,
                Some(ADMIN_TOKEN),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{one}");
        assert_eq!(one["overdue"], json!(expect));
    }
}

#[tokio::test]
async fn report_detail_validates_its_id_and_404s_cleanly() {
    let dir = TempDir::create("d2-report-detail");
    let db = dir.join("wxlike.db");
    let p = platform(&db);
    let s = seed(&p);
    let app = app_for(&db, &dir.join("adminx.db"));
    let router = app.router();

    let (status, _) = call(
        &router,
        req(
            "GET",
            &format!("/api/reports/{}", s.report_id),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &router,
        req("GET", "/api/reports/deadbeef", None, Some(ADMIN_TOKEN)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 形状不对的 id 是 422，不是拿去查一把
    let (status, body) = call(
        &router,
        req(
            "GET",
            &format!("/api/reports/{}", urlencode("../secrets")),
            None,
            Some(ADMIN_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // 没令牌仍然先 401
    let (status, _) = call(&router, req("GET", "/api/reports/x", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn session_token_can_read_the_search_surface_too() {
    // 登录后不该再要求填一次静态令牌：审计员拿会话就能查历史
    let dir = TempDir::create("d2-session-read");
    let db = dir.join("wxlike.db");
    let p = platform(&db);
    seed(&p);
    let app = app_for(&db, &dir.join("adminx.db"));
    let token = login(&app, "auditor", Role::Viewer);
    let router = app.router();
    for path in ["/api/audit?command=msg.send", "/api/reports", "/api/oplog"] {
        let (status, _) = call(&router, req("GET", path, None, Some(&token))).await;
        assert_eq!(status, StatusCode::OK, "{path}");
    }
    // 过期会话读不了：用 ttl=0 再造一个
    app.accounts().add("shorty", PW, Role::Viewer).unwrap();
    let (_, expired) = app
        .accounts()
        .login("shorty", PW, None, 0, "")
        .expect("ttl=0 也该发得出会话，只是立刻过期");
    let (status, _) = call(&router, req("GET", "/api/stats", None, Some(&expired))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "过期会话必须重新登录");
}
