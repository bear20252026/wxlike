//! #114（G02-05）账户四面的 HTTP 面：资料、偏好 / 数据导出 / 注销。
//!
//! 全部走 `tower::oneshot` 打**真路由**（[`wxlike_platform::Platform::router`]），
//! 断言的是**可观察状态**（再查一次库/再打一次认证），不是响应里的好话：
//!
//! * 偏好写进去之后库里真的有两行、另一个用户读不到；
//! * 导出的产物里真的有这个人的数据（收藏正文、偏好值、账目），
//!   而不是一个"status = done"的空壳；
//! * 注销之后这个令牌**再也不认证**、口令也登不进来、别人的时间线里
//!   真的不再出现他的内容；
//! * 坏数据一律报错：解析不出来的作业不会被折成 `export`。
//!
//! # 卡面要求的六类用例分别落在哪
//!
//! | 类别 | 用例 |
//! |---|---|
//! | 成功 | `preferences_round_trip_…`、`export_payload_actually_contains_…`、`delete_revokes_sessions_…`、`profile_update_…` |
//! | 空结果 | `empty_results_are_reported_as_empty_not_fabricated` |
//! | 参数错误 | `preferences_refuse_unknown_keys_…`、`delete_refuses_without_confirm_or_step_up_…`、`profile_update_…`（原子性半段） |
//! | 未授权 | 上述各面的 401/404 断言 + `step_up_ticket_is_single_use_…`、`jobs_are_listed_for_the_owner_only` |
//! | 超时/断网 | `export_fails_closed_when_the_media_store_is_unreachable`（媒体仓注入） |
//! | 取消 | `cancel_stops_a_queued_export_and_never_undoes_a_deletion` |
//! | 写操作重复提交 | `export_payload_…`（同一条作业）、`preferences_sync_…`（UPSERT）、`delete_revokes_…`（409）、`profile_update_…`、`step_up_ticket_…`（单次） |
//! | 日志脱敏/最小权限 | `logs_and_audit_never_carry_personal_values_or_credentials` |
//! | 坏数据不假装 | `a_corrupt_job_row_is_an_error_not_a_fabricated_export` |

mod common;

use axum::http::StatusCode;
use rusqlite::params;
use serde_json::{json, Value};

use common::{app, call, platform, req, signup};
use wxlike_platform::PlatformState;

/// 读库做断言：状态以库里行为准，不以响应为准。
fn count(st: &PlatformState, sql: &str, arg: &str) -> i64 {
    st.db()
        .lock()
        .query_row(sql, params![arg], |r| r.get::<_, i64>(0))
        .unwrap_or_else(|e| panic!("{sql} 读不出来: {e}"))
}

fn text(st: &PlatformState, sql: &str, arg: &str) -> String {
    st.db()
        .lock()
        .query_row(sql, params![arg], |r| r.get::<_, String>(0))
        .unwrap_or_else(|e| panic!("{sql} 读不出来: {e}"))
}

/// 带设备的登录：注册也会建一条会话，注销要数得清。
async fn login(app: &axum::Router, name: &str, device: &str) -> (String, String, String) {
    let _ = call(
        app,
        req(
            "POST",
            "/api/register",
            Some(json!({ "name": name, "password": "pw", "device_id": device })),
            None,
        ),
    )
    .await;
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/login",
            Some(json!({
                "name": name,
                "password": "pw",
                "device_id": device,
                "device_name": "test",
            })),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login {name}: {body}");
    (
        body["user_id"].as_str().unwrap().to_string(),
        body["token"].as_str().unwrap().to_string(),
        body["refresh_token"].as_str().unwrap().to_string(),
    )
}

async fn get_prefs(app: &axum::Router, token: &str) -> (StatusCode, Value) {
    call(app, req("GET", "/api/preferences", None, Some(token))).await
}

async fn put_prefs(app: &axum::Router, token: &str, body: Value) -> (StatusCode, Value) {
    call(app, req("PUT", "/api/preferences", Some(body), Some(token))).await
}

/// 拿一张 step-up 通行证（契约 fixture `jobRequest`：导出与注销都必须持有）。
async fn step_up(app: &axum::Router, token: &str, purpose: &str) -> (StatusCode, Value) {
    call(
        app,
        req(
            "POST",
            "/api/account/step-up",
            Some(json!({ "purpose": purpose, "password": "pw" })),
            Some(token),
        ),
    )
    .await
}

/// 走完"拿证 → 申请"两步。通行证是**单次**的，所以每次申请都要重新拿一张；
/// 这条路径本身就是 `export.request` 的契约形状。
async fn request_export(app: &axum::Router, token: &str) -> (StatusCode, Value) {
    let (status, body) = step_up(app, token, "account.export").await;
    assert_eq!(status, StatusCode::OK, "step-up export: {body}");
    call(
        app,
        req(
            "POST",
            "/api/account/export",
            Some(json!({ "step_up_purpose": "account.export" })),
            Some(token),
        ),
    )
    .await
}

async fn request_delete(app: &axum::Router, token: &str) -> (StatusCode, Value) {
    let (status, body) = step_up(app, token, "account.delete").await;
    assert_eq!(status, StatusCode::OK, "step-up delete: {body}");
    call(
        app,
        req(
            "POST",
            "/api/account/delete",
            Some(json!({ "step_up_purpose": "account.delete", "confirm": true })),
            Some(token),
        ),
    )
    .await
}

/// 建一段好友关系（注销后的可见性断言要用"别人"的视角）。
async fn befriend(app: &axum::Router, from_tok: &str, to_uid: &str, to_tok: &str) {
    let (status, body) = call(
        app,
        req(
            "POST",
            "/api/friends/requests",
            Some(json!({ "to_user_id": to_uid, "message": "hi" })),
            Some(from_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "friend request: {body}");
    let req_id = body["id"].as_str().unwrap().to_string();
    let (status, body) = call(
        app,
        req(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
            Some(to_tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "accept: {body}");
}

#[tokio::test]
async fn preferences_round_trip_is_per_user_and_the_policy_layer_decides() {
    let st = platform();
    let app = app(st.clone());
    let (alice, at) = signup(&app, "alice").await;
    let (_bob, bt) = signup(&app, "bob").await;

    let (status, body) = put_prefs(
        &app,
        &at,
        json!({
            "patch": {"notify.dm": "off", "privacy.discover_by_phone": "on"},
            "device_id": "dev-a",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["values"]["notify.dm"], "off");
    assert_eq!(body["values"]["privacy.discover_by_phone"], "on");
    // 生效判定出自 wxlike-security::privacy，不在平台侧重读一遍键
    assert_eq!(
        body["privacy"]["searchable_by_phone"], true,
        "刚打开的手机号可搜必须反映到判定上: {body}"
    );
    assert_eq!(
        body["privacy"]["searchable_by_id"], true,
        "没动过的键用产品默认值"
    );
    assert_eq!(
        body["notify"]["dm"]["deliver"], false,
        "关掉.dm 就不该再投递"
    );
    assert_eq!(
        body["notify"]["group"]["deliver"], true,
        "dm 的开关不能连累别的类别"
    );
    assert_eq!(
        body["notify"]["call"]["deliver"], true,
        "@我/通话是硬放行的类别"
    );
    assert!(body["head_seq"].as_i64().unwrap() >= 2, "{body}");

    // 写没写进库——响应说写了不算
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_pref WHERE user_id = ?1",
            &alice
        ),
        2
    );

    // 逐用户：bob 一个字节都看不到
    let (status, body) = get_prefs(&app, &bt).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["values"].as_object().unwrap().is_empty(),
        "偏好跨用户了: {body}"
    );
    assert_eq!(
        body["privacy"]["searchable_by_phone"], false,
        "bob 没开过手机号可搜"
    );

    // 免打扰时段由客户端声明，判定仍在策略层：响铃关掉、投递不变
    let (status, body) = call(
        &app,
        req("GET", "/api/preferences?quiet_hours=true", None, Some(&bt)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["notify"]["group"]["deliver"].as_bool().unwrap());
    assert!(
        !body["notify"]["group"]["sound"].as_bool().unwrap(),
        "免打扰时段不该响: {body}"
    );

    // 没有令牌一律 401
    let (status, _) = call(&app, req("GET", "/api/preferences", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &app,
        req(
            "PUT",
            "/api/preferences",
            Some(json!({"patch": {"notify.dm": "off"}})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn preferences_refuse_unknown_keys_without_writing_anything() {
    let st = platform();
    let app = app(st.clone());
    let (_uid, tok) = signup(&app, "carol").await;

    // `theme` 是本地偏好，走服务端通道必须被拒（白名单是封闭集合）
    let (status, body) = put_prefs(&app, &tok, json!({"patch": {"theme": "dark"}})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, _) = put_prefs(&app, &tok, json!({"patch": {"notify.dm": "o\u{7}ff"}})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "控制字符要拒");
    let (status, _) = put_prefs(&app, &tok, json!({"patch": {}})).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "空补丁是客户端漏字段"
    );

    // 被拒的那一批一行都没落库（不是"写了一半再报错"）
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_pref WHERE user_id = ?1",
            &_uid
        ),
        0
    );

    // 混合补丁也不能部分写入
    let (status, _) = put_prefs(
        &app,
        &tok,
        json!({"patch": {"notify.dm": "off", "theme": "dark"}}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_pref WHERE user_id = ?1",
            &_uid
        ),
        0,
        "一个键非法就不能写任何其他键"
    );
}

#[tokio::test]
async fn preferences_sync_returns_the_increment_not_a_full_snapshot() {
    let st = platform();
    let app = app(st.clone());
    let (uid, tok) = signup(&app, "dave").await;

    let (status, first) = put_prefs(&app, &tok, json!({"patch": {"notify.dm": "off"}})).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let head_after_first = first["head_seq"].as_i64().unwrap();
    put_prefs(
        &app,
        &tok,
        json!({"patch": {"privacy.read_receipts": "off"}, "device_id": "dev-b"}),
    )
    .await;

    let (status, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/preferences/sync?since_seq={head_after_first}"),
            None,
            Some(&tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let changes = body["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "增量只有一条: {body}");
    assert_eq!(changes[0]["key"], "privacy.read_receipts");
    assert_eq!(changes[0]["device_id"], "dev-b", "设备归属要真的记下来");
    assert!(body["head_seq"].as_i64().unwrap() > head_after_first);

    // 幂等：同一段补丁再提交一次不产生第二行（值相同、游标仍是最新）
    let (status, again) = put_prefs(
        &app,
        &tok,
        json!({"patch": {"privacy.read_receipts": "off"}, "device_id": "dev-b"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again["values"]["privacy.read_receipts"], "off");
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_pref WHERE user_id = ?1",
            &uid
        ),
        2,
        "UPSERT 不该把同一键插成两行"
    );
}

#[tokio::test]
async fn export_payload_actually_contains_the_users_data() {
    let st = platform();
    let app = app(st.clone());
    let (alice, at, _r) = login(&app, "erin", "dev-e").await;
    let (_bob, bt, _) = login(&app, "frank", "dev-f").await;

    put_prefs(&app, &at, json!({"patch": {"general.language": "zh-CN"}})).await;
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/favorites",
            Some(json!({
                "source_conv": "d-erin~frank",
                "source_msg_id": "0123456789abcdef0123456789abcdef",
                "kind": "text",
                "text": "登机口 B12 改了",
                "sender": "frank",
                "sent_at_ms": 1,
            })),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "favorite: {body}");
    st.fund(&alice, 500, "seed funding").unwrap();

    // 申请：只回执作业 handle，状态是真的"排队中"
    let (status, body) = request_export(&app, &at).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "queued");
    assert_eq!(body["kind"], "export");
    assert_eq!(body["download_ready"], false);
    let job = body["job_id"].as_str().unwrap().to_string();

    // 重复申请是同一作业（不重复打包）
    let (_, again) = request_export(&app, &at).await;
    assert_eq!(again["job_id"], job, "导出必须幂等: {again}");

    // 没做完就想签下载链接 → 403
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/account/export/{job}/link"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "还没产物就签链接是假的成功");

    // 推进作业（服务端由 worker 周期跑，测试直接调，推进是确定的）
    assert_eq!(st.run_account_jobs().await.unwrap(), 1);

    let (status, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/account/export/{job}"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "done");
    assert_eq!(body["download_ready"], true);
    assert!(body["bytes"].as_i64().unwrap() > 0, "{body}");

    let (status, link) = call(
        &app,
        req(
            "POST",
            &format!("/api/account/export/{job}/link"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{link}");
    assert_eq!(link["single_use"], true);
    let url = link["url"].as_str().unwrap().to_string();

    // 取件：里面的必须是这个人的真数据
    let (status, doc) = call(&app, req("GET", &url, None, None)).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["profile"]["name"], "erin");
    assert_eq!(doc["profile"]["id"], alice);
    assert_eq!(doc["settings"]["preferences"]["general.language"], "zh-CN");
    let messages = doc["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "收藏就是这个人的一条消息快照: {doc}");
    assert_eq!(messages[0]["text"], "登机口 B12 改了");
    let ledger = doc
        .get("ledger")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("导出少了 ledger 域（六个契约域一个都不能少）: {doc}"));
    assert!(!ledger.is_empty(), "账目域不能是空的: {doc}");
    assert_eq!(ledger[0]["amount"], 500);
    assert_eq!(doc["scopes"].as_array().unwrap().len(), 6);
    for scope in wxlike_identity::preferences::EXPORT_SCOPES {
        assert!(
            doc.get(scope).is_some(),
            "导出缺少契约声明的 {scope} 域: {doc}"
        );
    }

    // 凭据材料一律不外泄
    let rendered = doc.to_string();
    for banned in ["pw_hash", "pw_salt", "totp_secret", "token"] {
        assert!(
            !rendered.contains(banned),
            "导出里出现了 {banned}: {rendered}"
        );
    }

    // 单次兑换
    let (status, _) = call(&app, req("GET", &url, None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "下载链接必须只能用一次");

    // 别人的作业读不到（也不告诉你它存在）
    let (status, _) = call(
        &app,
        req(
            "GET",
            &format!("/api/account/export/{job}"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_corrupt_job_row_is_an_error_not_a_fabricated_export() {
    let st = platform();
    let app = app(st.clone());
    let (_uid, tok, _) = login(&app, "gina", "dev-g").await;

    let (_, body) = request_export(&app, &tok).await;
    let job = body["job_id"].as_str().unwrap().to_string();

    // 模拟别的写入方/迁移留下的坏行：kind 是拼错的 delete
    st.db()
        .lock()
        .execute(
            "UPDATE identity_job SET kind = 'deleet' WHERE id = ?1",
            params![job],
        )
        .unwrap();

    // 旧实现 `unwrap_or(JobKind::Export)` 会在这里回一个正常的 export。
    // 现在必须是 500：坏数据不许假装功能正常。
    let (status, body) = call(
        &app,
        req(
            "GET",
            &format!("/api/account/export/{job}"),
            None,
            Some(&tok),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "坏行必须报错，实际回的是: {body}"
    );
    assert_ne!(body["kind"], "export", "{body}");

    // worker 也不能把它领走：验原文在改状态之前，作业仍然是 queued 等人处置
    let err = st.run_account_jobs().await.unwrap_err();
    assert!(
        err.to_string().contains("corrupt"),
        "应当是数据损坏而不是别的错误: {err}"
    );
    assert_eq!(
        text(&st, "SELECT status FROM identity_job WHERE id = ?1", &job),
        "queued",
        "验不过原文就绝不该把行改成 running"
    );
}

#[tokio::test]
async fn delete_refuses_without_confirm_or_step_up_and_changes_nothing() {
    let st = platform();
    let app = app(st.clone());
    let (uid, tok, _r) = login(&app, "henry", "dev-h").await;

    // 没有 confirm：一个带 Bearer 的脚本不该能把账号抹掉
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/account/delete",
            Some(json!({"step_up_purpose": "account.delete"})),
            Some(&tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // 有 confirm 但没通行证（也没顺手带口令）：注销只认 step-up 一条通道
    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/account/delete",
            Some(json!({"confirm": true, "password": "pw"})),
            Some(&tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // 通行证用途写错 → 422，且**不该把别的有效通行证吃掉**
    let (status, _) = step_up(&app, &tok, "account.export").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/delete",
            Some(json!({"confirm": true, "step_up_purpose": "account.delete"})),
            Some(&tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "没领过 delete 证就是没领");
    // 那张 export 证仍然有效：下一次导出照样能成
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/export",
            Some(json!({"step_up_purpose": "account.export"})),
            Some(&tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "错用途的尝试不该消耗通行证");

    // 三次尝试都没动过注销本身：没有注销作业、没落标记、会话还能认证
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_job WHERE user_id = ?1 AND kind = 'delete'",
            &uid
        ),
        0
    );
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM users WHERE id = ?1 AND deleted_at IS NULL",
            &uid
        ),
        1
    );
    assert_eq!(
        call(&app, req("GET", "/api/me", None, Some(&tok))).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn delete_revokes_sessions_and_closes_the_account() {
    let st = platform();
    let app = app(st.clone());
    let (alice, at, refresh) = login(&app, "iris", "dev-i1").await;
    // 同一账号第二台设备（register 已经建过一条会话，所以至少 3 条）
    let (_, at2, _) = login(&app, "iris", "dev-i2").await;
    let before = count(
        &st,
        "SELECT COUNT(*) FROM sessions WHERE user_id = ?1",
        &alice,
    );
    assert!(before >= 3, "登录两台设备后应有多条会话: {before}");

    let (status, body) = request_delete(&app, &at).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["job"]["kind"], "delete");
    assert_eq!(body["job"]["status"], "queued");
    assert_eq!(body["revoked_sessions"].as_i64().unwrap(), before);
    assert_eq!(body["login_revoked"], true);

    // 撤销不是"标记一下"，是**行真的没了**（ADR-10 选 DB session 的全部意义）
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM sessions WHERE user_id = ?1",
            &alice
        ),
        0,
        "注销必须真的把会话行删掉"
    );
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_refresh WHERE user_id = ?1 AND revoked_at IS NULL",
            &alice
        ),
        0,
        "刷新链必须一并作废"
    );

    // **两个令牌都不再认证**——这才是"撤销会话"的可观察定义
    for tok in [&at, &at2] {
        assert_eq!(
            call(&app, req("GET", "/api/me", None, Some(tok))).await.0,
            StatusCode::UNAUTHORIZED,
            "注销后这个令牌必须失效"
        );
    }
    // 刷新链也断了：不然一次撤销等于没撤销
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/sessions/refresh",
            Some(json!({"refresh_token": refresh})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 重新登录：账号已经注销，口令正确也不给会话
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/login",
            Some(json!({"name": "iris", "password": "pw", "device_id": "dev-i3"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "注销后不能再登录");

    // 库里的会话行确实清零，标记确实落下了
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM sessions WHERE user_id = ?1",
            &alice
        ),
        0
    );
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM users WHERE id = ?1 AND deleted_at IS NOT NULL",
            &alice
        ),
        1
    );

    // 第二次申请：令牌早死了 → 401；直接调也只会得到 409，且不会多排一条作业
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/delete",
            Some(json!({"confirm": true, "password": "pw"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let err = st.schedule_deletion(&alice).unwrap_err();
    assert!(
        err.to_string().contains("already scheduled"),
        "重复注销必须明确拒绝而不是静默重做: {err}"
    );
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_job WHERE user_id = ?1 AND kind = 'delete'",
            &alice
        ),
        1,
        "重复申请不能排出第二条清理作业"
    );

    // 第二道防线：即使别的实例留下了一条会话行，它也不能认证
    let stale = "cafe00000000000000000000000000000000000000000000000000000000beef";
    st.db()
        .lock()
        .execute(
            "INSERT INTO sessions(token, user_id, created_at, expires_at) VALUES(?1, ?2, 1, 999999999999)",
            params![wxlike_platform::token_digest(stale), alice],
        )
        .unwrap();
    assert_eq!(
        call(&app, req("GET", "/api/me", None, Some(stale))).await.0,
        StatusCode::UNAUTHORIZED,
        "已注销账号的任何会话都不该认证成功"
    );
}

#[tokio::test]
async fn delete_purge_makes_the_data_unreachable_for_everyone() {
    let st = platform();
    let app = app(st.clone());
    let (alice, at, _) = login(&app, "judy", "dev-j").await;
    let (bob, bt, _) = login(&app, "kate", "dev-k").await;
    // bob 主动加 alice，alice 接受：注销后这段关系要从双方都消失
    befriend(&app, &bt, &alice, &at).await;

    put_prefs(&app, &at, json!({"patch": {"notify.group": "off"}})).await;
    let (status, moment) = call(
        &app,
        req(
            "POST",
            "/api/moments",
            Some(json!({"text": "周末去爬山"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{moment}");

    // bob 现在看得见
    let (_, feed) = call(&app, req("GET", "/api/moments/feed", None, Some(&bt))).await;
    assert!(
        feed.to_string().contains("周末去爬山"),
        "前提没成立：bob 的朋友圈里没有 alice 的内容: {feed}"
    );
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM content_index WHERE owner = ?1",
            &alice
        ),
        1,
        "内容得有检索索引，后面才谈得上清掉"
    );

    let (status, body) = request_delete(&app, &at).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let job = body["job"]["job_id"].as_str().unwrap().to_string();

    assert_eq!(st.run_account_jobs().await.unwrap(), 1);
    assert_eq!(
        text(&st, "SELECT status FROM identity_job WHERE id = ?1", &job),
        "done"
    );

    // 别人的视角：内容、关系都不在了
    let (_, feed) = call(&app, req("GET", "/api/moments/feed", None, Some(&bt))).await;
    assert!(
        !feed.to_string().contains("周末去爬山"),
        "清理后 bob 还能看到 alice 的朋友圈: {feed}"
    );
    let (_, friends) = call(&app, req("GET", "/api/friends", None, Some(&bt))).await;
    assert!(
        !friends.to_string().contains(&alice),
        "关系域没清干净: {friends}"
    );
    // 只影响这一个人：bob 的会话与数据不受牵连
    assert_eq!(
        call(&app, req("GET", "/api/me", None, Some(&bt))).await.0,
        StatusCode::OK,
        "注销 alice 不该动 bob 的会话"
    );
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM sessions WHERE user_id = ?1",
            &bob
        ),
        2,
        "bob 的两条会话（注册 + 登录）都该还在"
    );

    // 逐域核对（八域 + 偏好本体）
    for (label, sql) in [
        (
            "sessions",
            "SELECT COUNT(*) FROM sessions WHERE user_id = ?1",
        ),
        (
            "device_keys",
            "SELECT COUNT(*) FROM prekey_bundles WHERE user_id = ?1",
        ),
        (
            "messages",
            "SELECT COUNT(*) FROM contact_log WHERE user_id = ?1",
        ),
        (
            "relations",
            "SELECT COUNT(*) FROM friends WHERE user_id = ?1",
        ),
        ("content", "SELECT COUNT(*) FROM moments WHERE author = ?1"),
        (
            "search_index",
            "SELECT COUNT(*) FROM content_index WHERE owner = ?1",
        ),
        (
            "preferences",
            "SELECT COUNT(*) FROM identity_pref WHERE user_id = ?1",
        ),
    ] {
        assert_eq!(count(&st, sql, &alice), 0, "{label} 域还有残留: {sql}");
    }

    // 用户行本身被匿名化，口令材料一个字节都不留
    assert_eq!(
        text(&st, "SELECT name FROM users WHERE id = ?1", &alice),
        format!("deleted-{alice}")
    );
    assert_eq!(
        text(&st, "SELECT pw_hash FROM users WHERE id = ?1", &alice),
        ""
    );
    // 作业摘要按域记了清掉的行数，八个契约域都在
    let artifact = text(&st, "SELECT artifact FROM identity_job WHERE id = ?1", &job);
    for domain in wxlike_identity::preferences::PURGE_DOMAINS {
        assert!(
            artifact.contains(domain),
            "作业摘要缺域 {domain}: {artifact}"
        );
    }
}

#[tokio::test]
async fn delete_purge_refuses_a_non_empty_wallet_and_rolls_back() {
    let st = platform();
    let app = app(st.clone());
    let (alice, at, _) = login(&app, "liam", "dev-l").await;
    put_prefs(&app, &at, json!({"patch": {"notify.dm": "off"}})).await;
    call(
        &app,
        req(
            "POST",
            "/api/moments",
            Some(json!({"text": "还有钱没花"})),
            Some(&at),
        ),
    )
    .await;
    st.fund(&alice, 700, "leftover").unwrap();

    let (_, body) = request_delete(&app, &at).await;
    let job = body["job"]["job_id"].as_str().unwrap().to_string();

    // 静默销毁余额是不可接受的"清理成功"
    let err = st.run_account_jobs().await.unwrap_err();
    assert!(
        err.to_string().contains("wallet not empty"),
        "应当被余额拦下: {err}"
    );
    assert_eq!(
        text(&st, "SELECT status FROM identity_job WHERE id = ?1", &job),
        "failed"
    );
    assert!(
        text(&st, "SELECT error FROM identity_job WHERE id = ?1", &job).contains("wallet"),
        "用户得能看到为什么没清: {job}"
    );

    // 关键：整个清理是一个事务，没做成就是**一行都没动**
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM moments WHERE author = ?1",
            &alice
        ),
        1,
        "失败的清理不该留下半截状态"
    );
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_pref WHERE user_id = ?1",
            &alice
        ),
        1
    );

    // 余额清零 → 重试通道排出一条**新**作业（旧的已经 failed，不会被再领走）
    st.db()
        .lock()
        .execute(
            "UPDATE wallets SET balance = 0 WHERE user_id = ?1",
            params![alice],
        )
        .unwrap();
    let (retry, revoked) = st.schedule_deletion(&alice).unwrap();
    assert_ne!(retry.id, job, "重试要排一条新作业");
    assert_eq!(revoked, 0, "重试不该再撤一遍会话");
    assert_eq!(st.run_account_jobs().await.unwrap(), 1);

    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM moments WHERE author = ?1",
            &alice
        ),
        0,
        "重试之后才真的清干净"
    );
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_job WHERE user_id = ?1 AND kind = 'delete'",
            &alice
        ),
        2,
        "一条失败 + 一条成功"
    );
}

#[tokio::test]
async fn jobs_are_listed_for_the_owner_only() {
    let st = platform();
    let app = app(st.clone());
    let (alice, at, _) = login(&app, "mia", "dev-m").await;
    let (_bob, bt, _) = login(&app, "noah", "dev-n").await;

    request_export(&app, &at).await;
    let (status, body) = call(&app, req("GET", "/api/account/jobs", None, Some(&at))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(body["jobs"][0]["kind"], "export");
    assert_eq!(
        body["purge_domains"].as_array().unwrap().len(),
        wxlike_identity::preferences::PURGE_DOMAINS.len()
    );

    // 别人的列表里没有这条作业
    let (_, other) = call(&app, req("GET", "/api/account/jobs", None, Some(&bt))).await;
    assert!(other["jobs"].as_array().unwrap().is_empty(), "{other}");
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_job WHERE user_id = ?1",
            &alice
        ),
        1,
        "作业确实挂在 alice 名下"
    );
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_job WHERE user_id = ?1",
            &_bob
        ),
        0
    );

    // 畸形/不存在的路径参数一律 404，不做存在性探测
    assert_eq!(
        call(
            &app,
            req("GET", "/api/account/export/../users", None, Some(&at))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            req("GET", "/api/account/export/deadbeef", None, Some(&at))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

// ---------------------------------------------------------------------------
// 下面是 #114 验收要求的其余用例类别：profile.update、空结果、参数错误、
// 未授权（step-up 单次）、超时/断网、取消。每类都另查一次可观察状态。
// ---------------------------------------------------------------------------

/// `PUT /api/profile`（profile.update）。路由在 users.rs（G02 自有文件），
/// 本卡补的是它的 G02-05 语义：审计、原子写、已注销拒写。
#[tokio::test]
async fn profile_update_is_audited_atomic_and_refuses_a_dead_account() {
    let st = platform();
    let app = app(st.clone());
    let (uid, at, _) = login(&app, "rosa", "dev-r").await;

    // 成功：改了哪两个字段，库里就得真的变成那两个
    let (status, body) = call(
        &app,
        req(
            "PUT",
            "/api/profile",
            Some(json!({"name": "Rosa2", "signature": "登机口 B12"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], "Rosa2");
    assert_eq!(
        text(&st, "SELECT name FROM users WHERE id = ?1", &uid),
        "Rosa2"
    );
    assert_eq!(
        text(&st, "SELECT signature FROM users WHERE id = ?1", &uid),
        "登机口 B12"
    );

    // 审计：字段名在，字段值不在（昵称/签名是个人数据）
    let audit = st.audit_of(&uid, 50).unwrap();
    let row = audit
        .iter()
        .find(|(_, action, _, _)| action == "profile.update")
        .expect("profile.update 必须留审计");
    assert_eq!(row.3, "name,signature", "审计记改了哪些字段");
    let rendered = format!("{audit:?}");
    assert!(
        !rendered.contains("登机口"),
        "审计里出现了签名的**内容**: {rendered}"
    );

    // 未授权：没令牌一律 401
    let (status, _) = call(
        &app,
        req("PUT", "/api/profile", Some(json!({"name": "Hacker"})), None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 参数错误 + 原子性：合法 name 配畸形 signature，**一个字段都不许落库**
    let (status, _) = call(
        &app,
        req(
            "PUT",
            "/api/profile",
            Some(json!({"name": "NotMe", "signature": "x\u{7}y"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        text(&st, "SELECT name FROM users WHERE id = ?1", &uid),
        "Rosa2",
        "失败的补丁不该留下半套资料"
    );

    // 空补丁：漏字段比"故意什么都不改"更可能，必须报错而不是回 200
    let (status, _) = call(&app, req("PUT", "/api/profile", Some(json!({})), Some(&at))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 重复提交：同一补丁再来一次，状态不变（幂等），库里仍是一行
    let (status, _) = call(
        &app,
        req(
            "PUT",
            "/api/profile",
            Some(json!({"name": "Rosa2", "signature": "登机口 B12"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        count(&st, "SELECT COUNT(*) FROM users WHERE id = ?1", &uid),
        1
    );

    // 已注销账号：行只是外键锚点，绝不再接受写入
    st.schedule_deletion(&uid).unwrap();
    let patch = wxlike_platform::ProfilePatch {
        name: Some("Back".into()),
        ..Default::default()
    };
    let err = match st.update_profile(&uid, &patch) {
        Ok(_) => panic!("注销后的 profile.update 必须失败，不能回 200"),
        Err(err) => err,
    };
    assert!(
        matches!(err, wxlike_platform::PlatformError::NotFound),
        "已注销账号按不存在处理：{err:?}"
    );
    assert_eq!(
        text(&st, "SELECT name FROM users WHERE id = ?1", &uid),
        "Rosa2",
        "被拒绝的写一行都不该动"
    );
    // 清理作业跑完之后：行被匿名化，profile.update 仍然进不来
    st.run_account_jobs().await.unwrap();
    assert_eq!(
        text(&st, "SELECT name FROM users WHERE id = ?1", &uid),
        format!("deleted-{uid}")
    );
    assert!(matches!(
        st.update_profile(&uid, &patch),
        Err(wxlike_platform::PlatformError::NotFound)
    ));
}

/// 空结果必须报成空，不能被折成"有一个默认值的样子"。
///
/// 假绿的老形状是：查不到就 `unwrap_or_default()`，于是"这个账号还没写过偏好"
/// 和"库里读不动"在响应里同形；客户端会把游标重置、把默认当成对方的设置。
#[tokio::test]
async fn empty_results_are_reported_as_empty_not_fabricated() {
    let st = platform();
    let app = app(st.clone());
    let (uid, at, _) = login(&app, "tina", "dev-t").await;

    let (status, body) = get_prefs(&app, &at).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["values"].as_object().unwrap().is_empty());
    assert_eq!(
        body["head_seq"].as_i64().unwrap(),
        0,
        "没写过偏好的游标就是 0，不是编的"
    );
    // quiet_hours 没给 ≠ 免打扰时段=false：服务端必须自报"这一格我没拿到输入"
    assert_eq!(
        body["quiet_hours_known"], false,
        "客户端没说时服务端不得假装知道: {body}"
    );
    // 但策略判定仍然是可用的：默认值是产品行为，不是"读不到"
    assert_eq!(body["privacy"]["searchable_by_id"], true);

    // 空增量：200 + 空数组，不是错误、也不是整份快照
    let (status, sync) = call(
        &app,
        req("GET", "/api/preferences/sync?since_seq=0", None, Some(&at)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sync}");
    assert!(sync["changes"].as_array().unwrap().is_empty());
    assert_eq!(sync["head_seq"].as_i64().unwrap(), 0);

    // 空作业列表
    let (_, jobs) = call(&app, req("GET", "/api/account/jobs", None, Some(&at))).await;
    assert!(jobs["jobs"].as_array().unwrap().is_empty());

    // 空结果不留痕：一次读也不该在库里造行
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_pref WHERE user_id = ?1",
            &uid
        ),
        0
    );
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_job WHERE user_id = ?1",
            &uid
        ),
        0
    );
}

/// 超时/断网：媒体仓写不进去时，作业**不许**是 done。
///
/// 这是供应商故障注入（DoD 允许 mock 的唯一用途）：真实的 `LocalDirStore`
/// 被包一层，`put` 永远失败。断言的是库里的状态与下载面，不是响应里的好话。
#[tokio::test]
async fn export_fails_closed_when_the_media_store_is_unreachable() {
    use std::sync::Arc;
    use wxlike_platform::{MediaStore, PlatformError};

    struct Offline {
        inner: Arc<dyn MediaStore>,
    }

    #[async_trait::async_trait]
    impl MediaStore for Offline {
        async fn put(&self, _sha_hex: &str, _bytes: &[u8]) -> wxlike_platform::Result<()> {
            Err(PlatformError::Io(std::io::Error::other(
                "media store unreachable",
            )))
        }
        async fn get(
            &self,
            sha_hex: &str,
            start: u64,
            end: u64,
        ) -> wxlike_platform::Result<Option<Vec<u8>>> {
            self.inner.get(sha_hex, start, end).await
        }
        async fn size_of(&self, sha_hex: &str) -> wxlike_platform::Result<Option<u64>> {
            self.inner.size_of(sha_hex).await
        }
        async fn put_thumb(&self, sha_hex: &str, png: &[u8]) -> wxlike_platform::Result<()> {
            self.inner.put_thumb(sha_hex, png).await
        }
        async fn get_thumb(&self, sha_hex: &str) -> wxlike_platform::Result<Option<Vec<u8>>> {
            self.inner.get_thumb(sha_hex).await
        }
    }

    let st = platform();
    let app = app(st.clone());
    let (_uid, at, _) = login(&app, "umbra", "dev-u").await;
    let (_, body) = request_export(&app, &at).await;
    let job = body["job_id"].as_str().unwrap().to_string();

    let healthy = st.media_store();
    st.set_media_store(Arc::new(Offline {
        inner: healthy.clone(),
    }));

    // worker 这一轮必须失败，而不是"完成 1 条"
    let err = st.run_account_jobs().await.unwrap_err();
    assert!(
        err.to_string().contains("export storage unreachable"),
        "要报出存储不可达，而不是一个笼统的成功: {err}"
    );

    // 库里的状态：failed + 有原因 + 没有产物地址
    assert_eq!(
        text(&st, "SELECT status FROM identity_job WHERE id = ?1", &job),
        "failed"
    );
    assert!(
        text(&st, "SELECT error FROM identity_job WHERE id = ?1", &job).contains("storage"),
        "用户得能看到为什么没导出来"
    );
    assert_eq!(
        text(&st, "SELECT artifact FROM identity_job WHERE id = ?1", &job),
        "",
        "没写进仓就绝不该有产物地址"
    );

    // 对外同样不伪装：状态面 done=false / download_ready=false，签链接 403
    let (status, reply) = call(
        &app,
        req(
            "GET",
            &format!("/api/account/export/{job}"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["status"], "failed");
    assert_eq!(reply["download_ready"], false);
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/account/export/{job}/link"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "失败的作业签不出链接");

    // 网络恢复：重新申请是一条**新**作业，旧的仍是 failed（不会被悄悄重跑）
    st.set_media_store(healthy);
    let (_, again) = request_export(&app, &at).await;
    let job2 = again["job_id"].as_str().unwrap().to_string();
    assert_ne!(job2, job, "失败的作业不该被当成在排的那一条");
    assert_eq!(st.run_account_jobs().await.unwrap(), 1);
    assert_eq!(
        text(&st, "SELECT status FROM identity_job WHERE id = ?1", &job2),
        "done"
    );
    assert_eq!(
        text(&st, "SELECT status FROM identity_job WHERE id = ?1", &job),
        "failed",
        "旧作业的状态不该被下一次成功改写"
    );
}

/// 取消：排队中的导出可以撤回；注销**撤回不了**，且拒绝必须一行都不动。
#[tokio::test]
async fn cancel_stops_a_queued_export_and_never_undoes_a_deletion() {
    let st = platform();
    let app = app(st.clone());
    let (alice, at, _) = login(&app, "vera", "dev-v").await;
    let (_bob, bt, _) = login(&app, "walt", "dev-w").await;

    let (_, body) = request_export(&app, &at).await;
    let job = body["job_id"].as_str().unwrap().to_string();

    // 别人的作业取消不了，也不告诉你它存在
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/account/jobs/{job}/cancel"),
            None,
            Some(&bt),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 没带令牌 → 401
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/account/jobs/{job}/cancel"),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // 取消不该有任何副作用：库里仍是 queued
    assert_eq!(
        text(&st, "SELECT status FROM identity_job WHERE id = ?1", &job),
        "queued"
    );

    // 取消
    let (status, reply) = call(
        &app,
        req(
            "POST",
            &format!("/api/account/jobs/{job}/cancel"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["status"], "cancelled");
    assert_eq!(reply["download_ready"], false);
    assert_eq!(
        text(&st, "SELECT status FROM identity_job WHERE id = ?1", &job),
        "cancelled",
        "取消必须落到库里"
    );

    // worker 不再领它；重复取消是 409（已经离开队列）；链接 403
    assert_eq!(st.run_account_jobs().await.unwrap(), 0);
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/account/jobs/{job}/cancel"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "重复取消要如实拒绝");
    let (status, _) = call(
        &app,
        req(
            "POST",
            &format!("/api/account/export/{job}/link"),
            None,
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 取消之后再申请是一条新作业（那条 cancelled 不算"在排"）
    let (_, fresh) = request_export(&app, &at).await;
    assert_ne!(fresh["job_id"], job);
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_job WHERE user_id = ?1",
            &alice
        ),
        2
    );

    // 注销作业：撤销会话不可逆，所以取消必须被拒，且拒得一行不动。
    // 走 HTTP 时这个令牌已经被注销流程撤销 → 401（这就是"注销即失效"）；
    // 规则本体用库内面复核。
    let (uid2, at2, _) = login(&app, "xena", "dev-x").await;
    let (_, dbody) = request_delete(&app, &at2).await;
    let djob = dbody["job"]["job_id"].as_str().unwrap().to_string();
    let (status, body) = call(
        &app,
        req(
            "POST",
            &format!("/api/account/jobs/{djob}/cancel"),
            None,
            Some(&at2),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "注销之后这张令牌不该还能撤任何东西: {body}"
    );
    let err = st
        .cancel_account_job(&uid2, &djob)
        .expect_err("注销作业不可取消：凭证撤销没有回滚路径");
    assert!(
        err.to_string().contains("deletion cannot be cancelled"),
        "要给出可执行的理由: {err}"
    );
    assert_eq!(
        text(&st, "SELECT status FROM identity_job WHERE id = ?1", &djob),
        "queued",
        "被拒绝的取消一行都不该动"
    );
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM sessions WHERE user_id = ?1",
            &uid2
        ),
        0,
        "会话仍是撤销后的状态，没有被复活的半套"
    );
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM users WHERE id = ?1 AND deleted_at IS NOT NULL",
            &uid2
        ),
        1
    );
}

/// step-up 通行证：用途封闭、验证不可缺、单次消费。
#[tokio::test]
async fn step_up_ticket_is_single_use_and_never_mints_without_verification() {
    let st = platform();
    let app = app(st.clone());
    let (uid, at, _) = login(&app, "yuri", "dev-y").await;

    // 没令牌 → 401
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/step-up",
            Some(json!({"purpose": "account.export", "password": "pw"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 口令不对 → 401，且**一张证都没发**（发一张"验不过的证"是最坏形状）
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/step-up",
            Some(json!({"purpose": "account.export", "password": "wrong"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_step_up WHERE user_id = ?1",
            &uid
        ),
        0
    );

    // 凭证都没给 → 422（不当成验证通过）
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/step-up",
            Some(json!({"purpose": "account.export"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 用途不在本卡名单里 → 422（device.revoke 归 #103，password.change 不走这里）
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/step-up",
            Some(json!({"purpose": "password.change", "password": "pw"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 单次：一张 export 证只够一次导出
    let (status, _) = step_up(&app, &at, "account.export").await;
    assert_eq!(status, StatusCode::OK);
    let (status, first) = call(
        &app,
        req(
            "POST",
            "/api/account/export",
            Some(json!({"step_up_purpose": "account.export"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/export",
            Some(json!({"step_up_purpose": "account.export"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "通行证必须单次");
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_job WHERE user_id = ?1 AND kind = 'export'",
            &uid
        ),
        1,
        "第二次请求根本没进到排队环节"
    );

    // 口令闸门：这个端点是"Bearer + 猜口令"的预言机，连续猜错必须闸住。
    // 换个干净账号，免得和上面的单次消费断言抢同一个桶。
    let (uid2, at2, _) = login(&app, "yuri2", "dev-y2").await;
    for _ in 0..5 {
        let (status, _) = call(
            &app,
            req(
                "POST",
                "/api/account/step-up",
                Some(json!({"purpose": "account.export", "password": "nope"})),
                Some(&at2),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // 第 6 次即使口令是对的，也已经在窗口里被封——闸的是"这个账号在被人猜"
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/step-up",
            Some(json!({"purpose": "account.export", "password": "pw"})),
            Some(&at2),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "口令闸门没闸住");
    // 被闸住时**不许**发证：库里仍然没有任何 step-up 行
    assert_eq!(
        count(
            &st,
            "SELECT COUNT(*) FROM identity_step_up WHERE user_id = ?1",
            &uid2
        ),
        0
    );
}

/// 日志脱敏与最小权限（#114 的"涉及个人数据"证据）。
///
/// 订阅器是手写的，只用 `tracing` 本身——不为一件事往依赖树里塞
/// `tracing-subscriber`（那要走 integration-request 给 G00）。
#[tokio::test]
async fn logs_and_audit_never_carry_personal_values_or_credentials() {
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<String>>>);

    struct Collect<'a>(&'a mut String);

    impl tracing::field::Visit for Collect<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write;
            let _ = write!(self.0, " {}={:?}", field.name(), value);
        }
    }

    impl tracing::Subscriber for Capture {
        fn enabled(&self, _m: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _v: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            // 本用例只关心事件文本；span 不进缓冲。
            tracing::span::Id::from_u64(0)
        }
        fn record(&self, _id: &tracing::span::Id, _v: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _a: &tracing::span::Id, _b: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            let mut line = event.metadata().name().to_string();
            event.record(&mut Collect(&mut line));
            self.0.lock().unwrap().push(line);
        }
        fn enter(&self, _id: &tracing::span::Id) {}
        fn exit(&self, _id: &tracing::span::Id) {}
    }

    const PREF_VALUE: &str = "SECRET-PREF-VALUE-4f2a";
    const PASSWORD: &str = "SECRET-PASSWORD-9c1b";
    const SIGNATURE: &str = "SECRET-SIGNATURE-77ad";

    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(capture.clone());

    let st = platform();
    let app = app(st.clone());
    let (uid, at, token) = login(&app, "zed", "dev-z").await;

    // 个人数据从三条面各走一遍：偏好值、昵称/签名、口令验证
    put_prefs(
        &app,
        &at,
        json!({"patch": {"general.language": PREF_VALUE}}),
    )
    .await;
    call(
        &app,
        req(
            "PUT",
            "/api/profile",
            Some(json!({"signature": SIGNATURE})),
            Some(&at),
        ),
    )
    .await;
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/step-up",
            Some(json!({"purpose": "account.export", "password": PASSWORD})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "口令不对必须验不过，且不能把这条日志变绿"
    );

    // 作业失败路径也会打日志（worker 的 error 分支）
    let (_, body) = request_export(&app, &at).await;
    let job = body["job_id"].as_str().unwrap().to_string();
    st.db()
        .lock()
        .execute(
            "UPDATE identity_job SET kind = 'deleet' WHERE id = ?1",
            params![job],
        )
        .unwrap();
    let _ = st.run_account_jobs().await;

    // 最小权限：导出文档取的是本人可见数据，凭据列一个都不进
    let doc = String::from_utf8(st.export_document(&uid).unwrap()).unwrap();
    for banned in [
        "pw_hash",
        "pw_salt",
        "totp_secret",
        "token_hash",
        "artifact",
    ] {
        assert!(!doc.contains(banned), "导出取到了凭据列 {banned}: {doc}");
    }
    assert!(
        doc.contains(&uid),
        "导出必须是这个人的数据，不是空壳: {doc}"
    );

    // 重复注销申请会打一条 warn（本卡唯一"看得见"的取消/拒绝日志路径）
    st.schedule_deletion(&uid).unwrap();
    let _ = st.schedule_deletion(&uid);

    let lines = capture.0.lock().unwrap().join("\n");
    for banned in [PREF_VALUE, PASSWORD, SIGNATURE, &token] {
        assert!(
            !lines.contains(banned),
            "日志里出现了敏感数据 {banned}:\n{lines}"
        );
    }

    // 审计表同样只有键名/字段名，没有值
    let audit = st.audit_of(&uid, 50).unwrap();
    let rendered = format!("{audit:?}");
    for banned in [PREF_VALUE, PASSWORD, SIGNATURE] {
        assert!(
            !rendered.contains(banned),
            "审计里出现了敏感数据 {banned}: {rendered}"
        );
    }
    assert!(
        rendered.contains("general.language"),
        "审计要真的记下了改动（不是空审计装样子）: {rendered}"
    );
    assert!(
        rendered.contains("profile.update"),
        "profile.update 必须有审计行: {rendered}"
    );
}
