//! 命令执行器：catalog → HTTP。
//!
//! 这一层只做三件事：查权限、按 id 分派、把参数拼进请求。它不知道调用方
//! 是命令行还是 MCP，也不知道 UI 存在——那正是"一套命令、多个入口"的前提。

use serde_json::{json, Value};

use wxlike_cmd::policy::Policy;

use crate::Http;

/// 兜底错误的标记：测试据此判断"catalog 里说有、执行器里没做"。
pub const UNIMPLEMENTED: &str = "尚未实现";

/// 命令执行器：把 catalog 里的 id 翻译成 HTTP 调用。
///
/// CLI 与 MCP server 共用这一份——**执行逻辑只有一处**，协议层（命令行 /
/// JSON-RPC）各自只负责解析参数、包装输出。
pub struct Executor {
    http: Http,
    policy: Policy,
}

impl Executor {
    pub fn new(http: Http, policy: Policy) -> Self {
        Self { http, policy }
    }

    pub fn http(&self) -> &Http {
        &self.http
    }

    /// 执行一条命令。**权限先于网络**：被拒的命令不会发出任何请求。
    ///
    /// 分派走 `HANDLERS` 表而不是 `match`：加一条命令 = catalog 加一条 +
    /// 表里加一行，**这个函数不用改**（开放-封闭）。表与 catalog 的一致性
    /// 由 `every_catalog_command_is_implemented` 守着。
    pub fn run(&self, id: &str, params: &Value) -> Result<Value, String> {
        // **权限先于网络**：被拒的命令不该产生任何请求。
        let spec = self
            .policy
            .authorize(id, params)
            .map_err(|e| e.to_string())?;
        // 这一次调用的所有请求都带上命令 id，服务端才能记对审计
        let http = self.http.with_command(spec.id);
        let handler = HANDLERS
            .iter()
            .find(|(cmd, _)| *cmd == spec.id)
            .map(|(_, handler)| *handler)
            .ok_or_else(|| format!("{UNIMPLEMENTED}: {}", spec.id))?;
        handler(self, &http, params)
    }

    /// 会话清单：群 + 直聊（由好友列表推导），并附未读。
    fn conv_list(&self, http: &Http) -> Result<Value, String> {
        let me = http.expect_json("GET", "/api/me", None)?;
        let me_id = me["user_id"].as_str().unwrap_or_default().to_string();

        let mut out: Vec<Value> = Vec::new();
        if let Ok(groups) = http.expect_json("GET", "/api/groups/mine", None) {
            for g in groups.as_array().cloned().unwrap_or_default() {
                let id = g["id"].as_str().unwrap_or_default().to_string();
                out.push(json!({
                    "conv": id,
                    "kind": "group",
                    "title": g["name"],
                    "members": g["member_count"],
                }));
            }
        }
        if let Ok(friends) = http.expect_json("GET", "/api/friends", None) {
            for f in friends.as_array().cloned().unwrap_or_default() {
                let uid = f["friend_id"].as_str().unwrap_or_default().to_string();
                if uid.is_empty() {
                    continue;
                }
                out.push(json!({
                    "conv": crate::direct_conv_id(&me_id, &uid),
                    "kind": "direct",
                    "title": uid,
                }));
            }
        }

        // 未读：逐个问信箱；被白名单挡掉的会话直接不出现在结果里。
        for item in out.iter_mut() {
            let conv = item["conv"].as_str().unwrap_or_default().to_string();
            if !self.policy.conv_allowed(&conv) {
                item["unread"] = json!(null);
                item["note"] = json!("不在白名单，未读数不查");
                continue;
            }
            match http.conv_unread(&conv) {
                Ok(u) => item["unread"] = u["unread"].clone(),
                Err(e) => item["unread_error"] = json!(e),
            }
        }
        // 白名单外的会话不返回——列会话不该泄露名单外的东西
        out.retain(|v| v["unread"].is_number() || v["unread"].is_null() && v["note"].is_null());
        Ok(Value::Array(out))
    }
}

/// 一条命令的处理函数：执行器、**已打上审计标记**的 Http、原始参数。
///
/// 注意传的是 `&Http` 而不是 `&Executor`：处理函数不该拿到整个执行器，
/// 否则它随时可以绕过审计标记直接 `self.http`。以前就有两条命令这么干了
/// （`group.kick` / `msg.send`），服务端收不到 `X-Wxlike-Command`，
/// 审计里就少了这两条。
// T-Z1/G00-02 纯移动拆分：type Handler + HANDLERS 表与 str_param/urlencode
// 辅助移至子模块 execute/handlers.rs（内容逐字节一致，路径语义不变）。
mod handlers;

use handlers::HANDLERS;

#[cfg(test)]
mod tests {
    use super::*;
    use handlers::urlencode;

    /// **`group.members` 把翻页参数透传给服务端**（#331 第 3 条：游标下推
    /// 到 SQL 之后，执行器不再整份拉回来本地切；切片语义的用例在
    /// `crates/platform/tests/groups.rs` 的服务端真面里）。
    /// 这里只钉两件事：请求行带对了参数、服务端给什么就回什么。
    #[test]
    fn group_members_forwards_the_cursor_to_the_server() {
        let (base, rx) = recorder();
        let exec = Executor::new(Http::new(base), Policy::new(["*"], true, true));
        let page = exec
            .run(
                "group.members",
                &json!({"conv": "g-1", "after_user": "u1", "limit": 1}),
            )
            .expect("翻页");
        let sent = next_request(&rx);
        assert!(
            sent.starts_with("GET /api/groups/g-1/members?after_user=u1&limit=1 "),
            "{sent}"
        );
        assert_eq!(page, json!({}), "服务端给什么就回什么，不二次加工：{page}");
        // 两个参数都不给 = 裸路径（整份名单的老调用方一个字没变）
        exec.run("group.members", &json!({"conv": "g-1"}))
            .expect("全量");
        let sent = next_request(&rx);
        assert!(sent.starts_with("GET /api/groups/g-1/members "), "{sent}");
        // 只要 limit 也要带上
        exec.run("group.members", &json!({"conv": "g-1", "limit": 2}))
            .expect("首页");
        let sent = next_request(&rx);
        assert!(
            sent.starts_with("GET /api/groups/g-1/members?limit=2 "),
            "{sent}"
        );
    }

    /// **`msg.send` 把 `reply_to` 拼成服务端的 `quote_of`**（#331 第 1 条回接）。
    /// 命令面叫 `reply_to`、HTTP 面叫 `quote_of`，这条用例钉住翻译不丢、
    /// 不带引用时 body 里根本不出现这个键（与回接前逐字一致）。
    #[test]
    fn msg_send_forwards_reply_to_as_quote_of() {
        let (base, rx) = recorder();
        let exec = Executor::new(Http::new(base), Policy::new(["*"], true, true));
        exec.run(
            "msg.send",
            &json!({"conv": "g-1", "text": "hi", "reply_to": "0123abcd", "idempotency_key": "k-1"}),
        )
        .expect("带引用发送");
        let sent = next_request(&rx);
        let (head, body) = sent.split_once('\n').expect("请求带 body");
        assert!(head.starts_with("POST /api/conv/g-1/send "), "{head}");
        let body: Value = serde_json::from_str(body).expect("body 是 JSON");
        assert_eq!(body["quote_of"], json!("0123abcd"), "{body}");
        assert_eq!(body["text"], json!("hi"), "{body}");

        exec.run(
            "msg.send",
            &json!({"conv": "g-1", "text": "plain", "idempotency_key": "k-2"}),
        )
        .expect("普通发送");
        let sent = next_request(&rx);
        let body: Value = serde_json::from_str(sent.split_once('\n').expect("请求带 body").1)
            .expect("body 是 JSON");
        assert!(
            body.get("quote_of").is_none(),
            "不带引用时不许凭空造 quote_of：{body}"
        );
    }

    #[test]
    fn urlencode_handles_query_metacharacters() {
        assert_eq!(urlencode("a b&c#d+e?f%g"), "a%20b%26c%23d%2Be%3Ff%25g");
    }

    /// 被权限拒绝时**不该发任何请求**：地址必然连不上，
    /// 如果错误是"连接失败"就说明权限没拦住。
    #[test]
    fn policy_denies_before_any_network_call() {
        let exec = Executor::new(Http::new("http://127.0.0.1:1"), Policy::none());
        let err = exec
            .run("conv.unread", &json!({"conv": "d-a~b"}))
            .unwrap_err();
        assert!(err.contains("not allowed"), "{err}");
        assert!(!err.contains("connect"), "不该走到网络：{err}");
    }

    /// 建群没有幂等键 = 直接拒绝：重试一次就是两个群。
    #[test]
    fn group_create_requires_an_idempotency_key() {
        let exec = Executor::new(
            Http::new("http://127.0.0.1:1"),
            Policy::new(["*"], true, false),
        );
        let err = exec
            .run("group.create", &json!({"name": "t", "member_ids": []}))
            .unwrap_err();
        assert!(err.contains("idempotency_key"), "{err}");
    }

    /// **防漂移**：catalog 里声明有、执行器里没做，是对 agent 撒谎。
    /// 这条测试遍历全部命令——加命令忘了实现，CI 立刻红。
    #[test]
    fn every_catalog_command_is_implemented() {
        // 用一个"立刻回 400"的本地服务器：命令发不发得出去不重要，
        // 重要的是它**走到了网络**（而不是命中"尚未实现"兜底）。
        // 直连 127.0.0.1:1 会因重试把这条测试拖到 30s+。
        // 换行用 (13,10) 拼，避免源码里的转义歧义。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            let crlf = String::from_utf8(vec![13, 10]).expect("crlf");
            let resp = format!(
                "HTTP/1.1 400 Bad Request{crlf}Content-Type: application/json{crlf}                 Content-Length: 17{crlf}Connection: close{crlf}{crlf}{{\"error\":\"nope\"}}"
            );
            for sock in listener.incoming().flatten() {
                let mut sock = sock;
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf);
                let _ = sock.write_all(resp.as_bytes());
            }
        });
        let exec = Executor::new(
            Http::new(format!("http://{addr}")),
            Policy::new(["*"], true, true),
        );
        let params = json!({
            "conv": "d-a~b", "to_conv": "d-a~b", "from_conv": "d-a~b",
            "community": "c0ffee01",
            "name": "t", "text": "x", "msg_id": "00000000000000000000000000000000",
            "idempotency_key": "k", "user_ids": ["u"], "user_id": "u",
            "to_user_id": "u2", "req_id": "a1b2c3d4e5f6",
            "q": "x", "from_seq": 1, "version": 0,
        });
        for spec in wxlike_cmd::CATALOG {
            match exec.run(spec.id, &params) {
                Ok(_) => {}
                Err(e) => assert!(
                    !e.contains(UNIMPLEMENTED),
                    "{} 在 catalog 里但执行器没实现",
                    spec.id
                ),
            }
        }
    }

    #[test]
    fn unknown_command_is_rejected_by_catalog() {
        let exec = Executor::new(Http::new("http://127.0.0.1:1"), Policy::none());
        let err = exec.run("msg.send_for_ai", &json!({})).unwrap_err();
        assert!(err.contains("unknown command"), "{err}");
    }

    // ─────────── #316 relations 第一批：执行器侧的真面用例 ───────────
    //
    // `crates/cmd/tests/relations_surface.rs` 对的是"命令面 ↔ 服务端路由表"这份
    // 源码事实；这里补上**真的走到网络**的那一段：起一个把请求行与请求体交回来的
    // 一次性服务器（手法同 `crates/client/tests/zone_c_surface.rs`），
    // 于是"路径少一段""body 多一个键""权限没拦住就发出去了"都会在这里红。

    /// 应答 200 `{}`，并把每个请求的 `请求行 + \n + body` 送进 channel。
    fn recorder() -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                let mut stream = stream;
                let sep = [13u8, 10, 13, 10];
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            // 头收齐**并且** body 收齐才回包：只对头就关连接会让
                            // 还在发 body 的客户端拿到 RST（POST 带 body 的命令必踩）。
                            if let Some(pos) = buf.windows(4).position(|w| w == sep) {
                                let head_end = pos + 4;
                                let text = String::from_utf8_lossy(&buf).to_string();
                                let need = text
                                    .lines()
                                    .filter_map(|l| l.split_once(':'))
                                    .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                                    .unwrap_or(0);
                                if buf.len() >= head_end + need {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let head = text.lines().next().unwrap_or_default().to_string();
                let body = text
                    .split("\r\n\r\n")
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let _ = tx.send(if body.is_empty() {
                    head
                } else {
                    format!("{head}\n{body}")
                });
                let crlf = String::from_utf8(vec![13, 10]).expect("crlf");
                let resp = format!(
                    "HTTP/1.1 200 OK{crlf}Content-Type: application/json{crlf}Content-Length: 2{crlf}Connection: close{crlf}{crlf}{{}}"
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (format!("http://{addr}"), rx)
    }

    fn next_request(rx: &std::sync::mpsc::Receiver<String>) -> String {
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("服务器该收到一条请求")
    }

    fn assert_no_request(rx: &std::sync::mpsc::Receiver<String>) {
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "被拒的命令不该产生任何请求"
        );
    }

    fn authed(base: String) -> Http {
        let mut http = Http::new(base);
        http.token = Some("t".to_string());
        http
    }

    /// 三条读命令打到**各自**的端点，不带查询串。
    #[test]
    fn friend_reads_reach_their_own_routes() {
        let (base, rx) = recorder();
        let exec = Executor::new(authed(base), Policy::new(["*"], false, false));
        for (id, want) in [
            ("friend.list", "GET /api/friends HTTP/1.1"),
            (
                "friend.requests.incoming",
                "GET /api/friends/requests/incoming HTTP/1.1",
            ),
            (
                "friend.requests.outgoing",
                "GET /api/friends/requests/outgoing HTTP/1.1",
            ),
        ] {
            exec.run(id, &json!({}))
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            assert_eq!(next_request(&rx), want, "{id}");
        }
    }

    /// 发起申请：`POST /api/friends/requests`，body 只带声明过的两个键，
    /// 而且不许带 `Idempotency-Key` 头（标注是 `None`，服务端也不消费）。
    #[test]
    fn friend_request_create_posts_the_declared_body_only() {
        let (base, rx) = recorder();
        let exec = Executor::new(authed(base), Policy::new(["*"], true, false));
        exec.run(
            "friend.request.create",
            &json!({"to_user_id": "u-2", "message": "你好，我是小王"}),
        )
        .expect("发起申请");
        let got = next_request(&rx);
        let (head, body) = got.split_once('\n').expect("create 该带 body");
        assert_eq!(head, "POST /api/friends/requests HTTP/1.1", "{got}");
        let sent: Value = serde_json::from_str(body).expect("body 是 json");
        assert_eq!(
            sent,
            json!({"to_user_id": "u-2", "message": "你好，我是小王"}),
            "{sent}"
        );
        assert!(
            !got.to_lowercase().contains("idempotency-key"),
            "没标 Idempotency::Required 却发了幂等头：{got}"
        );

        // 不给附言 = body 里**没有** message 键（缺席与空串在文案上不是一个意思）
        exec.run("friend.request.create", &json!({"to_user_id": "u-3"}))
            .expect("只给对象");
        let got = next_request(&rx);
        let (_, body) = got.split_once('\n').expect("create 该带 body");
        let sent: Value = serde_json::from_str(body).expect("body 是 json");
        assert_eq!(sent, json!({"to_user_id": "u-3"}), "{sent}");
    }

    /// #317 备注与删除：备注是 `PUT …/remark` 带 `{remark}`；
    /// 删除与解除拉黑是 DELETE 无 body。删除要走破坏性许可
    /// （Policy 第三个参数），否则在策略层就被拦下、碰不到网络。
    #[test]
    fn friend_remark_and_remove_reach_their_routes() {
        let (base, rx) = recorder();
        let exec = Executor::new(authed(base), Policy::new(["*"], true, true));
        exec.run(
            "friend.remark.set",
            &json!({"user_id": "u2", "remark": "同事小王"}),
        )
        .expect("设备注");
        let got = next_request(&rx);
        let (head, body) = got.split_once('\n').expect("remark 该带 body");
        assert_eq!(head, "PUT /api/friends/u2/remark HTTP/1.1", "{got}");
        assert_eq!(
            serde_json::from_str::<Value>(body).expect("json"),
            json!({"remark": "同事小王"})
        );

        // 删除好友要确认：策略层没有确认流程时 Executor 直接执行的是
        // 已确认的调用（令牌在确认层消费），这里验的是分派形状。
        let out = exec
            .run("friend.remove", &json!({"user_id": "u2"}))
            .expect("删好友");
        assert_eq!(out, json!({"removed": "u2"}));
        assert_eq!(next_request(&rx), "DELETE /api/friends/u2 HTTP/1.1");
    }

    /// #317 黑名单与隐私：拉黑 PUT、解除 DELETE、列表 GET，
    /// 隐私读写打 `/api/settings/strangers`，写的键是服务端的 `allow`。
    #[test]
    fn friend_block_and_privacy_reach_their_routes() {
        let (base, rx) = recorder();
        let exec = Executor::new(authed(base), Policy::new(["*"], true, true));
        let out = exec
            .run("friend.block.add", &json!({"user_id": "u9"}))
            .expect("拉黑");
        assert_eq!(out, json!({"blocked": "u9"}));
        assert_eq!(next_request(&rx), "PUT /api/blocks/u9 HTTP/1.1");

        let out = exec
            .run("friend.block.remove", &json!({"user_id": "u9"}))
            .expect("解除拉黑");
        assert_eq!(out, json!({"unblocked": "u9"}));
        assert_eq!(next_request(&rx), "DELETE /api/blocks/u9 HTTP/1.1");

        exec.run("friend.block.list", &json!({}))
            .expect("黑名单列表");
        assert_eq!(next_request(&rx), "GET /api/blocks HTTP/1.1");

        exec.run("friend.privacy.get", &json!({}))
            .expect("隐私查询");
        assert_eq!(next_request(&rx), "GET /api/settings/strangers HTTP/1.1");

        let out = exec
            .run("friend.privacy.set", &json!({"allow": false}))
            .expect("隐私设置");
        assert_eq!(out, json!({"allow_strangers": false}));
        let got = next_request(&rx);
        let (head, body) = got.split_once('\n').expect("privacy.set 该带 body");
        assert_eq!(head, "PUT /api/settings/strangers HTTP/1.1", "{got}");
        assert_eq!(
            serde_json::from_str::<Value>(body).expect("json"),
            json!({"allow": false}),
            "发给服务端的键必须是 allow，不是 allow_strangers"
        );
    }

    /// #317 的路径参数同样过 `path_id`：带斜杠的 id 不许拼进路径。
    #[test]
    fn friend_batch2_rejects_path_shaped_ids_before_the_network() {
        let (base, rx) = recorder();
        let exec = Executor::new(authed(base), Policy::new(["*"], true, true));
        for (id, params) in [
            ("friend.remove", json!({"user_id": "a/b"})),
            ("friend.block.add", json!({"user_id": "..%2f"})),
            (
                "friend.remark.set",
                json!({"user_id": "a b", "remark": "x"}),
            ),
        ] {
            assert!(exec.run(id, &params).is_err(), "{id} 该拒绝路径形 id");
        }
        assert!(rx.try_recv().is_err(), "拒绝发生在本地，不许有请求发出去");
    }

    /// 接受与拒绝：申请 id 在**路径**里，方法 POST，无 body。
    #[test]
    fn friend_request_decision_posts_to_the_request_scoped_route() {
        let (base, rx) = recorder();
        let exec = Executor::new(authed(base), Policy::new(["*"], true, false));
        for (verb, id) in [("accept", "a1b2c3"), ("decline", "d4e5f6")] {
            exec.run(&format!("friend.request.{verb}"), &json!({"req_id": id}))
                .unwrap_or_else(|e| panic!("{verb}: {e}"));
            assert_eq!(
                next_request(&rx),
                format!("POST /api/friends/requests/{id}/{verb} HTTP/1.1")
            );
        }
    }

    /// **参数不传就报错，而且报错的命令不许碰网络**。
    #[test]
    fn friend_commands_fail_locally_when_a_required_argument_is_missing() {
        let (base, rx) = recorder();
        let exec = Executor::new(authed(base), Policy::new(["*"], true, false));
        let err = exec
            .run("friend.request.create", &json!({"message": "hi"}))
            .unwrap_err();
        assert!(err.contains("to_user_id"), "{err}");
        assert_no_request(&rx);

        let err = exec.run("friend.request.accept", &json!({})).unwrap_err();
        assert!(err.contains("req_id"), "{err}");
        assert_no_request(&rx);
    }

    /// `req_id` 要拼进 URL 路径：带 `/ ? # %` 与空白的值会把这次调用**改写成
    /// 另一条路由**，那时审计里的命令 id 与实际端点就对不上了。执行器侧先拒
    /// （可见错误，不是 panic）；合法 id 照旧走得通，不许把命令本身堵死。
    #[test]
    fn a_path_shaped_request_id_is_refused_before_the_wire() {
        let (base, rx) = recorder();
        let exec = Executor::new(authed(base), Policy::new(["*"], true, false));
        for bad in [
            "a/b",
            "../../groups/g-1/members/u",
            "x?to=everyone",
            "x#frag",
            "with space",
            "",
        ] {
            let err = exec
                .run("friend.request.accept", &json!({"req_id": bad}))
                .unwrap_err();
            assert!(err.contains("req_id"), "{bad} → {err}");
        }
        assert_no_request(&rx);
        exec.run("friend.request.accept", &json!({"req_id": "a1b2c3d4e5f6"}))
            .expect("合法 id");
        assert_eq!(
            next_request(&rx),
            "POST /api/friends/requests/a1b2c3d4e5f6/accept HTTP/1.1"
        );
    }

    /// **权限先于网络**：只读策略下三条写命令一条请求都不该发出去；
    /// 三条读命令不受写权限影响（它们是 `Account` 域，与会话白名单无关）。
    #[test]
    fn read_only_policy_denies_friend_writes_but_not_friend_reads() {
        let (base, rx) = recorder();
        let exec = Executor::new(authed(base), Policy::new(["*"], false, false));
        let err = exec
            .run("friend.request.create", &json!({"to_user_id": "u-2"}))
            .unwrap_err();
        assert!(
            err.contains("write access") || err.contains("read-only"),
            "{err}"
        );
        for id in ["friend.request.accept", "friend.request.decline"] {
            let err = exec.run(id, &json!({"req_id": "a1b2"})).unwrap_err();
            assert!(err.contains("write access"), "{id}: {err}");
        }
        assert_no_request(&rx);

        for id in [
            "friend.list",
            "friend.requests.incoming",
            "friend.requests.outgoing",
        ] {
            exec.run(id, &json!({}))
                .unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(next_request(&rx).starts_with("GET /api/friends"), "{id}");
        }
    }

    /// 请求头里必须带上命令 id：服务端审计靠它把请求归到命令上。
    #[test]
    fn friend_commands_carry_the_audit_header() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                let mut stream = stream;
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
                let crlf = String::from_utf8(vec![13, 10]).expect("crlf");
                let resp = format!(
                    "HTTP/1.1 200 OK{crlf}Content-Type: application/json{crlf}Content-Length: 2{crlf}Connection: close{crlf}{crlf}{{}}"
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let exec = Executor::new(
            authed(format!("http://{addr}")),
            Policy::new(["*"], true, false),
        );
        exec.run("friend.list", &json!({})).expect("读好友列表");
        let raw = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("该收到请求");
        assert!(
            raw.to_lowercase().contains("x-wxlike-command: friend.list"),
            "审计头没带上命令 id：{raw}"
        );
    }
}
