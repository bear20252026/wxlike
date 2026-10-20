//! B1 的 Rust 侧验收：绑定层在**没有手机**的情况下能验的部分全部在这里。
//!
//! 打的是本机起的假 HTTP 服务（`crates/client` 只会走 HTTP），断言的是行为：
//! 幂等键、离线兜底、错误分类、队列状态机、凭据落盘。不测 uniffi 生成的代码——
//! 那是 `crates/ffi/regenerate.sh` 与 `apps/mobile/test/ffi_smoke_test.dart` 的事。
#![allow(unknown_lints, linker_messages)] // 见 crates/ffi/src/lib.rs 里的同一条说明

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use wxlike_ffi::{classify, direct_conv_id, LiveState, OutboxState, WxlikeClient, WxlikeError};

// ------------------------------------------------------------------ 假服务端

#[derive(Clone)]
struct Rules {
    /// 非 200 时 `POST /api/conv/*/send` 走这条路（测"服务端明确拒绝"）
    send_status: u16,
    send_error: &'static str,
    login_status: u16,
    /// 服务端返回的 token。带 CR/LF 的必须被绑定层拒掉（请求头注入）。
    login_token: &'static str,
    /// 空 = 用默认的三行（bob 的文本、我的撤回、对方的撤回）
    history_rows: Vec<Value>,
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            send_status: 200,
            send_error: "",
            login_status: 200,
            login_token: "tok-1",
            history_rows: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct Hit {
    method: String,
    path: String,
    body: String,
}

struct FakeServer {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<Hit>>>,
    stop: Arc<AtomicBool>,
}

impl FakeServer {
    fn start(rules: Rules) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_hits, thread_stop) = (hits.clone(), stop.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if thread_stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let Ok(hit) = read_request(&mut stream) else {
                    continue;
                };
                thread_hits.lock().expect("hits lock").push(hit.clone());
                let (status, body) = respond(&hit, &rules);
                let _ = write_response(&mut stream, status, &body);
            }
        });
        Self { addr, hits, stop }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().expect("hits lock").clone()
    }

    fn sent_bodies(&self) -> Vec<String> {
        self.hits()
            .into_iter()
            .filter(|h| h.method == "POST" && h.path.ends_with("/send"))
            .map(|h| h.body)
            .collect()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// 一个"必然连不上"的地址：绑定后立刻释放，回环上连它得到 connection refused。
/// 用它模拟飞行模式，比让假服务端返回 500 诚实——500 不是断网。
fn dead_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    format!("http://{addr}")
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Hit> {
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        let read = stream.read(&mut byte)?;
        if read == 0 {
            break;
        }
        buf.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&buf).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let content_length = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        stream.read_exact(&mut body)?;
    }
    Ok(Hit {
        method,
        path,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    if status == 204 {
        return stream.write_all(
            b"HTTP/1.1 204 No Content\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        );
    }
    let head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())
}

fn respond(hit: &Hit, rules: &Rules) -> (u16, String) {
    let path = hit.path.split('?').next().unwrap_or_default().to_string();
    let reply = |value: Value| (200u16, value.to_string());
    match (hit.method.as_str(), path.as_str()) {
        ("POST", "/api/register") | ("POST", "/api/login") if rules.login_status != 200 => (
            rules.login_status,
            json!({ "error": "wrong password" }).to_string(),
        ),
        ("POST", "/api/register") | ("POST", "/api/login") => reply(json!({
            "user_id": "alice",
            "token": rules.login_token,
        })),
        ("GET", "/api/me") => reply(json!({ "user_id": "alice", "name": "alice" })),
        ("GET", "/api/groups/mine") => {
            reply(json!([{"id": "g-1", "name": "项目组", "member_count": 3}]))
        }
        ("GET", "/api/friends") => reply(json!([{"friend_id": "bob"}])),
        ("POST", "/api/devices") => (204, String::new()),
        _ if path.starts_with("/api/conv/") && path.ends_with("/unread") => {
            let conv = trim_conv(&path, "/unread");
            reply(json!({
                "conv": conv, "owner": "direct",
                "head_seq": 7, "read_seq": 5, "unread": 2,
            }))
        }
        _ if path.starts_with("/api/conv/") && path.ends_with("/read") => {
            let conv = trim_conv(&path, "/read");
            reply(json!({
                "conv": conv, "owner": "direct",
                "head_seq": 7, "read_seq": 7, "unread": 0,
            }))
        }
        _ if path.starts_with("/api/conv/") && path.ends_with("/send") => {
            if rules.send_status != 200 {
                return (
                    rules.send_status,
                    json!({ "error": rules.send_error }).to_string(),
                );
            }
            let conv = trim_conv(&path, "/send");
            let key = serde_json::from_str::<Value>(&hit.body)
                .ok()
                .and_then(|v| v["idempotency_key"].as_str().map(str::to_string))
                .unwrap_or_default();
            reply(json!({
                "msg_id": format!("srv-{}", &key.chars().take(8).collect::<String>()),
                "conv_id": conv,
                "accepted": true,
            }))
        }
        _ if path.starts_with("/api/history/") => {
            let conv = path.trim_start_matches("/api/history/").to_string();
            let rows = if rules.history_rows.is_empty() {
                default_history(&conv)
            } else {
                rules.history_rows.clone()
            };
            let next = if rows.is_empty() {
                Value::Null
            } else {
                json!(
                    rows.iter()
                        .filter_map(|r| r["seq"].as_i64())
                        .min()
                        .unwrap_or(0)
                        - 1
                )
            };
            reply(json!({ "conv_id": conv, "messages": rows, "next_before_seq": next }))
        }
        _ => (
            404,
            json!({ "error": format!("no fake route for {path}") }).to_string(),
        ),
    }
}

/// `/api/conv/{conv}/{suffix}` 里取回 conv。
fn trim_conv(path: &str, suffix: &str) -> String {
    path.strip_prefix("/api/conv/")
        .and_then(|rest| rest.strip_suffix(suffix))
        .unwrap_or_default()
        .to_string()
}

/// 三行：别人发的文本、我撤回的、别人撤回的——正好覆盖 kind 文案规则。
fn default_history(conv: &str) -> Vec<Value> {
    vec![
        json!({
            "conv_id": conv, "seq": 5, "sender": "bob", "msg_id": "h5",
            "sent_at_ms": 1_700_000_000_005i64, "withdrawn": false, "kind": "text",
            "text": "在",
        }),
        json!({
            "conv_id": conv, "seq": 6, "sender": "alice", "msg_id": "h6",
            "sent_at_ms": 1_700_000_000_006i64, "withdrawn": false, "kind": "recall",
        }),
        json!({
            "conv_id": conv, "seq": 7, "sender": "bob", "msg_id": "h7",
            "sent_at_ms": 1_700_000_000_007i64, "withdrawn": true, "kind": "recall",
        }),
    ]
}

fn db_path(tag: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("wxlike-ffi-{}-{tag}.sqlite", std::process::id()));
    // 上一轮留下的 token 会让这一轮的"已登录"断言变成假绿
    let _ = std::fs::remove_file(&path);
    path
}

fn client(url: &str, db: &Path) -> Arc<WxlikeClient> {
    WxlikeClient::new(url.to_string(), db.display().to_string()).expect("create_client")
}

// ---------------------------------------------------------------------- 测试

#[test]
fn login_persists_so_a_restart_does_not_need_the_password_again() {
    let server = FakeServer::start(Rules::default());
    let db = db_path("login");
    {
        let first = client(&server.url(), &db);
        let session = first.login("alice".into(), "pw".into()).expect("login");
        assert_eq!(session.user_id, "alice");
        assert!(session.signed_in);
    }
    // 重开一个客户端：token 从本地库回来，不需要再输一次密码
    let again = client(&server.url(), &db);
    assert!(again.session().signed_in, "token 没落盘");
    let info = again.connect().expect("connect");
    assert_eq!(info.state, LiveState::Online);
    assert_eq!(info.user_id, "alice");
}

#[test]
fn wrong_password_is_unauthorized_and_stays_signed_out() {
    let server = FakeServer::start(Rules {
        login_status: 401,
        ..Rules::default()
    });
    let db = db_path("badpw");
    let c = client(&server.url(), &db);
    let err = c.login("alice".into(), "nope".into()).unwrap_err();
    assert!(
        matches!(err, WxlikeError::Unauthorized { .. }),
        "401 被分成了 {err}"
    );
    assert!(!err.retryable(), "鉴权失败不该重试");
    assert!(!c.session().signed_in);
}

/// token 会原样进 `Authorization` 头。带 CR/LF 的 token 就是请求头注入的入口，
/// 绑定层必须在存下来之前拒掉——这跟"服务端可信"无关，是边界上的自检。
#[test]
fn a_token_with_a_newline_is_never_stored_or_sent() {
    let server = FakeServer::start(Rules {
        login_token: "bad\r\nX-Injected: 1",
        ..Rules::default()
    });
    let db = db_path("crlftoken");
    let c = client(&server.url(), &db);
    let err = c.login("alice".into(), "pw".into()).unwrap_err();
    assert!(matches!(err, WxlikeError::Unexpected { .. }), "{err}");
    assert!(!c.session().signed_in, "坏 token 不该被当成登录成功");
    // 重启一次：本地库里存的也必须是不可用的空值，而不是那条注入串
    let again = client(&server.url(), &db);
    assert!(!again.session().signed_in);
}

#[test]
fn offline_send_is_queued_then_a_second_client_flushes_it_unchanged() {
    let db = db_path("offline");
    let offline = dead_url();
    let task_id;
    {
        let a = client(&offline, &db);
        let outcome = a
            .send_text("d-alice~bob".into(), "地铁里发的".into())
            .expect("send_text");
        assert_eq!(outcome.state, OutboxState::Pending, "断网必须排队");
        assert!(
            matches!(classify(&outcome.error), WxlikeError::Network { .. }),
            "排队的原因必须是网络类：{}",
            outcome.error
        );
        assert!(outcome.msg_id.is_empty(), "没发出去就没有 msg_id");
        task_id = outcome.task_id;
        let status = a.outbox_status().expect("outbox_status");
        assert_eq!(status.pending, 1);
        assert_eq!(status.entries[0].text, "地铁里发的");
    }
    let server = FakeServer::start(Rules::default());
    let b = client(&server.url(), &db);
    // 退避窗口没到就不该发：`record_failure` 把 next_retry_at 推后了 1 秒，
    // 提前撞墙等于白烧一次尝试次数（一共只有 5 次）。
    assert_eq!(
        b.flush_outbox().expect("flush too early").sent,
        0,
        "退避没到就发，是把队列当成了急着送"
    );
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    let report = b.flush_outbox().expect("flush");
    assert_eq!(report.sent, 1);
    assert_eq!(b.outbox_status().expect("status").pending, 0);
    // 幂等键 = 入队时的 task_id：重试不会变成两条消息
    let bodies = server.sent_bodies();
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].contains(&task_id), "{}", bodies[0]);
    assert!(bodies[0].contains("地铁里发的"), "{}", bodies[0]);
}

#[test]
fn history_falls_back_to_the_local_copy_when_offline() {
    let db = db_path("history");
    let server = FakeServer::start(Rules::default());
    let online = client(&server.url(), &db);
    online.login("alice".into(), "pw".into()).expect("login");
    let live = online
        .history("d-alice~bob".into(), 0, 50)
        .expect("history");
    assert!(!live.from_cache);
    assert_eq!(live.messages.len(), 3);

    // 同一个库、连不上的地址 = 应用重启后进飞行模式
    let offline = client(&dead_url(), &db);
    let cached = offline
        .history("d-alice~bob".into(), 0, 50)
        .expect("cached");
    assert!(cached.from_cache, "离线时必须是本地那一份");
    assert_eq!(cached.messages.len(), 3, "断网看历史是 B3 的验收项");
    assert_eq!(cached.messages[0].msg_id, "h5");
    assert_eq!(
        offline.live_info().expect("live").state,
        LiveState::Offline,
        "兜底之后状态要如实变成离线"
    );
}

#[test]
fn sent_message_is_cached_once_and_survives_as_one_row() {
    let db = db_path("dedupe");
    let server = FakeServer::start(Rules::default());
    let c = client(&server.url(), &db);
    c.login("alice".into(), "pw".into()).expect("login");
    let outcome = c
        .send_text("d-alice~bob".into(), "只有一条".into())
        .expect("send");
    assert_eq!(outcome.state, OutboxState::Sent);
    assert!(!outcome.msg_id.is_empty());

    // 发送成功先按占位 seq 落本地；服务端的这一页没有它（假服务端只回固定的三行），
    // 所以离线那一页里它必须还在——而且只有一行。
    let offline = client(&dead_url(), &db);
    let page = offline.history("d-alice~bob".into(), 0, 50).expect("page");
    let mine = page
        .messages
        .iter()
        .filter(|m| m.text == "只有一条")
        .count();
    assert_eq!(mine, 1, "发送成功的消息在本地只能有一行");
    assert!(
        page.messages
            .iter()
            .find(|m| m.text == "只有一条")
            .expect("row")
            .mine
    );
}

#[test]
fn server_rejection_fails_the_message_instead_of_retrying_it() {
    let server = FakeServer::start(Rules {
        send_status: 403,
        send_error: "you are not a member",
        ..Rules::default()
    });
    let db = db_path("denied");
    let c = client(&server.url(), &db);
    c.login("alice".into(), "pw".into()).expect("login");
    let outcome = c
        .send_text("g-1".into(), "不该发出去".into())
        .expect("send_text");
    assert_eq!(outcome.state, OutboxState::Failed, "403 重试五次也不会成功");
    assert!(outcome.error.contains("not a member"), "{}", outcome.error);
    let status = c.outbox_status().expect("status");
    assert_eq!(status.failed, 1);
    assert_eq!(status.pending, 0);
    // 再 flush 一轮：failed 的行不该被重发
    c.flush_outbox().expect("flush");
    assert_eq!(server.sent_bodies().len(), 1, "判死的消息不能再发");
}

#[test]
fn a_denied_command_never_reaches_the_queue() {
    let server = FakeServer::start(Rules::default());
    let db = db_path("precheck");
    let c = client(&server.url(), &db);
    c.login("alice".into(), "pw".into()).expect("login");
    // 空 conv：Policy 在入队之前就拒（MissingConv），队列里不该留下僵尸行
    let err = c.send_text(String::new(), "空的".into()).unwrap_err();
    assert!(matches!(err, WxlikeError::Permission { .. }), "{err}");
    assert_eq!(c.outbox_status().expect("status").pending, 0);
    assert!(server.sent_bodies().is_empty(), "被拒的命令不该发出请求");
}

#[test]
fn recall_rows_are_worded_by_who_recalled() {
    let server = FakeServer::start(Rules::default());
    let db = db_path("recall");
    let c = client(&server.url(), &db);
    c.login("alice".into(), "pw".into()).expect("login");
    let page = c.history("d-alice~bob".into(), 0, 50).expect("history");
    let by_seq = |seq: i64| {
        page.messages
            .iter()
            .find(|m| m.seq == seq)
            .unwrap_or_else(|| panic!("没有 seq {seq}"))
            .text
            .clone()
    };
    assert_eq!(by_seq(6), "你撤回了一条消息", "自己撤的要按我说");
    assert_eq!(by_seq(7), "对方撤回了一条消息");
    assert_eq!(by_seq(5), "在");
    assert!(page.messages.iter().find(|m| m.seq == 6).expect("row").mine);
}

#[test]
fn unread_and_mark_read_come_straight_from_the_mailbox() {
    let server = FakeServer::start(Rules::default());
    let db = db_path("unread");
    let c = client(&server.url(), &db);
    c.login("alice".into(), "pw".into()).expect("login");
    let unread = c.unread("d-alice~bob".into()).expect("unread");
    assert_eq!((unread.head_seq, unread.read_seq, unread.unread), (7, 5, 2));
    assert_eq!(unread.owner, "direct");
    let after = c.mark_read("d-alice~bob".into(), 0).expect("mark_read");
    assert_eq!(after.unread, 0);
    assert_eq!(after.read_seq, 7);
    assert!(server
        .hits()
        .iter()
        .any(|h| h.path == "/api/conv/d-alice~bob/read"),);
}

#[test]
fn conversation_list_mixes_groups_and_direct_chats_and_caches_them() {
    let server = FakeServer::start(Rules::default());
    let db = db_path("convs");
    let c = client(&server.url(), &db);
    c.login("alice".into(), "pw".into()).expect("login");
    let rows = c.conversations().expect("conversations").conversations;
    let group = rows.iter().find(|r| r.kind == "group").expect("group row");
    assert_eq!(
        (
            group.conv.as_str(),
            group.title.as_str(),
            group.members,
            group.unread
        ),
        ("g-1", "项目组", 3, 2)
    );
    let direct = rows
        .iter()
        .find(|r| r.kind == "direct")
        .expect("direct row");
    assert_eq!(direct.title, "bob");
    assert_eq!(direct.conv, direct_conv_id("alice".into(), "bob".into()));

    // 离线时读的是本地那一份，并且必须说清楚那是缓存
    let offline = client(&dead_url(), &db);
    let cached = offline.conversations().expect("cached list");
    assert!(cached.from_cache, "离线兜底要标出来，不能装作是最新的");
    assert_eq!(
        cached.conversations.len(),
        rows.len(),
        "会话列表要能在断网时打开"
    );
}

#[test]
fn pending_count_shows_up_on_the_conversation_row() {
    let db = db_path("badge");
    let offline = dead_url();
    let a = client(&offline, &db);
    a.send_text("d-alice~bob".into(), "待发送".into())
        .expect("queue it");
    // 有 token 才能读会话列表；直接写进本地库模拟"上次登录过"
    let server = FakeServer::start(Rules::default());
    let b = client(&server.url(), &db);
    b.login("alice".into(), "pw".into()).expect("login");
    let rows = b.conversations().expect("conversations").conversations;
    let direct = rows
        .iter()
        .find(|r| r.conv == "d-alice~bob")
        .expect("direct row");
    assert_eq!(direct.pending, 1, "会话列表上要看得见有几条没发出去");
}

#[test]
fn push_token_goes_to_the_device_registry() {
    let server = FakeServer::start(Rules::default());
    let db = db_path("push");
    let c = client(&server.url(), &db);
    c.login("alice".into(), "pw".into()).expect("login");
    c.register_push_token("android".into(), "fcm-token-1".into())
        .expect("register");
    let hit = server
        .hits()
        .into_iter()
        .find(|h| h.path == "/api/devices")
        .expect("device hit");
    assert_eq!(hit.method, "POST");
    assert!(
        hit.body.contains("\"platform\":\"android\""),
        "{}",
        hit.body
    );
    assert!(hit.body.contains("fcm-token-1"), "{}", hit.body);
    let err = c
        .register_push_token("android".into(), "  ".into())
        .unwrap_err();
    assert!(matches!(err, WxlikeError::Unexpected { .. }), "{err}");
}

#[test]
fn logout_clears_credentials_messages_and_the_queue() {
    let db = db_path("logout");
    let a = client(&dead_url(), &db);
    let queued = a
        .send_text("d-alice~bob".into(), "留给下一个人".into())
        .expect("queued");
    assert_eq!(queued.state, OutboxState::Pending);
    let server = FakeServer::start(Rules::default());
    let b = client(&server.url(), &db);
    b.login("alice".into(), "pw".into()).expect("login");
    b.logout().expect("logout");
    assert!(!b.session().signed_in, "token 没清");
    assert_eq!(
        b.outbox_status().expect("status").pending,
        0,
        "队列属于上一个人"
    );
    // 假服务端不看 Authorization，所以"本地库真的空了"必须用离线客户端验：
    // 那时读到的只可能是本地那一份。
    let after = client(&dead_url(), &db);
    assert!(
        after
            .conversations()
            .expect("list")
            .conversations
            .is_empty(),
        "会话缓存要清掉"
    );
    assert!(
        after
            .history("d-alice~bob".into(), 0, 50)
            .expect("history")
            .messages
            .is_empty(),
        "本地正文要清掉"
    );
}

#[test]
fn retry_button_resends_the_same_task_id() {
    let server = FakeServer::start(Rules {
        send_status: 422,
        send_error: "message too long",
        ..Rules::default()
    });
    let db = db_path("retry");
    let c = client(&server.url(), &db);
    c.login("alice".into(), "pw".into()).expect("login");
    let first = c
        .send_text("d-alice~bob".into(), "太长".into())
        .expect("send");
    assert_eq!(first.state, OutboxState::Failed);
    let status = c.retry_outbox(first.task_id.clone()).expect("retry");
    assert_eq!(status.failed, 1, "还是同一条失败的消息");
    let bodies = server.sent_bodies();
    assert_eq!(bodies.len(), 2);
    assert_eq!(
        bodies[0], bodies[1],
        "重试必须带同一个幂等键，否则服务端会当成两条"
    );
    assert!(c.drop_outbox(first.task_id.clone()).expect("drop"));
    assert_eq!(c.outbox_status().expect("status").failed, 0);
    assert!(!c.drop_outbox(first.task_id).expect("drop again"));
}

#[test]
fn an_unopenable_database_is_reported_instead_of_swallowed() {
    let mut blocker = std::env::temp_dir();
    blocker.push(format!("wxlike-ffi-blocker-{}", std::process::id()));
    std::fs::write(&blocker, b"not a directory").expect("write blocker");
    let mut bad = blocker.clone();
    bad.push("nested");
    bad.push("db.sqlite");
    let err = match WxlikeClient::new("http://127.0.0.1:1".into(), bad.display().to_string()) {
        Ok(_) => panic!("目录创建不了就必须报错"),
        Err(err) => err,
    };
    assert!(
        matches!(err, WxlikeError::Unexpected { .. }),
        "本地库问题不该被当成网络错误：{err}"
    );
    let _ = std::fs::remove_file(&blocker);
}
