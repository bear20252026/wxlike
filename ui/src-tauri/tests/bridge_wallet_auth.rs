//! #293（provider 半边）：核心是会话的唯一归属地，钱包切片经桥走认证 HTTP。
//!
//! 缺陷本体：新 UI 的七个业务域直接打 `apps/desktop-web/src/lib/http.ts`，而那份
//! 传输的 `setBaseUrl`/`saveSession`/`clearSession` **没有任何调用点**，所以这些
//! 请求根本没带凭据；打包后的 CSP（`connect-src ipc: http://ipc.localhost`）又
//! 压根不许 webview 发 HTTP。修法不是放宽 CSP，而是让这些域不再发 HTTP：会话
//! 留在 Rust 侧，钱的动作由 `core_dispatch` 代持。
//!
//! 全程真服务端栈（`support::stack()`，与 `core_bridge_e2e.rs` 同一夹具）。只有
//! 两条用例用自建的 axum 小服务：一条证明**凭据真的挂上了头**，一条证明
//! **服务端少给字段时不会被兜成看起来像成功的空载荷**。
//!
//! 逐条钉住的口径：
//! 1. 没登录 → `UNAUTHENTICATED`，且**一个字节都不发**（死端口判别：真发了请求
//!    会先撞上 `OFFLINE`，拿到 `OFFLINE` 就是闸门失效的实锤）；
//! 2. 服务端 401/403 → 如实的 `UNAUTHENTICATED`/`FORBIDDEN`，不是 `{"data":null}`；
//! 3. 服务端缺字段 → `INTERNAL` 指名缺哪一个，不是 `unwrap_or_default` 编出来的
//!    "0 元余额"；
//! 4. 幂等键：同一把键重试只落一笔；换键是另一笔；没键就是没安全网（桥不替
//!    调用方编键，那会把两笔不同操作认成同一笔）；
//! 5. Bearer 只出 Rust：任何 action 的返回值里都找不到它。

use serde_json::{json, Value};
use tauri::Manager as _;
use wxlike_ui_lib::core_bridge::{
    capabilities, dispatch, CoreRequest, CoreState, IMPLEMENTED_ACTIONS, STUBBED_ACTIONS,
};
use wxlike_ui_lib::AppState;

mod support;

use crate::support::stack;

type MockApp = tauri::App<tauri::test::MockRuntime>;
type MockHandle = tauri::AppHandle<tauri::test::MockRuntime>;

fn request(action: &str, params: Value, idem: Option<&str>) -> CoreRequest {
    CoreRequest {
        action_id: action.to_string(),
        request_id: format!("g01-293-{action}"),
        idempotency_key: idem.map(str::to_string),
        params,
    }
}

fn boot() -> (MockApp, MockHandle) {
    let app = tauri::test::mock_app();
    app.manage(CoreState::new());
    let handle = app.handle().clone();
    (app, handle)
}

/// 一条 core_dispatch → JSON 信封。与其余桥测试同样在独立 OS 线程里跑：ureq 是
/// 阻塞的，不能占住要等 axum/服务端应答的那条 worker。
fn call_with(app: &MockHandle, action: &str, params: Value, idem: Option<&str>) -> Value {
    let handle = app.clone();
    let req = request(action, params, idem);
    std::thread::spawn(move || {
        let state = handle.state::<AppState>();
        let core = handle.state::<CoreState>();
        serde_json::to_value(dispatch(&handle, &state, &core, req)).unwrap()
    })
    .join()
    .unwrap_or_else(|e| std::panic::resume_unwind(e))
}

fn call(app: &MockHandle, action: &str, params: Value) -> Value {
    call_with(app, action, params, None)
}

fn ok_data<'a>(reply: &'a Value, what: &str) -> &'a Value {
    assert_eq!(reply["ok"], json!(true), "{what} 必须成功，实际：{reply}");
    let data = reply
        .get("data")
        .unwrap_or_else(|| panic!("{what} 的成功信封必须带 data：{reply}"));
    assert!(
        !data.is_null(),
        "{what} 成功时 data 不许是 null（那与失败没区别）：{reply}"
    );
    data
}

/// 失败信封：`ok:false` + 指定错误码 + **没有** `data`。
/// `{"data": null}` 这种"看着像成功"的形状正是本卡要消灭的东西。
fn error<'a>(reply: &'a Value, what: &str, want: &str) -> &'a Value {
    assert_eq!(
        reply["ok"],
        json!(false),
        "{what} 必须失败，不许伪造成功：{reply}"
    );
    assert!(
        reply.get("data").is_none(),
        "{what} 失败时不许带 data 字段（UI 会把 null 当空载荷）：{reply}"
    );
    let err = &reply["error"];
    assert_eq!(err["code"], json!(want), "{what} 的错误码不对：{reply}");
    assert!(
        !err["message"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "{what} 的失败要带上服务端说了什么，不能只给个码：{reply}"
    );
    err
}

fn register(state: &AppState, name: &str) -> String {
    let reply = state
        .register(name, "pw")
        .unwrap_or_else(|e| panic!("{name} 注册失败：{e}"));
    reply["user_id"]
        .as_str()
        .unwrap_or_else(|| panic!("注册响应没有 user_id：{reply}"))
        .to_string()
}

/// 已注册的 AppState + 桥句柄，并在服务端给它的钱包注资。
///
/// `Platform::fund` 是服务端的入账口（不是桥的），所以"余额读得到"证明的是
/// 桥读的是真账本，不是自己算的。钱包只走 HTTP 控制面，不需要连 hub。
fn signed_in(
    http: &str,
    platform: &wxlike_platform::Platform,
    name: &str,
    fund: i64,
) -> (MockApp, MockHandle, String) {
    let (app, handle) = boot();
    let state = AppState::new(http);
    let user_id = register(&state, name);
    if fund > 0 {
        platform.fund(&user_id, fund, "fixture").unwrap();
    }
    app.manage(state);
    (app, handle, user_id)
}

/// 桥读到的余额（整数分）。失败即 panic——用例要的是"读得到真实数字"。
fn balance_of(handle: &MockHandle) -> i64 {
    let reply = call(handle, "wallet.balance.get", json!({}));
    ok_data(&reply, "余额")["balance_minor"]
        .as_i64()
        .unwrap_or_else(|| panic!("data 里没有 balance_minor：{reply}"))
}

/// 账本里某种 kind 的行（走桥读，不碰服务端内部）。
fn kinds(handle: &MockHandle, kind: &str) -> Vec<Value> {
    let reply = call(handle, "wallet.ledger.page", json!({}));
    let page = ok_data(&reply, "账本").clone();
    page["items"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|r| r["kind"] == json!(kind))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unauthenticated_wallet_calls_never_touch_the_network() {
    let (app, handle) = boot();
    // 死端口：请求真发出去会先得到 OFFLINE，所以拿到 OFFLINE == "闸门失效"。
    app.manage(AppState::new("http://127.0.0.1:1"));

    let cases: Vec<(&str, Value)> = vec![
        ("wallet.balance.get", json!({})),
        ("wallet.ledger.page", json!({"limit": 10})),
        (
            "wallet.transfer",
            json!({"to_uid": "u_bob", "amount_minor": 100}),
        ),
        (
            "wallet.redpacket.create",
            json!({"amount_minor": 400, "count": 2}),
        ),
        ("wallet.redpacket.get", json!({"redpacket_id": "deadbeef"})),
        (
            "wallet.redpacket.claim",
            json!({"redpacket_id": "deadbeef"}),
        ),
    ];
    for (action, params) in cases {
        let reply = call(&handle, action, params);
        let err = error(&reply, action, "UNAUTHENTICATED");
        assert!(
            !err["retryable"].as_bool().unwrap_or(true),
            "{action}：没登录不是\"重试一下\"能解决的：{reply}"
        );
        assert!(
            !err["message"]
                .as_str()
                .unwrap_or_default()
                .contains("传输失败"),
            "{action} 必须没发出任何请求（OFFLINE/传输失败都算发了）：{reply}"
        );
    }
}

/// 只认 `Authorization: Bearer …` 的小服务：桥能读到它给的数据，本身就是
/// "凭据确实挂上了头"的证据（#293 的缺陷正是那些请求**没有**这个头）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_request_actually_carries_the_bearer_token() {
    let base = bearer_gate_server().await;
    let (app, handle) = boot();
    let state = AppState::new(&base);
    state.set_auth_token("gate-token-1");
    app.manage(state);

    assert_eq!(
        balance_of(&handle),
        4_321,
        "带凭据的请求要被门卫放行（门卫只在收到 Bearer 头时才回余额）"
    );

    // 同一个服务、同一份代码，把凭据抽掉就再也读不到那份数据：
    // 桥在发请求之前就拦住，报 UNAUTHENTICATED 而不是"余额 0"。
    let (bare_app, bare_handle) = boot();
    bare_app.manage(AppState::new(&base));
    let reply = call(&bare_handle, "wallet.balance.get", json!({}));
    error(&reply, "不带凭据的余额", "UNAUTHENTICATED");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoked_session_surfaces_as_error_not_empty_payload() {
    let (http, _quic, platform) = stack().await;
    let (_app, handle, alice_id) = signed_in(&http, &platform, "r_alice", 5_000);
    assert_eq!(
        balance_of(&handle),
        5_000,
        "夹具注资要能在桥里读到（alice={alice_id}）"
    );

    // 服务端撤销这条凭据：桥里的 token 还在，但服务端不再认。
    let token = handle
        .state::<AppState>()
        .auth_token()
        .expect("登录后要有凭据");
    platform.revoke_session(&token).unwrap();

    let reply = call(&handle, "wallet.balance.get", json!({}));
    let err = error(&reply, "撤销凭据后的余额", "UNAUTHENTICATED");
    assert!(
        err["message"]
            .as_str()
            .unwrap_or_default()
            .contains("expired"),
        "要把服务端的原话带回来，不能只说\"失败了\"：{reply}"
    );
    error(
        &call(&handle, "wallet.ledger.page", json!({})),
        "撤销凭据后的账本",
        "UNAUTHENTICATED",
    );
    error(
        &call_with(
            &handle,
            "wallet.transfer",
            json!({"to_uid": alice_id, "amount_minor": 100}),
            Some("r-key"),
        ),
        "撤销凭据后的转账",
        "UNAUTHENTICATED",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_forbidden_surfaces_as_forbidden() {
    let (http, _quic, platform) = stack().await;
    let (_app, handle, _alice_id) = signed_in(&http, &platform, "f_alice", 5_000);
    let bob = AppState::new(&http);
    let bob_id = register(&bob, "f_bob");
    let carol = AppState::new(&http);
    let carol_id = register(&carol, "f_carol");
    // bob 与 carol 的直聊：alice 既不是收发人也不是成员。
    let foreign = wxlike_ui_lib::direct_conv_id(&bob_id, &carol_id);

    let before = balance_of(&handle);
    let reply = call(
        &handle,
        "wallet.redpacket.create",
        json!({"amount_minor": 400, "count": 2, "conv_id": foreign}),
    );
    let err = error(&reply, "往别人的会话发红包", "FORBIDDEN");
    assert!(
        !err["retryable"].as_bool().unwrap_or(true),
        "403 不是重试能解决的问题：{reply}"
    );
    // 被拒的写操作一分钱都不许动（服务端在扣款之前就挡掉了）。
    assert_eq!(balance_of(&handle), before, "FORBIDDEN 的红包不许已经扣款");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn balance_ledger_and_transfer_are_the_real_thing() {
    let (http, _quic, platform) = stack().await;
    let (alice_app, alice, alice_id) = signed_in(&http, &platform, "b_alice", 5_000);
    let (bob_app, bob, bob_id) = signed_in(&http, &platform, "b_bob", 0);

    assert_eq!(balance_of(&alice), 5_000, "alice 有注资");
    assert_eq!(
        balance_of(&bob),
        0,
        "bob 没被注资，余额就是 0（这是服务端给的，不是桥兜出来的）"
    );

    let reply = call_with(
        &alice,
        "wallet.transfer",
        json!({"to_uid": bob_id, "amount_minor": 1_200, "memo": "AA 制"}),
        Some("t-once"),
    );
    let data = ok_data(&reply, "转账");
    assert!(
        !data["transfer_id"].as_str().unwrap_or_default().is_empty(),
        "转账要带回 transfer_id：{reply}"
    );

    assert_eq!(balance_of(&alice), 3_800, "alice 扣了 1200");
    assert_eq!(balance_of(&bob), 1_200, "bob 收了 1200");

    let page = ok_data(&call(&alice, "wallet.ledger.page", json!({})), "账本").clone();
    assert_eq!(page["has_more"], json!(false), "两行不满一页：{page}");
    assert!(
        page["next_cursor"].is_i64(),
        "游标必须是服务端的账本头（不是桥自己数的序号）：{page}"
    );
    let outs = kinds(&alice, "transfer_out");
    assert_eq!(outs.len(), 1, "账本里要有这条转出：{page}");
    let out = &outs[0];
    // 金额是**大小**（正数，整数分），方向在 kind 里：桥不给它补负号，
    // 那是在客户端另立一份服务端账本口径（`redpacket_refund` 算进还是算出
    // 只有服务端知道）。方向的实际后果由余额那两条断言证。
    assert_eq!(
        out["amount_minor"],
        json!(1_200),
        "转出要读成 1200 分（方向看 kind）：{out}"
    );
    assert_eq!(out["kind"], json!("transfer_out"), "方向在 kind 里：{out}");
    assert_eq!(out["counterparty"], json!(bob_id), "对方是 bob：{out}");
    assert_eq!(out["memo"], json!("AA 制"), "{out}");
    assert!(
        out["ref_id"].as_str().is_some_and(|s| !s.is_empty()),
        "转出要带回引用号：{out}"
    );
    assert!(
        out["created_at_ms"]
            .as_str()
            .is_some_and(|s| s.ends_with('Z')),
        "时间戳按桥的惯例是 ISO-8601：{out}"
    );
    assert_eq!(
        kinds(&alice, "fund_in").len(),
        1,
        "夹具注资那条也该在账本里（桥没把它筛掉）"
    );
    let ins = kinds(&bob, "transfer_in");
    assert_eq!(ins.len(), 1, "bob 那边要有且只有一条转入");
    assert_eq!(ins[0]["amount_minor"], json!(1_200));
    let _ = (alice_app, bob_app, alice_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idempotency_key_dedupes_a_retry_and_absence_is_honest() {
    let (http, _quic, platform) = stack().await;
    let (app, alice, _alice_id) = signed_in(&http, &platform, "i_alice", 5_000);
    let bob = AppState::new(&http);
    let bob_id = register(&bob, "i_bob");

    let params = json!({"to_uid": bob_id, "amount_minor": 300, "memo": "同一笔"});
    let first = ok_data(
        &call_with(&alice, "wallet.transfer", params.clone(), Some("same-key")),
        "第一次转账",
    )
    .clone();
    let retry = ok_data(
        &call_with(&alice, "wallet.transfer", params.clone(), Some("same-key")),
        "重放同一把键",
    )
    .clone();
    assert_eq!(
        first, retry,
        "同一把幂等键必须重放第一次的结果：{first} {retry}"
    );
    assert_eq!(balance_of(&alice), 4_700, "重放不许扣两次");
    assert_eq!(
        kinds(&alice, "transfer_out").len(),
        1,
        "同一把键只该有一条流水"
    );

    // 换一把键 = 另一笔钱（服务端按 (键, 请求指纹) 判重，不是按金额）。
    ok_data(
        &call_with(
            &alice,
            "wallet.transfer",
            json!({"to_uid": bob_id, "amount_minor": 300, "memo": "另一笔"}),
            Some("other-key"),
        ),
        "第二把键的转账",
    );
    assert_eq!(balance_of(&alice), 4_400, "换键要真的再扣一次");

    // 没给键就不发这个头：桥**不**替调用方编一个键（那会把两笔不同操作认成
    // 同一笔），所以两次都是独立的账本行——这是"没有安全网"的如实表现。
    for memo in ["无键一", "无键二"] {
        ok_data(
            &call(
                &alice,
                "wallet.transfer",
                json!({"to_uid": bob_id, "amount_minor": 100, "memo": memo}),
            ),
            "无键转账",
        );
    }
    assert_eq!(balance_of(&alice), 4_200, "两笔无键转账都要落账");
    assert_eq!(
        kinds(&alice, "transfer_out").len(),
        4,
        "四笔转出：同键 1 + 换键 1 + 无键 2"
    );
    let _ = app;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redpacket_create_read_claim_round_trip() {
    let (http, _quic, platform) = stack().await;
    let (alice_app, alice, alice_id) = signed_in(&http, &platform, "p_alice", 5_000);
    let (bob_app, bob, bob_id) = signed_in(&http, &platform, "p_bob", 0);
    // 会话外的第三个人：也走桥，好证明"读不到"是桥如实报的错，不是没登录。
    let (carol_app, carol, _carol_id) = signed_in(&http, &platform, "p_carol", 0);
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);

    let created = ok_data(
        &call_with(
            &alice,
            "wallet.redpacket.create",
            json!({"amount_minor": 400, "count": 2, "memo": "开工大吉", "conv_id": conv}),
            Some("rp-1"),
        ),
        "发红包",
    )
    .clone();
    let id = created["redpacket_id"]
        .as_str()
        .unwrap_or_else(|| panic!("发红包要带回 id：{created}"))
        .to_string();

    let record = ok_data(
        &call(&alice, "wallet.redpacket.get", json!({"redpacket_id": &id})),
        "读红包",
    )
    .clone();
    assert_eq!(record["total_minor"], json!(400), "{record}");
    assert_eq!(record["count"], json!(2));
    assert_eq!(record["claimed"], json!(0));
    assert_eq!(record["remaining"], json!(2), "剩余是算出来的：{record}");
    assert_eq!(record["status"], json!("open"));
    assert_eq!(record["sender_id"], json!(alice_id));
    assert_eq!(record["conv_id"], json!(conv));
    assert_eq!(balance_of(&alice), 4_600, "发出去的包要进托管");

    // bob 领一份。
    let before = balance_of(&bob);
    let claimed = ok_data(
        &call_with(
            &bob,
            "wallet.redpacket.claim",
            json!({"redpacket_id": &id}),
            Some("claim-1"),
        ),
        "领红包",
    )
    .clone();
    let share = claimed["amount_minor"]
        .as_i64()
        .unwrap_or_else(|| panic!("领取要带回这份金额：{claimed}"));
    assert!(share > 0 && share <= 400, "份额不合理：{claimed}");
    assert_eq!(balance_of(&bob), before + share, "领到的钱要进账");

    // 同一个人第二次领（换一把键也不行）：一人一份，服务端如实冲突。
    // 注：服务端的 claim 不读 Idempotency-Key（只有 transfer/create 读），
    // 所以这里兜住重复领取的是"一人一份"，不是幂等表。
    error(
        &call_with(
            &bob,
            "wallet.redpacket.claim",
            json!({"redpacket_id": &id}),
            Some("claim-2"),
        ),
        "同一个人重复领取",
        "CONFLICT",
    );
    assert_eq!(balance_of(&bob), before + share, "重复领取不许再给钱");

    // 发红包的人自己领 → 服务端 403（作者已付过钱）。
    error(
        &call(
            &alice,
            "wallet.redpacket.claim",
            json!({"redpacket_id": &id}),
        ),
        "作者自己领红包",
        "FORBIDDEN",
    );

    // 会话外的人连"这个包存不存在"都不该知道：服务端回 404，桥如实报错。
    error(
        &call(&carol, "wallet.redpacket.get", json!({"redpacket_id": &id})),
        "陌生人读红包",
        "VALIDATION",
    );

    let after_reply = call(
        &alice,
        "wallet.redpacket.get",
        json!({"redpacket_id": id.as_str()}),
    );
    let after = ok_data(&after_reply, "领取后读红包");
    assert_eq!(after["claimed"], json!(1), "已领笔数要推进：{after}");
    assert_eq!(after["remaining"], json!(1));
    assert_eq!(after["claimed_total_minor"], json!(share));
    let _ = (alice_app, bob_app, carol_app);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_server_fields_are_not_fabricated() {
    let (app, handle, base) = lying_server().await;
    let state = AppState::new(&base);
    // 手工放一个凭据：这条用例只关心"服务端少给字段时桥怎么说"。
    state.set_auth_token("lying-token");
    app.manage(state);

    // 200 + 缺 balance：不许变成 `balance_minor: 0`。
    let reply = call(&handle, "wallet.balance.get", json!({}));
    let err = error(&reply, "余额缺 balance 字段", "INTERNAL");
    assert!(
        err["message"]
            .as_str()
            .unwrap_or_default()
            .contains("balance"),
        "要指名缺哪个字段：{reply}"
    );
    assert!(
        !reply.to_string().contains("balance_minor"),
        "更不许把缺字段兜成一份 0 元余额上报：{reply}"
    );

    // 200 + 账本行缺 kind（其余字段齐）：整页要报错，不许交出半截行。
    let page = call(&handle, "wallet.ledger.page", json!({}));
    let err = error(&page, "账本行缺字段", "INTERNAL");
    assert!(
        err["message"].as_str().unwrap_or_default().contains("kind"),
        "账本行少 kind 也要指名：{page}"
    );

    // 2xx + 不是 JSON 的体：照样是错误，不是"没有数据"。
    error(
        &call(
            &handle,
            "wallet.transfer",
            json!({"to_uid": "u_bob", "amount_minor": 100}),
        ),
        "转账响应不是 JSON",
        "INTERNAL",
    );

    // 领取的响应里没有 amount。
    let empty = call(
        &handle,
        "wallet.redpacket.claim",
        json!({"redpacket_id": "deadbeef"}),
    );
    let err = error(&empty, "领取响应没有 amount", "INTERNAL");
    assert!(
        err["message"]
            .as_str()
            .unwrap_or_default()
            .contains("amount"),
        "{empty}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn money_writes_refuse_malformed_params_before_the_socket() {
    let (http, _quic, platform) = stack().await;
    let (_app, handle, alice_id) = signed_in(&http, &platform, "m_alice", 5_000);
    let before = balance_of(&handle);

    // 空/畸形 id 会拼出 `/api/wallet/redpacket//claim` 这种请求；金额非正数是
    // 把钱凭空变出来或负着转。全部在发请求之前拦掉，且一分钱都不许动。
    let cases: Vec<(&str, Value)> = vec![
        ("wallet.transfer", json!({"amount_minor": 100})),
        (
            "wallet.transfer",
            json!({"to_uid": "", "amount_minor": 100}),
        ),
        (
            "wallet.transfer",
            json!({"to_uid": "bob?admin=1", "amount_minor": 100}),
        ),
        (
            "wallet.transfer",
            json!({"to_uid": "../admin", "amount_minor": 100}),
        ),
        (
            "wallet.transfer",
            json!({"to_uid": "bob", "amount_minor": 0}),
        ),
        (
            "wallet.transfer",
            json!({"to_uid": "bob", "amount_minor": -50}),
        ),
        (
            "wallet.transfer",
            json!({"to_uid": "bob", "amount_minor": "100"}),
        ),
        (
            "wallet.transfer",
            json!({"to_uid": alice_id, "amount_minor": 100, "memo": 7}),
        ),
        ("wallet.redpacket.create", json!({"count": 2})),
        (
            "wallet.redpacket.create",
            json!({"amount_minor": 400, "count": 0}),
        ),
        (
            "wallet.redpacket.create",
            json!({"amount_minor": 400, "count": -1}),
        ),
        ("wallet.redpacket.get", json!({})),
        ("wallet.redpacket.get", json!({"redpacket_id": "../x"})),
        ("wallet.redpacket.get", json!({"redpacket_id": ""})),
        ("wallet.redpacket.claim", json!({"redpacket_id": "a b"})),
        ("wallet.redpacket.claim", json!({"redpacket_id": "x#y"})),
    ];
    for (action, params) in cases {
        let reply = call(&handle, action, params);
        error(&reply, &format!("{action} 的畸形参数"), "VALIDATION");
    }
    assert_eq!(balance_of(&handle), before, "被拒的写操作一分钱都不许动");
    assert!(
        kinds(&handle, "transfer_out").is_empty(),
        "被拒的转账不许有流水"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bearer_token_never_crosses_the_bridge() {
    let (http, _quic, platform) = stack().await;
    let (_app, handle, _alice_id) = signed_in(&http, &platform, "t_alice", 5_000);
    let bob = AppState::new(&http);
    let bob_id = register(&bob, "t_bob");
    let token = handle
        .state::<AppState>()
        .auth_token()
        .expect("登录后要有凭据");
    assert!(!token.is_empty(), "夹具凭据不该是空串");

    let mut seen = vec![
        call(&handle, "wallet.balance.get", json!({})),
        call(&handle, "wallet.ledger.page", json!({"limit": 5})),
        call(
            &handle,
            "wallet.redpacket.create",
            json!({"amount_minor": 200, "count": 1}),
        ),
        call_with(
            &handle,
            "wallet.transfer",
            json!({"to_uid": bob_id, "amount_minor": 100}),
            Some("t-token"),
        ),
        // 失败信封也不许把凭据回出去（服务端原话里也不该有它）。
        call(
            &handle,
            "wallet.redpacket.get",
            json!({"redpacket_id": "nope-not-here"}),
        ),
        capabilities(),
    ];
    seen.push(
        error(
            &call(&handle, "wallet.redpacket.list", json!({})),
            "红包列表（如实拒绝）",
            "NOT_IMPLEMENTED",
        )
        .clone(),
    );
    for reply in seen {
        let text = reply.to_string();
        assert!(
            !text.contains(&token),
            "凭据不许跨过桥（会直接进 webview）：{text}"
        );
        assert!(
            !text.contains("Bearer"),
            "桥不该把鉴权头本身回给 UI：{text}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redpacket_list_is_honestly_refused_and_not_advertised() {
    let (http, _quic, platform) = stack().await;
    let (app, handle, _alice_id) = signed_in(&http, &platform, "l_alice", 5_000);

    // 服务端没有 GET /api/wallet/redpacket（见 crates/platform/src/wallet.rs 的
    // routes()），所以这条只能是如实的 NOT_IMPLEMENTED——**不是空列表**。
    let reply = call(&handle, "wallet.redpacket.list", json!({}));
    let err = error(&reply, "红包列表", "NOT_IMPLEMENTED");
    assert!(
        err["message"]
            .as_str()
            .unwrap_or_default()
            .contains("redpacket"),
        "要说清缺的是哪条服务端路由：{reply}"
    );

    // 目录也不许吹它。
    assert!(
        !IMPLEMENTED_ACTIONS.contains(&"wallet.redpacket.list"),
        "capabilities 不能把做不到的上报成能做"
    );
    assert!(
        STUBBED_ACTIONS.contains(&"wallet.redpacket.list"),
        "wallet.redpacket.list 要登记在 STUBBED_ACTIONS（见 tests/catalog_drift.rs）"
    );
    for action in [
        "wallet.balance.get",
        "wallet.ledger.page",
        "wallet.transfer",
        "wallet.redpacket.create",
        "wallet.redpacket.get",
        "wallet.redpacket.claim",
    ] {
        assert!(
            IMPLEMENTED_ACTIONS.contains(&action),
            "{action} 是真实现了的，必须让 UI 能发现"
        );
    }
    let _ = app;
}

// ---------------------------------------------------------------------------
// 自建小服务：一条证"头挂上了"，一条证"缺字段不兜底"
// ---------------------------------------------------------------------------

/// 只认 Bearer 的门卫：不带 `Authorization: Bearer …` 一律 401。
async fn bearer_gate_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/api/wallet/balance",
        axum::routing::get(|headers: axum::http::HeaderMap| async move {
            let bearer = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("Bearer ") && v.len() > "Bearer ".len());
            if !bearer {
                return (
                    axum::http::StatusCode::UNAUTHORIZED,
                    r#"{"error":"missing bearer token"}"#.to_string(),
                );
            }
            (
                axum::http::StatusCode::OK,
                r#"{"balance":4321}"#.to_string(),
            )
        }),
    );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

/// 说谎的服务端：200 + 缺字段 / 200 + 非 JSON 体。
async fn lying_server() -> (MockApp, MockHandle, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .route(
            "/api/wallet/balance",
            axum::routing::any(|| async { r#"{"nothing":1}"# }),
        )
        .route(
            "/api/wallet/ledger",
            axum::routing::any(|| async {
                r#"{"entries":[{"id":1}],"cursor":1,"has_more":false}"#
            }),
        )
        .route(
            "/api/wallet/transfer",
            axum::routing::any(|| async { "not json at all" }),
        )
        .route(
            "/api/wallet/redpacket/deadbeef/claim",
            axum::routing::any(|| async { r#"{"entries":[]}"# }),
        );
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let (app, handle) = boot();
    (app, handle, format!("http://{addr}"))
}
