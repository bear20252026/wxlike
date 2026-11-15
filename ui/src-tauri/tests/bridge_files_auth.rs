//! D6（文件域逐域退役）：文件中心控制面经桥走认证 HTTP（G04 `/api/v1/files*`）。
//!
//! 与钱包/通话/视频/内容域同口径：会话留在 Rust 侧，控制面由 core_dispatch 代持
//! （file.list/rename/share/remove，见 core_bridge/files.inc）。二进制上传/下载
//! 与备份清理计划无 JSON 流式提供者，在消费侧显式阻塞，不在本桥。
//!
//! 全程真服务端栈（support::stack()，组合根已挂载 G04）。逐条钉住：
//! 1. 没登录 → UNAUTHENTICATED，且一个字节都不发（死端口判别）；
//! 2. 畸形 id 在发请求之前拦掉（VALIDATION）；
//! 3. 上传建对象 → 列表含它 → 改名 → 分享 → 删除 往返；
//! 4. Bearer 只出 Rust：任何 action 返回值里都找不到它。

use serde_json::{json, Value};
use sha2::Digest as _;
use tauri::Manager as _;
use wxlike_ui_lib::core_bridge::{dispatch, CoreRequest, CoreState, IMPLEMENTED_ACTIONS};
use wxlike_ui_lib::AppState;

mod support;

use crate::support::stack;

type MockApp = tauri::App<tauri::test::MockRuntime>;
type MockHandle = tauri::AppHandle<tauri::test::MockRuntime>;

fn request(action: &str, params: Value, idem: Option<&str>) -> CoreRequest {
    CoreRequest {
        action_id: action.to_string(),
        request_id: format!("d6-files-{action}"),
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
    reply
        .get("data")
        .unwrap_or_else(|| panic!("{what} 的成功信封必须带 data：{reply}"))
}

fn error<'a>(reply: &'a Value, what: &str, want: &str) -> &'a Value {
    assert_eq!(
        reply["ok"],
        json!(false),
        "{what} 必须失败，不许伪造成功：{reply}"
    );
    assert!(
        reply.get("data").is_none(),
        "{what} 失败时不许带 data：{reply}"
    );
    let err = &reply["error"];
    assert_eq!(err["code"], json!(want), "{what} 的错误码不对：{reply}");
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

fn signed_in(http: &str, name: &str) -> (MockApp, MockHandle, String) {
    let (app, handle) = boot();
    let state = AppState::new(http);
    let user_id = register(&state, name);
    app.manage(state);
    (app, handle, user_id)
}

// ---------------------------------------------------------------------------
// G04 分片上传夹具：直接打 `/api/v1/uploads*` 建一个真实对象 + 文件中心行，
// 供 file.* 控制面往返使用。上传本身不是本桥的 action（二进制面无 JSON 提供者），
// 这里只是测试夹具，凭据用注册返回的会话 token。
// ---------------------------------------------------------------------------

fn upload_fixture(http: &str, token: &str, name: &str, bytes: &[u8]) -> String {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .proxy(None)
        .build()
        .into();
    let base = http.trim_end_matches('/');
    let digest = hex::encode(sha2::Sha256::digest(bytes).as_slice());
    let auth = format!("Bearer {token}");

    // start
    let start_body = json!({
        "purpose": "file",
        "name": name,
        "size": bytes.len(),
        "mime": "text/plain",
        "digest": digest,
    });
    let mut start_resp = agent
        .post(format!("{base}/api/v1/uploads"))
        .header("authorization", &auth)
        .send_json(&start_body)
        .expect("upload start 传输失败");
    assert!(
        start_resp.status().is_success(),
        "upload start 状态：{}",
        start_resp.status()
    );
    let session: Value = start_resp
        .body_mut()
        .read_json()
        .expect("upload start 响应非 JSON");
    let upload_id = session["upload_id"]
        .as_str()
        .expect("upload start 没有 upload_id")
        .to_string();

    // chunk（一次性整块，offset=0）
    let mut chunk_resp = agent
        .put(format!("{base}/api/v1/uploads/{upload_id}/chunks?offset=0"))
        .header("authorization", &auth)
        .header("x-chunk-sha256", &digest)
        .send(bytes)
        .expect("upload chunk 传输失败");
    assert!(
        chunk_resp.status().is_success(),
        "upload chunk 状态：{}",
        chunk_resp.status()
    );
    let _progress: Value = chunk_resp
        .body_mut()
        .read_json()
        .expect("upload chunk 响应非 JSON");

    // complete
    let mut complete_resp = agent
        .post(format!("{base}/api/v1/uploads/{upload_id}/complete"))
        .header("authorization", &auth)
        .send_json(&json!({ "digest": digest }))
        .expect("upload complete 传输失败");
    assert!(
        complete_resp.status().is_success(),
        "upload complete 状态：{}",
        complete_resp.status()
    );
    let completed: Value = complete_resp
        .body_mut()
        .read_json()
        .expect("upload complete 响应非 JSON");
    completed["file_id"]
        .as_str()
        .expect("upload complete 没有 file_id")
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unauthenticated_file_actions_never_touch_the_network() {
    let (app, handle) = boot();
    app.manage(AppState::new("http://127.0.0.1:1"));

    let cases: Vec<(&str, Value)> = vec![
        ("file.list", json!({})),
        ("file.rename", json!({"file_id": "f1", "name": "x"})),
        ("file.share", json!({"file_id": "f1"})),
        ("file.remove", json!({"file_id": "f1"})),
    ];
    for (action, params) in cases {
        let reply = call(&handle, action, params);
        let err = error(&reply, action, "UNAUTHENTICATED");
        assert!(
            !err["message"]
                .as_str()
                .unwrap_or_default()
                .contains("传输失败"),
            "{action} 必须没发出任何请求：{reply}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_ids_are_refused_before_the_socket() {
    let (http, _quic, _platform) = stack().await;
    let (_app, handle, _id) = signed_in(&http, "f_mal");

    let cases: Vec<(&str, Value)> = vec![
        ("file.rename", json!({"name": "x"})),
        ("file.rename", json!({"file_id": "", "name": "x"})),
        ("file.rename", json!({"file_id": "../admin", "name": "x"})),
        ("file.rename", json!({"file_id": "f1"})),
        ("file.share", json!({"file_id": "a b"})),
        ("file.remove", json!({"file_id": "x?y=1"})),
    ];
    for (action, params) in cases {
        let reply = call(&handle, action, params);
        error(&reply, &format!("{action} 的畸形参数"), "VALIDATION");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_lifecycle_round_trip() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "f_author");
    let token = handle
        .state::<AppState>()
        .auth_token()
        .expect("登录后要有凭据");

    // 上传建对象（G04 夹具）→ file_id。
    let file_id = upload_fixture(&http, &token, "纪要.txt", b"hello files");

    // 列表：含刚上传的文件。
    let list = ok_data(&call(&handle, "file.list", json!({"limit": 50})), "列表").clone();
    let items = list["items"].as_array().cloned().unwrap_or_default();
    let mine = items
        .iter()
        .find(|f| f["file_id"] == json!(file_id))
        .unwrap_or_else(|| panic!("列表里没有刚上传的文件 {file_id}：{list}"));
    assert_eq!(mine["name"], json!("纪要.txt"), "{mine}");

    // 改名 → FileItem，name 更新。
    let renamed = ok_data(
        &call_with(
            &handle,
            "file.rename",
            json!({"file_id": file_id, "name": "修订.txt"}),
            Some("rn-1"),
        ),
        "改名",
    )
    .clone();
    assert_eq!(renamed["name"], json!("修订.txt"), "{renamed}");
    assert_eq!(renamed["file_id"], json!(file_id), "{renamed}");

    // 分享 → ShareGrant {url, expires_at, token, share_id}。
    let shared = ok_data(
        &call_with(
            &handle,
            "file.share",
            json!({"file_id": file_id}),
            Some("sh-1"),
        ),
        "分享",
    )
    .clone();
    assert!(
        shared["url"]
            .as_str()
            .is_some_and(|u| u.contains("/api/v1/objects/")),
        "{shared}"
    );
    assert!(
        shared["expires_at"].as_i64().is_some_and(|t| t > 0),
        "{shared}"
    );

    // 删除 → 204 → {}。
    ok_data(
        &call(&handle, "file.remove", json!({"file_id": file_id})),
        "删除",
    );

    // 删除后列表不再含它。
    let after = ok_data(
        &call(&handle, "file.list", json!({"limit": 50})),
        "删后列表",
    )
    .clone();
    let still = after["items"].as_array().cloned().unwrap_or_default();
    assert!(
        !still.iter().any(|f| f["file_id"] == json!(file_id)),
        "删除后仍在列表：{after}"
    );

    let _ = app;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remove_unknown_file_is_honest_validation() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "f_404");
    // 不存在的文件：服务端 404 → 桥 VALIDATION，绝不伪造成功。
    let reply = call(&handle, "file.remove", json!({"file_id": "file_nope"}));
    error(&reply, "删除不存在文件", "VALIDATION");
    let _ = app;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bearer_token_never_crosses_the_bridge() {
    let (http, _quic, _platform) = stack().await;
    let (app, handle, _id) = signed_in(&http, "f_tok");
    let token = handle
        .state::<AppState>()
        .auth_token()
        .expect("登录后要有凭据");
    assert!(!token.is_empty());

    let file_id = upload_fixture(&http, &token, "秘密.txt", b"secret bytes");
    let seen = vec![
        call(&handle, "file.list", json!({})),
        call(
            &handle,
            "file.rename",
            json!({"file_id": file_id, "name": "改名.txt"}),
        ),
        call(&handle, "file.share", json!({"file_id": file_id})),
        call(&handle, "file.remove", json!({"file_id": "file_nope"})),
    ];
    for reply in seen {
        let text = reply.to_string();
        assert!(!text.contains(&token), "凭据不许跨过桥：{text}");
        assert!(!text.contains("Bearer"), "桥不该把鉴权头回给 UI：{text}");
    }
    let _ = app;
}

#[test]
fn file_actions_are_advertised_in_the_catalog() {
    for action in ["file.list", "file.rename", "file.share", "file.remove"] {
        assert!(
            IMPLEMENTED_ACTIONS.contains(&action),
            "{action} 必须让 UI 能发现"
        );
    }
}
