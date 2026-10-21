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
    /// 分派走 [`HANDLERS`] 表而不是 `match`：加一条命令 = catalog 加一条 +
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
type Handler = fn(&Executor, &Http, &Value) -> Result<Value, String>;

/// 命令 id → 处理函数。与 `wxlike_cmd::CATALOG` 一一对应。
///
/// **加命令只加一行**，`run()` 不动。顺序与 catalog 的"人怎么用"一致：
/// 先看（会话/消息/成员/自己/视频），再写，最后危险操作。
const HANDLERS: &[(&str, Handler)] = &[
    // ---- 读：会话 ----
    ("conv.list", |ex, http, _p| ex.conv_list(http)),
    ("conv.unread", |_ex, http, p| {
        http.conv_unread(str_param(p, "conv")?)
    }),
    ("conv.history", |_ex, http, p| {
        let conv = str_param(p, "conv")?;
        let before = p["before_seq"].as_i64().unwrap_or(0);
        let limit = p["limit"]
            .as_u64()
            .unwrap_or(wxlike_limits::DEFAULT_PAGE_LIMIT);
        http.expect_json(
            "GET",
            &format!("/api/history/{conv}?before_seq={before}&limit={limit}"),
            None,
        )
    }),
    ("conv.sync", |_ex, http, p| {
        http.conv_sync(str_param(p, "conv")?, p["version"].as_u64().unwrap_or(0))
    }),
    // ---- 读：消息 ----
    ("msg.list", |_ex, http, p| {
        // history 是 before_seq 语义；这里按区间取：先翻到 to_seq 之前，
        // 再按 from_seq 过滤。命令语义是"给我这一段"，不该让调用方懂翻页。
        let conv = str_param(p, "conv")?;
        let from = p["from_seq"].as_u64().unwrap_or(1);
        let to = p["to_seq"].as_u64().unwrap_or(u64::MAX);
        let limit = (to.saturating_sub(from) + 1).min(200);
        let before = if to == u64::MAX { 0 } else { to + 1 };
        let page = http.expect_json(
            "GET",
            &format!("/api/history/{conv}?before_seq={before}&limit={limit}"),
            None,
        )?;
        let rows: Vec<Value> = page["messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| {
                let seq = m["seq"].as_u64().unwrap_or(0);
                seq >= from && seq <= to
            })
            .collect();
        Ok(json!({ "conv": conv, "count": rows.len(), "messages": rows }))
    }),
    ("msg.search", |_ex, http, p| {
        let q = str_param(p, "q")?;
        let limit = p["limit"]
            .as_u64()
            .unwrap_or(wxlike_limits::DEFAULT_SEARCH_LIMIT);
        http.expect_json(
            "GET",
            &format!("/api/search/messages?q={}&limit={limit}", urlencode(q)),
            None,
        )
    }),
    ("msg.get", |_ex, http, p| {
        let conv = str_param(p, "conv")?;
        let msg_id = str_param(p, "msg_id")?;
        http.expect_json("GET", &format!("/api/conv/{conv}/msg/{msg_id}"), None)
    }),
    // ---- 读：成员与自查 ----
    ("group.members", |_ex, http, p| {
        let conv = str_param(p, "conv")?;
        http.expect_json("GET", &format!("/api/groups/{conv}/members"), None)
    }),
    ("group.info", |_ex, http, p| {
        let conv = str_param(p, "conv")?;
        http.expect_json("GET", &format!("/api/groups/{conv}"), None)
    }),
    ("community.threads", |_ex, http, p| {
        let community = str_param(p, "community")?;
        let limit = p["limit"]
            .as_u64()
            .unwrap_or(wxlike_limits::DEFAULT_PAGE_LIMIT);
        http.expect_json(
            "GET",
            &format!("/api/community/{community}/threads?limit={limit}"),
            None,
        )
    }),
    ("channel.roles", |_ex, http, p| {
        // 路径里是频道 id：`Policy` 的会话白名单按 conv 参数管住它
        let conv = str_param(p, "conv")?;
        http.expect_json("GET", &format!("/api/channel/{conv}/roles"), None)
    }),
    ("me.info", |_ex, http, _p| {
        http.expect_json("GET", "/api/me", None)
    }),
    ("me.sessions", |_ex, http, _p| {
        http.expect_json("GET", "/api/sessions", None)
    }),
    // ---- 读：视频 / 点播 ----
    ("video.select", |_ex, http, p| {
        let id = str_param(p, "video_id")?;
        let kbps = p["kbps"]
            .as_u64()
            .ok_or_else(|| "缺少参数 kbps（客户端上一段的实测可用带宽，千比特/秒）".to_string())?;
        http.expect_json("GET", &format!("/api/video/{id}/select?kbps={kbps}"), None)
    }),
    ("video.stats", |_ex, http, p| {
        let id = str_param(p, "video_id")?;
        http.expect_json("GET", &format!("/api/video/{id}/stats"), None)
    }),
    ("video.job.get", |_ex, http, p| {
        let id = str_param(p, "job_id")?;
        http.expect_json("GET", &format!("/api/video/jobs/{id}"), None)
    }),
    // ---- 写 ----
    ("msg.send", |_ex, http, p| {
        let conv = str_param(p, "conv")?;
        let text = str_param(p, "text")?;
        let key = str_param(p, "idempotency_key")?;
        // 服务端把 key 派生成 msg_id：重试落在同一条消息上
        http.post_idempotent(
            &format!("/api/conv/{conv}/send"),
            &json!({ "text": text, "idempotency_key": key }),
            key,
        )
    }),
    ("conv.ack", |_ex, http, p| {
        let conv = str_param(p, "conv")?;
        match p["seq"].as_u64() {
            Some(seq) => http.mark_conv_read_upto(conv, seq),
            None => http.mark_conv_read(conv),
        }
    }),
    ("msg.forward", |_ex, http, p| {
        let from = str_param(p, "from_conv")?;
        let to = str_param(p, "to_conv")?;
        let msg_id = str_param(p, "msg_id")?;
        let key = str_param(p, "idempotency_key")?;
        // 路径里是**目标**会话；来源会话在 body，两边都由服务端鉴权
        http.post_idempotent(
            &format!("/api/conv/{to}/forward"),
            &json!({
                "from_conv": from, "to_conv": to,
                "msg_id": msg_id, "idempotency_key": key,
            }),
            key,
        )
    }),
    ("group.create", |_ex, http, p| {
        let name = str_param(p, "name")?;
        let key = str_param(p, "idempotency_key")?;
        let members = p.get("member_ids").cloned().unwrap_or_else(|| json!([]));
        // 幂等键**必填**：建群重试两次就是两个群，没有键就没有安全网
        http.post_idempotent(
            "/api/groups",
            &json!({ "name": name, "member_ids": members }),
            key,
        )
    }),
    ("group.add_member", |_ex, http, p| {
        let conv = str_param(p, "conv")?;
        let ids = p
            .get("user_ids")
            .cloned()
            .ok_or_else(|| "缺少参数 user_ids".to_string())?;
        http.expect_json(
            "POST",
            &format!("/api/groups/{conv}/members"),
            Some(&json!({ "user_ids": ids })),
        )
    }),
    ("video.job.create", |_ex, http, p| {
        // 读本地文件是 agent 侧唯一碰磁盘的地方：路径由调用方给，
        // 但能力等级是 Write——没开写权限时根本走不到这里。
        let path = str_param(p, "path")?;
        let bytes = std::fs::read(path).map_err(|e| format!("读不到本地文件 {path}: {e}"))?;
        let callback = p.get("callback_url").and_then(|v| v.as_str());
        http.post_video_job(&bytes, callback)
    }),
    ("video.job.retry", |_ex, http, p| {
        let id = str_param(p, "job_id")?;
        http.expect_json("POST", &format!("/api/video/jobs/{id}/retry"), None)
    }),
    ("video.view.report", |_ex, http, p| {
        let id = str_param(p, "video_id")?;
        let viewer = str_param(p, "viewer")?;
        let watched = p["watched_ms"]
            .as_u64()
            .ok_or_else(|| "缺少参数 watched_ms（本次**新增**时长，必须为正）".to_string())?;
        http.expect_json(
            "POST",
            &format!("/api/video/{id}/view"),
            Some(&json!({
                "viewer": viewer,
                "watched_ms": watched,
                "position_ms": p["position_ms"].as_u64().unwrap_or(0),
                "completed": p["completed"].as_bool().unwrap_or(false),
                "session_id": p.get("session_id").and_then(|v| v.as_str()).unwrap_or_default(),
            })),
        )
    }),
    // ---- 破坏性 ----
    ("group.kick", |_ex, http, p| {
        let conv = str_param(p, "conv")?;
        let uid = str_param(p, "user_id")?;
        http.expect_empty("DELETE", &format!("/api/groups/{conv}/members/{uid}"), None)
            .map(|()| json!({ "removed": uid }))
    }),
    ("msg.recall", |_ex, http, p| {
        let conv = str_param(p, "conv")?;
        let msg_id = str_param(p, "msg_id")?;
        http.expect_json(
            "POST",
            &format!("/api/conv/{conv}/recall"),
            Some(&json!({ "msg_id": msg_id })),
        )
    }),
];

fn str_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("缺少参数 {key}"))
}

/// 最小 URL 编码：只要把空格、`&`、`#`、`+`、`?`、`%` 处理掉。
/// 不做完整 percent-encoding——搜索词来自命令行，够用即可。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            ' ' => out.push_str("%20"),
            '&' => out.push_str("%26"),
            '#' => out.push_str("%23"),
            '+' => out.push_str("%2B"),
            '?' => out.push_str("%3F"),
            '%' => out.push_str("%25"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
