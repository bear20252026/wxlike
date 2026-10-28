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
    use handlers::{members_page, urlencode};

    /// 起一个只会回 `body` 的一次性 HTTP 服务，返回 base url。
    ///
    /// 为什么要真的起一个服务：`group.members` 这类"参数只在前端切片"的命令，
    /// 光看 handler 的源码说明不了它真的动了——只有跑一次 `run()`，
    /// 看**返回的那一页**是不是期望的那几行，才算"参数接上了线"。
    fn serve_json(body: String) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            let crlf = String::from_utf8(vec![13, 10]).expect("crlf");
            let len = body.len();
            let resp = format!(
                "HTTP/1.1 200 OK{crlf}Content-Type: application/json{crlf}Content-Length: {len}{crlf}Connection: close{crlf}{crlf}{body}"
            );
            for sock in listener.incoming().flatten() {
                let mut sock = sock;
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf);
                let _ = sock.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    /// 三个人的群名单（`u1` 是群主），服务端原样返回。
    fn roster() -> Value {
        json!([
            { "user_id": "u1", "role": "owner" },
            { "user_id": "u2", "role": "member" },
            { "user_id": "u3", "role": "member" },
        ])
    }

    /// **`group.members` 的翻页参数是真接上线的**（#315：曾经是装饰性的）。
    #[test]
    fn group_members_honours_the_cursor_and_the_limit() {
        let exec = Executor::new(
            Http::new(serve_json(roster().to_string())),
            Policy::new(["*"], true, true),
        );
        // 游标之后取一条
        let page = exec
            .run(
                "group.members",
                &json!({"conv": "g-1", "after_user": "u1", "limit": 1}),
            )
            .expect("翻页");
        assert_eq!(
            page,
            json!([{ "user_id": "u2", "role": "member" }]),
            "{page}"
        );
        // 两个参数都不给 = 原样整份名单（contract-v1 的响应形状没变）
        let all = exec
            .run("group.members", &json!({"conv": "g-1"}))
            .expect("全量");
        assert_eq!(
            all,
            roster(),
            "不带游标与条数时必须和服务端给的一模一样：{all}"
        );
        // 只要 limit 就给这一页
        let head = exec
            .run("group.members", &json!({"conv": "g-1", "limit": 2}))
            .expect("首页");
        assert_eq!(head.as_array().expect("array").len(), 2, "{head}");
        // 游标落在已被移出的人身上 = 空页，**不许从头再给一遍**
        let gone = exec
            .run(
                "group.members",
                &json!({"conv": "g-1", "after_user": "u-zombie"}),
            )
            .expect("死游标");
        assert_eq!(gone, json!([]), "从头再给一遍会让翻页永远打转：{gone}");
    }

    /// 切片本身的边界（不联网，纯函数）。
    #[test]
    fn members_page_boundaries() {
        let rows: Vec<Value> = roster().as_array().expect("array").clone();
        assert_eq!(members_page(&rows, "", 0).len(), 0, "limit=0 就是空页");
        assert_eq!(members_page(&rows, "", 1).len(), 1);
        assert_eq!(
            members_page(&rows, "u3", 10).len(),
            0,
            "最后一个人之后没有下一页"
        );
        assert!(members_page(&rows, "u-nope", 10).is_empty());
        assert_eq!(members_page(&[], "", 10).len(), 0, "空名单翻页还是空");
        // 游标翻页要走完整个名单：每页一条，最后拿到 u3
        let mut cursor = String::new();
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..3 {
            let page = members_page(&rows, &cursor, 1);
            let Some(first) = page.first() else { break };
            let uid = first["user_id"].as_str().expect("user_id").to_string();
            seen.push(uid.clone());
            cursor = uid;
        }
        assert_eq!(seen, ["u1", "u2", "u3"], "{seen:?}");
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
}
