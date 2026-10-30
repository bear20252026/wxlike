//! Executor 的命令分派表（G00-02 自 execute.rs 纯移动：
//! 一条命令一个条目，id → handler；执行语义零改动）。

use serde_json::{json, Value};

use super::{Executor, Http};

type Handler = fn(&Executor, &Http, &Value) -> Result<Value, String>;

pub(super) const HANDLERS: &[(&str, Handler)] = &[
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
        // #331 第 3 条：游标已下推到服务端 SQL
        // （`crates/platform/src/groups.rs` 的 `group_members_page`），
        // 执行器只透传 `after_user` / `limit`，不再整份拉回来本地切。
        // 服务端语义与旧切片逐条一致（死游标 = 空页、`limit=0` = 空页、
        // 缺省 = 从游标之后全给），所以 catalog 的参数声明一个字不用改。
        let after_user = p
            .get("after_user")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut url = format!("/api/groups/{conv}/members");
        let mut sep = '?';
        if !after_user.is_empty() {
            url.push_str(&format!("{sep}after_user={}", urlencode(after_user)));
            sep = '&';
        }
        if let Some(limit) = p.get("limit").and_then(Value::as_u64) {
            url.push_str(&format!("{sep}limit={limit}"));
        }
        http.expect_json("GET", &url, None)
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
    // ---- 读：好友与申请（#316）----
    // 三条读命令都不带任何参数：服务端按 `authed.user_id` 取数
    // （`crates/platform/src/friends.rs:299-318` 的 SQL 是 `WHERE user_id = ?1`），
    // 命令面没有"看谁的名单"这个入口，所以调用方拿不到别人的通讯录。
    ("friend.list", |_ex, http, _p| {
        http.expect_json("GET", "/api/friends", None)
    }),
    ("friend.requests.incoming", |_ex, http, _p| {
        http.expect_json("GET", "/api/friends/requests/incoming", None)
    }),
    ("friend.requests.outgoing", |_ex, http, _p| {
        http.expect_json("GET", "/api/friends/requests/outgoing", None)
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
    // ---- 写：好友申请（#316）----
    ("friend.request.create", |_ex, http, p| {
        let to = str_param(p, "to_user_id")?;
        let mut body = json!({ "to_user_id": to });
        // 附言不给就**不发这个键**：服务端 `RequestBody.message` 是
        // `Option<String>` + `#[serde(default)]`（`crates/platform/src/friends.rs:38-43`），
        // 缺席与空串在落库与系统消息文案上不是一个意思（`:429-433`）。
        if let Some(msg) = p.get("message").and_then(Value::as_str) {
            body["message"] = json!(msg);
        }
        // 走 `expect_json` 而不是 `post_idempotent`：服务端 `create_request`
        // 不读 `Idempotency-Key` 头，键发了也没人消费。重试由唯一键去重，
        // 依据写在 catalog 的 `friend.request.create` 注释里。
        http.expect_json("POST", "/api/friends/requests", Some(&body))
    }),
    ("friend.request.accept", |_ex, http, p| {
        // 只有申请 id：我是谁由 token 决定，服务端 `req.to_uid != me` → Forbidden
        // （`crates/platform/src/friends.rs:200-202`），命令面递不出别人的身份。
        let id = path_id(str_param(p, "req_id")?, "req_id")?;
        http.expect_json("POST", &format!("/api/friends/requests/{id}/accept"), None)
    }),
    ("friend.request.decline", |_ex, http, p| {
        let id = path_id(str_param(p, "req_id")?, "req_id")?;
        http.expect_json("POST", &format!("/api/friends/requests/{id}/decline"), None)
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

// ── 参数与编码辅助（handler 专用；测试经 execute.rs 再导出引用）──
pub(super) fn str_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("缺少参数 {key}"))
}

/// 路径段允许的最大字符数——与服务端 `validate::id`（`crates/platform/src/validate.rs:21`
/// 的 `MAX_ID_CHARS = 64`）同一口径，写成常量免得两边各数一遍。
const MAX_PATH_ID_CHARS: usize = 64;

/// 要拼进 **URL 路径段** 的 id：只接受非空字母数字，否则给可见错误。
///
/// 为什么在执行器侧就判（服务端 `validate::id` 也会判）：`req_id` 这类值直接进
/// `format!("/api/friends/requests/{id}/accept")`，带 `/`、`?`、`#` 的值会把这次
/// 调用**改写成另一条路由**——那时"这条命令打到哪个端点"就不再是 catalog 说了算，
/// 审计里的 `X-Wxlike-Command` 与实际请求的端点也会各说一套。
/// 与服务端同口径还让合法 id 少一次无谓往返（400 变成本地错误）。
/// 返回的是 `&str` 而不是 panic：错误必须是调用方能看见的文本。
pub(super) fn path_id<'a>(raw: &'a str, key: &str) -> Result<&'a str, String> {
    if raw.is_empty()
        || raw.chars().count() > MAX_PATH_ID_CHARS
        || !raw.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(format!(
            "参数 {key} 不是合法 id：只许字母数字，长度 1~{MAX_PATH_ID_CHARS}"
        ));
    }
    Ok(raw)
}

/// 最小 URL 编码：只要把空格、`&`、`#`、`+`、`?`、`%` 处理掉。
/// 不做完整 percent-encoding——搜索词来自命令行，够用即可。
pub(super) fn urlencode(s: &str) -> String {
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
