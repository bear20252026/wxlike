//! #296 / G01：共享处理器 + 能力目录里那条"路由了但做不到"的 action。
//!
//! 审计当天点名的三处"一个处理器服务两条 action"（靠 `req.action_id` 内部分支）：
//!
//! | 处理器 | 服务的 action | 结论 |
//! |---|---|---|
//! | `act_messaging_conversation_pin` | `…conversation.pin` / `…conversation.mute` | 分支正确，但**兄弟标志会被写成 `null`** |
//! | `act_relations_request_accept` | `…request.accept` / `…request.decline` | 正确：打的是两个不同端点，不改 |
//! | `act_relations_tag_set` | `…tag.set` / `…star.set` | 分支正确，但 `star.set` 把 `tags` 写成 **`null`**（前端 `contact.tags.length` 当场崩） |
//!
//! 这里逐条把副作用钉死：只改自己那份状态、兄弟字段必须保持原值且类型正确。
//! 另外补上 `relations.group.kick`——它在路由表里挂了、却恒返
//! `NOT_IMPLEMENTED`（旧注释说"服务端暂无群成员移除路由"，实际
//! `crates/platform/src/groups.rs` 早就挂了 `DELETE /api/groups/{id}/members/{uid}`，
//! CLI 的 `group.kick` 用的就是它），前端 `group-page.tsx` 的踢人按钮因此永远失败。
//!
//! 全程真服务端栈（`support::stack()`），不上 mock。

use serde_json::{json, Value};
use tauri::Manager as _;
use wxlike_ui_lib::core_bridge::{
    capabilities, dispatch, CoreRequest, CoreState, IMPLEMENTED_ACTIONS,
};
use wxlike_ui_lib::AppState;

mod support;

use crate::support::{befriend, stack};

type MockApp = tauri::App<tauri::test::MockRuntime>;
type MockHandle = tauri::AppHandle<tauri::test::MockRuntime>;

/// `WXLIKE_DATA_DIR` 是进程级环境变量，登录瞬间被读；并行测试会互相踩数据目录，
/// 所以要用登录的用例整段串行（锁粒度 = 整个测试体，跨 await 持有是刻意的）。
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn req(action: &str, params: Value) -> CoreRequest {
    CoreRequest {
        action_id: action.to_string(),
        request_id: format!("g01-{action}"),
        idempotency_key: None,
        params,
    }
}

fn boot() -> (MockApp, MockHandle) {
    let app = tauri::test::mock_app();
    app.manage(CoreState::new());
    let handle = app.handle().clone();
    (app, handle)
}

/// 一条 core_dispatch → JSON 信封。与 `core_bridge_e2e.rs` 同样在独立 OS 线程里
/// 跑：Tauri 把 async 命令丢自己的工作线程，无 tokio 上下文时 QUIC 的 `wait()`
/// 才走纯 block_on 路径（见审计 R5）。
fn call(app: &MockHandle, action: &str, params: Value) -> Value {
    let handle = app.clone();
    let request = req(action, params);
    std::thread::spawn(move || {
        let state = handle.state::<AppState>();
        let core = handle.state::<CoreState>();
        serde_json::to_value(dispatch(&handle, &state, &core, request)).unwrap()
    })
    .join()
    .unwrap_or_else(|e| std::panic::resume_unwind(e))
}

fn ok_data<'a>(reply: &'a Value, what: &str) -> &'a Value {
    assert_eq!(reply["ok"], json!(true), "{what} 必须成功，实际：{reply}");
    &reply["data"]
}

fn error<'a>(reply: &'a Value, what: &str) -> &'a Value {
    assert_eq!(
        reply["ok"],
        json!(false),
        "{what} 必须失败，不许伪造成功：{reply}"
    );
    &reply["error"]
}

/// kv 里的 JSON（直接读盘，不经桥——证明副作用真的落了库）。
fn kv_json(db: &std::path::Path, key: &str) -> Value {
    let raw = wxlike_ui_lib::local_kv(db, key)
        .unwrap_or_else(|| panic!("kv 里没有 {key}（副作用没落盘？）"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("kv {key} 不是 JSON：{raw} / {e}"))
}

/// 信箱快照：`(请求 id, 申请人 user_id)`。服务端字段是 `from_uid`
/// （`crates/platform/src/friends.rs` 的 `RequestRow`）。
fn inbox(state: &AppState) -> Vec<(String, String)> {
    state
        .friend_requests_incoming()
        .unwrap()
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap_or_default().to_string(),
                r["from_uid"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn member_ids(info: &Value) -> Vec<String> {
    info["members"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            m["user_id"]
                .as_str()
                .or_else(|| m["member_id"].as_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

fn fresh_data_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-g01-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 一账号 + 一好友，桥已登录（本地库已开）。
async fn logged_in(tag: &str) -> (MockApp, MockHandle, String, std::path::PathBuf) {
    let (http, _quic, _platform) = stack().await;
    let data_dir = fresh_data_dir(tag);
    std::env::set_var("WXLIKE_DATA_DIR", &data_dir);
    let db = data_dir.join("messages.db");

    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register("bob", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice.register("alice", "pw").unwrap();
    befriend(&alice, &bob_id, &bob).await;

    let (app, handle) = boot();
    app.manage(alice);
    app.manage(bob);
    ok_data(
        &call(
            &handle,
            "identity.session.login",
            json!({"username": "alice", "password": "pw"}),
        ),
        "登录",
    );
    (app, handle, bob_id, db)
}

// ---------------------------------------------------------------------------
// 1. pin / mute
// ---------------------------------------------------------------------------

/// 置顶与免打扰共用一个处理器：分支只动自己那个标志，兄弟标志保持原值；而且
/// 返回/广播的投影两个字段都必须是 bool。#296 前：先免打扰再置顶，回执里
/// `pinned`/`muted` 有一侧是 `null`（契约是 boolean）。
// 锁要护住整个测试体（`WXLIKE_DATA_DIR` 是进程级的）：跨 await 持有是刻意的，
// 见文件头。clippy 只看类型，看不出这是测试串行化而不是共享状态。
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conversation_pin_and_mute_only_touch_their_own_flag() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (_app, handle, bob_id, db) = logged_in("pinmute").await;
    let conv = ok_data(
        &call(
            &handle,
            "messaging.conversation.ensure",
            json!({"user_id": bob_id}),
        ),
        "建会话",
    )["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let flags_key = format!("conv_flags:{conv}");

    // 先免打扰（此时 kv 里只有 muted 一个键）。
    let mute = call(
        &handle,
        "messaging.conversation.mute",
        json!({"conv_id": conv, "value": true}),
    );
    let mute_data = ok_data(&mute, "免打扰").clone();
    assert_eq!(mute_data["muted"], json!(true), "免打扰要生效：{mute_data}");
    assert!(
        mute_data["pinned"].is_boolean(),
        "#296：置顶从没设过时也必须报 bool，不能是 null（契约 Conversation.pinned: boolean）：{mute_data}"
    );
    assert_eq!(mute_data["pinned"], json!(false));
    assert_eq!(
        kv_json(&db, &flags_key),
        json!({"muted": true}),
        "mute 只写 muted"
    );

    // 再置顶：不能把 muted 冲掉。
    let pin = call(
        &handle,
        "messaging.conversation.pin",
        json!({"conv_id": conv, "value": true}),
    );
    let pin_data = ok_data(&pin, "置顶").clone();
    assert_eq!(pin_data["pinned"], json!(true));
    assert_eq!(
        pin_data["muted"],
        json!(true),
        "置顶不能顺手把免打扰清掉：{pin_data}"
    );
    assert!(pin_data["pinned"].is_boolean() && pin_data["muted"].is_boolean());
    assert_eq!(
        kv_json(&db, &flags_key),
        json!({"muted": true, "pinned": true}),
        "两条 action 各写各的键"
    );

    // 取消置顶：另一侧仍然原样。
    let unpin = call(
        &handle,
        "messaging.conversation.pin",
        json!({"conv_id": conv, "value": false}),
    );
    let unpin_data = ok_data(&unpin, "取消置顶").clone();
    assert_eq!(unpin_data["pinned"], json!(false));
    assert_eq!(
        unpin_data["muted"],
        json!(true),
        "取消置顶不该动免打扰：{unpin_data}"
    );

    std::env::remove_var("WXLIKE_DATA_DIR");
}

// ---------------------------------------------------------------------------
// 2. tag / star
// ---------------------------------------------------------------------------

/// 标签与星标共用一个处理器。#296 的实锤：给一个没设过标签的联系人加星标，
/// 返回/广播的 contact 里 `tags` 是 `null`，前端 `contacts-page.tsx` 的
/// `contact.tags.length` 当场 TypeError。
// 同上：整测试体串行化，跨 await 持锁是刻意的（见文件头）。
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn contact_tag_and_star_only_touch_their_own_field() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (_app, handle, bob_id, db) = logged_in("tagstar").await;
    let meta_key = format!("contact_meta:{bob_id}");

    // 全新联系人先星标（kv 里还没有 tags 键——就是 #296 崩的那条路径）。
    let star = call(
        &handle,
        "relations.star.set",
        json!({"user_id": bob_id, "star": true}),
    );
    let star_data = ok_data(&star, "星标").clone();
    assert_eq!(star_data["star"], json!(true), "星标要生效：{star_data}");
    assert!(
        star_data["tags"].is_array(),
        "#296：星标不该把没设过的 tags 写成 null（契约 Contact.tags: string[]，前端直接 .length）：{star_data}"
    );
    assert_eq!(star_data["tags"], json!([]));
    assert_eq!(
        kv_json(&db, &meta_key),
        json!({"star": true}),
        "star 只写 star"
    );

    // 再打标签：星标必须留着。
    let tag = call(
        &handle,
        "relations.tag.set",
        json!({"user_id": bob_id, "tags": ["家人", "同学"]}),
    );
    let tag_data = ok_data(&tag, "标签").clone();
    assert_eq!(tag_data["tags"], json!(["家人", "同学"]));
    assert_eq!(
        tag_data["star"],
        json!(true),
        "打标签不能把星标清掉：{tag_data}"
    );
    assert_eq!(
        kv_json(&db, &meta_key),
        json!({"star": true, "tags": ["家人", "同学"]}),
        "两条 action 各写各的键"
    );

    // 再改星标：标签必须留着。
    let unstar = call(
        &handle,
        "relations.star.set",
        json!({"user_id": bob_id, "star": false}),
    );
    let unstar_data = ok_data(&unstar, "取消星标").clone();
    assert_eq!(unstar_data["star"], json!(false));
    assert_eq!(
        unstar_data["tags"],
        json!(["家人", "同学"]),
        "改星标不能把标签写成 null/[]：{unstar_data}"
    );

    std::env::remove_var("WXLIKE_DATA_DIR");
}

// ---------------------------------------------------------------------------
// 3. accept / decline
// ---------------------------------------------------------------------------

/// 同意与拒绝共用一个处理器，但打的是两个不同端点、副作用确实相反：
/// 同意 → 进好友列表；拒绝 → 不进。这条是回归闸门——#296 复核结论：这里本来就
/// 是对的，别照着"共享即有罪"去改。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_accept_and_decline_perform_opposite_effects() {
    let (http, _quic, _platform) = stack().await;
    let alice_state = AppState::new(&http);
    let alice_id = alice_state.register("alice", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let carol = AppState::new(&http);
    let carol_id = carol.register("carol", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let dave = AppState::new(&http);
    let dave_id = dave.register("dave", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 两条待处理请求都落在 alice 的信箱里。按"请求前后的差集"认领新增那条，
    // 不靠 from_name 猜（显示名可能是空的）。
    let mut pairs: Vec<(String, String)> = Vec::new();
    for who in [&carol, &dave] {
        let before = inbox(&alice_state);
        who.friend_request(&alice_id, "g01").unwrap();
        let after = inbox(&alice_state);
        let fresh = after
            .iter()
            .find(|(id, _)| !before.iter().any(|(old, _)| old == id))
            .unwrap_or_else(|| panic!("新请求没到 alice 信箱：{after:?}"));
        pairs.push(fresh.clone());
    }
    let carol_req = pairs
        .iter()
        .find(|(_, from)| from == &carol_id)
        .expect("carol 的请求")
        .0
        .clone();
    let dave_req = pairs
        .iter()
        .find(|(_, from)| from == &dave_id)
        .expect("dave 的请求")
        .0
        .clone();

    let (app, handle) = boot();
    app.manage(alice_state);

    // 顺带钉死申请投影：`from.user_id` 必须认得出人（桥读错字段时这里是空串，
    // 新 UI 的"新好友"页就变成一排匿名申请）。
    let rows = ok_data(
        &call(&handle, "relations.requests.incoming", json!({})),
        "申请列表",
    )
    .clone();
    let seen: Vec<(String, String)> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["request_id"].as_str().unwrap_or_default().to_string(),
                r["from"]["user_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    for (id, who_id) in [(&carol_req, &carol_id), (&dave_req, &dave_id)] {
        let row = seen
            .iter()
            .find(|(rid, _)| rid == id)
            .unwrap_or_else(|| panic!("申请 {id} 要出现在投影里：{seen:?}"));
        assert_eq!(&row.1, who_id, "from.user_id 要对得上申请人：{seen:?}");
    }

    let accept = call(
        &handle,
        "relations.request.accept",
        json!({"request_id": carol_req.clone()}),
    );
    assert_eq!(ok_data(&accept, "同意")["status"], json!("ACCEPTED"));
    let decline = call(
        &handle,
        "relations.request.decline",
        json!({"request_id": dave_req.clone()}),
    );
    assert_eq!(
        ok_data(&decline, "拒绝")["status"],
        json!("DECLINED"),
        "拒绝必须回 DECLINED，不能照抄同意的回执"
    );

    let friends: Vec<String> = handle
        .state::<AppState>()
        .friends()
        .unwrap()
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|f| f["friend_id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(friends.contains(&carol_id), "同意→成为好友：{friends:?}");
    assert!(
        !friends.contains(&dave_id),
        "拒绝→不能成为好友（共享处理器串了分支）：{friends:?}"
    );

    let pending: Vec<String> = inbox(&handle.state::<AppState>())
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(
        !pending.contains(&carol_req) && !pending.contains(&dave_req),
        "两条请求都要被消费掉：{pending:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. relations.group.kick（#296 的目录漂移：挂了路由却恒返 NOT_IMPLEMENTED）
// ---------------------------------------------------------------------------

/// 踢人要真做成：桥走服务端已有的 `DELETE /api/groups/{id}/members/{uid}`，
/// 成员表里真的少一个人。缺参数/非群主/重复踢都要如实报错，不许伪造成功。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_kick_removes_the_member_for_real() {
    let (http, _quic, _platform) = stack().await;
    let alice_state = AppState::new(&http);
    let alice_id = alice_state.register("alice", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let bob = AppState::new(&http);
    let bob_id = bob.register("bob", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let group = alice_state
        .groups_create("踢人群", std::slice::from_ref(&bob_id))
        .unwrap();
    assert!(
        member_ids(&alice_state.group_info(&group).unwrap()).contains(&bob_id),
        "踢之前 bob 要在群里"
    );

    let (app, handle) = boot();
    app.manage(alice_state);
    let info = || handle.state::<AppState>().group_info(&group).unwrap();

    // 缺参数：先拦掉，不能拼出 `/api/groups//members/` 这种畸形路径。
    let malformed = call(
        &handle,
        "relations.group.kick",
        json!({"group_id": group, "user_id": ""}),
    );
    error(&malformed, "空 user_id");
    assert_eq!(
        malformed["error"]["code"],
        json!("VALIDATION"),
        "缺参数要报校验错：{malformed}"
    );
    assert!(
        member_ids(&info()).contains(&bob_id),
        "失败的调用不许改成员表"
    );

    let kick = call(
        &handle,
        "relations.group.kick",
        json!({"group_id": group, "user_id": bob_id.clone()}),
    );
    let data = ok_data(
        &kick,
        "踢人（#296 之前恒返 NOT_IMPLEMENTED，群页的踢人按钮永远失败）",
    )
    .clone();
    assert_eq!(data["removed"], json!(bob_id), "回执要说明移除了谁：{data}");
    assert_eq!(data["group_id"], json!(group));
    let after = member_ids(&info());
    assert!(
        !after.contains(&bob_id),
        "成员表里必须真的没有他了：{after:?}"
    );
    assert!(after.contains(&alice_id), "群主还在群里：{after:?}");

    // 再踢一次：人已不在 → 服务端报错，桥如实传递，不假装成功。
    error(
        &call(
            &handle,
            "relations.group.kick",
            json!({"group_id": group, "user_id": bob_id}),
        ),
        "重复踢人",
    );

    // 外人来踢群主：失败，且成员表不变（`remove_member` 是 owner-only）。
    let erin = AppState::new(&http);
    erin.register("erin", "pw").unwrap();
    let (app2, handle2) = boot();
    app2.manage(erin);
    error(
        &call(
            &handle2,
            "relations.group.kick",
            json!({"group_id": group, "user_id": alice_id.clone()}),
        ),
        "非群主踢人",
    );
    assert!(
        member_ids(&info()).contains(&alice_id),
        "被拒的踢人不能改动成员表"
    );
}

/// 漂移的运行时视角：`capabilities()` 报的目录里必须有 kick（前端从这条发现能力），
/// 且这条目录与 `IMPLEMENTED_ACTIONS` 逐条相等。与 `tests/catalog_drift.rs` 的
/// 静态对账互补：那边管"源码别漂"，这边管"上报的确实是这份表"。
#[test]
fn kick_is_advertised_as_implemented() {
    let caps = capabilities();
    let advertised: Vec<&str> = caps["actions"]
        .as_array()
        .expect("capabilities 要有 actions 数组")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        advertised.contains(&"relations.group.kick"),
        "kick 已实现，capabilities 必须报出来，否则 UI 无法发现：{advertised:?}"
    );
    assert_eq!(advertised, IMPLEMENTED_ACTIONS.to_vec());
}
