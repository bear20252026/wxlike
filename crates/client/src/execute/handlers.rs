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

// ── 参数与编码辅助（handler 专用；测试经 execute.rs 再导出引用）──
pub(super) fn str_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("缺少参数 {key}"))
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
