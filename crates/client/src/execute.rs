//! 命令执行器：catalog → HTTP。
//!
//! 这一层只做三件事：查权限、按 id 分派、把参数拼进请求。它不知道调用方
//! 是命令行还是 MCP，也不知道 UI 存在——那正是"一套命令、多个入口"的前提。

use serde_json::{json, Value};

use wxlike_cmd::policy::Policy;

use crate::Http;

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
    pub fn run(&self, id: &str, params: &Value) -> Result<Value, String> {
        // **权限先于网络**：被拒的命令不该产生任何请求。
        let spec = self
            .policy
            .authorize(id, params)
            .map_err(|e| e.to_string())?;
        // 这一次调用的所有请求都带上命令 id，服务端才能记对审计
        let http = self.http.with_command(spec.id);
        match spec.id {
            "me.info" => http.expect_json("GET", "/api/me", None),
            "conv.unread" => http.conv_unread(str_param(params, "conv")?),
            "conv.sync" => http.conv_sync(
                str_param(params, "conv")?,
                params["version"].as_u64().unwrap_or(0),
            ),
            "conv.history" => {
                let conv = str_param(params, "conv")?;
                let before = params["before_seq"].as_i64().unwrap_or(0);
                let limit = params["limit"].as_u64().unwrap_or(50);
                http.expect_json(
                    "GET",
                    &format!("/api/history/{conv}?before_seq={before}&limit={limit}"),
                    None,
                )
            }
            "msg.search" => {
                let q = str_param(params, "q")?;
                let limit = params["limit"].as_u64().unwrap_or(20);
                http.expect_json(
                    "GET",
                    &format!("/api/search/messages?q={}&limit={limit}", urlencode(q)),
                    None,
                )
            }
            "group.info" => {
                let conv = str_param(params, "conv")?;
                self.http
                    .expect_json("GET", &format!("/api/groups/{conv}"), None)
            }
            "group.members" => {
                let conv = str_param(params, "conv")?;
                self.http
                    .expect_json("GET", &format!("/api/groups/{conv}/members"), None)
            }
            "conv.list" => self.conv_list(&http),
            // ---- 写 ----
            "conv.ack" => {
                let conv = str_param(params, "conv")?;
                match params["seq"].as_u64() {
                    Some(seq) => http.mark_conv_read_upto(conv, seq),
                    None => http.mark_conv_read(conv),
                }
            }
            "group.create" => {
                let name = str_param(params, "name")?;
                let key = str_param(params, "idempotency_key")?;
                let members = params
                    .get("member_ids")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([]));
                // 幂等键**必填**：建群重试两次就是两个群，没有键就没有安全网
                http.post_idempotent(
                    "/api/groups",
                    &serde_json::json!({ "name": name, "member_ids": members }),
                    key,
                )
            }
            "group.add_member" => {
                let conv = str_param(params, "conv")?;
                let ids = params
                    .get("user_ids")
                    .cloned()
                    .ok_or_else(|| "缺少参数 user_ids".to_string())?;
                http.expect_json(
                    "POST",
                    &format!("/api/groups/{conv}/members"),
                    Some(&serde_json::json!({ "user_ids": ids })),
                )
            }
            "group.kick" => {
                let conv = str_param(params, "conv")?;
                let uid = str_param(params, "user_id")?;
                self.http
                    .expect_empty("DELETE", &format!("/api/groups/{conv}/members/{uid}"), None)
                    .map(|()| serde_json::json!({ "removed": uid }))
            }
            "msg.send" => {
                let conv = str_param(params, "conv")?;
                let text = str_param(params, "text")?;
                let key = str_param(params, "idempotency_key")?;
                // 服务端把 key 派生成 msg_id：重试落在同一条消息上
                self.http.post_idempotent(
                    &format!("/api/conv/{conv}/send"),
                    &serde_json::json!({ "text": text, "idempotency_key": key }),
                    key,
                )
            }
            // 仍需数据面能力的命令：明确拒绝，不假装成功
            other @ ("msg.forward" | "msg.recall") => Err(format!(
                "{other} 走 QUIC 数据面，HTTP 控制面暂不支持；请用客户端"
            )),
            other => Err(format!("尚未实现: {other}")),
        }
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

    /// 走数据面的命令要明确拒绝，而不是发一个假请求。
    #[test]
    fn data_plane_commands_are_refused_explicitly() {
        let exec = Executor::new(
            Http::new("http://127.0.0.1:1"),
            Policy::new(["*"], true, false),
        );
        let err = exec
            .run(
                "msg.forward",
                &json!({"from_conv": "g-1", "to_conv": "g-2", "msg_id": "m", "idempotency_key": "k"}),
            )
            .unwrap_err();
        assert!(err.contains("QUIC"), "{err}");
    }

    #[test]
    fn unknown_command_is_rejected_by_catalog() {
        let exec = Executor::new(Http::new("http://127.0.0.1:1"), Policy::none());
        let err = exec.run("msg.send_for_ai", &json!({})).unwrap_err();
        assert!(err.contains("unknown command"), "{err}");
    }
}
