//! #114（G02-05）账户三面的 HTTP 面：偏好 / 数据导出 / 注销。
//!
//! 全部走 `tower::oneshot` 打**真路由**（[`wxlike_platform::Platform::router`]），
//! 断言的是**可观察状态**，不是响应里的好话：
//!
//! * 偏好写进去之后库里真的有两行、另一个用户读不到；
//! * 导出的产物里真的有这个人的数据（收藏正文、偏好值、账目），
//!   而不是一个"status = done"的空壳；
//! * 注销之后这个令牌**再也不认证**、口令也登不进来、别人的时间线里
//!   真的不再出现他的内容；
//! * 坏数据一律报错：解析不出来的作业不会被折成 `export`。

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
    let (status, body) = call(&app, req("POST", "/api/account/export", None, Some(&at))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "queued");
    assert_eq!(body["kind"], "export");
    assert_eq!(body["download_ready"], false);
    let job = body["job_id"].as_str().unwrap().to_string();

    // 重复申请是同一作业（不重复打包）
    let (_, again) = call(&app, req("POST", "/api/account/export", None, Some(&at))).await;
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
    let ledger = doc["ledger"].as_array().unwrap();
    assert!(!ledger.is_empty(), "账目域不能是空的: {doc}");
    assert_eq!(ledger[0]["amount"], 500);
    assert_eq!(doc["scopes"].as_array().unwrap().len(), 6);

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

    let (_, body) = call(&app, req("POST", "/api/account/export", None, Some(&tok))).await;
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
async fn delete_refuses_without_confirm_or_credentials_and_changes_nothing() {
    let st = platform();
    let app = app(st.clone());
    let (uid, tok, _r) = login(&app, "henry", "dev-h").await;

    // 没有 confirm：一个带 Bearer 的脚本不该能把账号抹掉
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/delete",
            Some(json!({"password": "pw"})),
            Some(&tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 有 confirm 但凭据不对
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/delete",
            Some(json!({"confirm": true, "password": "wrong"})),
            Some(&tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // 两者都缺
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/account/delete",
            Some(json!({"confirm": true})),
            Some(&tok),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 三次尝试都没做成任何事：没作业、没标记、会话还在
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

    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/account/delete",
            Some(json!({"confirm": true, "password": "pw"})),
            Some(&at),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["job"]["kind"], "delete");
    assert_eq!(body["job"]["status"], "queued");
    assert_eq!(body["revoked_sessions"].as_i64().unwrap(), before);
    assert_eq!(body["login_revoked"], true);

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

    let (status, body) = call(
        &app,
        req(
            "POST",
            "/api/account/delete",
            Some(json!({"confirm": true, "password": "pw"})),
            Some(&at),
        ),
    )
    .await;
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

    let (_, body) = call(
        &app,
        req(
            "POST",
            "/api/account/delete",
            Some(json!({"confirm": true, "password": "pw"})),
            Some(&at),
        ),
    )
    .await;
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

    call(&app, req("POST", "/api/account/export", None, Some(&at))).await;
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
