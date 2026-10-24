//! 命令 → HTTP 的路由表：**客户端唯一一份**。
//!
//! 以前这份映射在三个地方各有一份：`crates/client/src/execute.rs`（桌面/CLI/MCP）、
//! `crates/ffi/src/client.rs`（手机，自己拼 URL）、`ui/src-tauri/src/commands.rs`
//! （桌面 Tauri）。三份里任意一份改了、另两份没改，表现就是"桌面能发、手机 404"。
//!
//! 这里把它变成数据：一条 action 一行。加命令 = catalog 加一条 + 这里加一行，
//! `every_catalog_action_is_routed` 会盯着两边不漂移。
//!
//! 少数命令**故意不在这张表里**（[`COMPOSITE`]）：它们要么由多个端点拼出来
//! （会话列表 = 群 + 好友 + 未读），要么需要文件上传通道（视频任务）。
//! 把它们硬塞进"一个 action 一个 URL"反而会掩盖真实语义。

use serde_json::{json, Map, Value};
use wxlike_types::{ActionId, DomainError};

use crate::ports::{HttpMethod, TransportRequest};

/// 一条路由。
#[derive(Clone, Copy, Debug)]
pub struct Route {
    /// 命令 id。
    pub id: &'static str,
    /// HTTP 方法。
    pub method: HttpMethod,
    /// 路径模板，`{name}` 会被同名参数替换。
    pub path: &'static str,
    /// 写命令的请求体字段（从参数里挑，其余丢掉）。
    pub body: &'static [&'static str],
}

/// 需要多个端点或上传通道的命令，以及原因。
pub const COMPOSITE: &[(&str, &str)] = &[
    ("conv.list", "会话列表 = 群列表 + 好友列表 + 逐会话未读"),
    ("msg.list", "区间查询由 history 翻页 + 本地过滤拼出"),
    ("conv.ack", "带 seq 走 mark_upto、不带走 mark_all，两条路径"),
    ("video.job.create", "需要二进制上传通道，不走 JSON 路由"),
];

/// 路由表。顺序与 catalog 一致，便于人工比对。
pub const ROUTES: &[Route] = &[
    Route {
        id: "conv.unread",
        method: HttpMethod::Get,
        path: "/api/conv/{conv}/unread",
        body: &[],
    },
    Route {
        id: "conv.history",
        method: HttpMethod::Get,
        path: "/api/history/{conv}?before_seq={before_seq}&limit={limit}",
        body: &[],
    },
    Route {
        id: "conv.sync",
        method: HttpMethod::Get,
        path: "/api/conv/{conv}/sync?version={version}",
        body: &[],
    },
    Route {
        id: "msg.search",
        method: HttpMethod::Get,
        path: "/api/search/messages?q={q}&limit={limit}",
        body: &[],
    },
    Route {
        id: "msg.get",
        method: HttpMethod::Get,
        path: "/api/conv/{conv}/msg/{msg_id}",
        body: &[],
    },
    Route {
        id: "group.members",
        method: HttpMethod::Get,
        path: "/api/groups/{conv}/members",
        body: &[],
    },
    Route {
        id: "group.info",
        method: HttpMethod::Get,
        path: "/api/groups/{conv}",
        body: &[],
    },
    Route {
        id: "community.threads",
        method: HttpMethod::Get,
        path: "/api/community/{community}/threads?limit={limit}",
        body: &[],
    },
    Route {
        id: "channel.roles",
        method: HttpMethod::Get,
        path: "/api/channel/{conv}/roles",
        body: &[],
    },
    Route {
        id: "me.info",
        method: HttpMethod::Get,
        path: "/api/me",
        body: &[],
    },
    Route {
        id: "me.sessions",
        method: HttpMethod::Get,
        path: "/api/sessions",
        body: &[],
    },
    Route {
        id: "video.select",
        method: HttpMethod::Get,
        path: "/api/video/{video_id}/select?kbps={kbps}",
        body: &[],
    },
    Route {
        id: "video.stats",
        method: HttpMethod::Get,
        path: "/api/video/{video_id}/stats",
        body: &[],
    },
    Route {
        id: "video.job.get",
        method: HttpMethod::Get,
        path: "/api/video/jobs/{job_id}",
        body: &[],
    },
    Route {
        id: "msg.send",
        method: HttpMethod::Post,
        path: "/api/conv/{conv}/send",
        body: &["text", "idempotency_key"],
    },
    Route {
        id: "msg.forward",
        method: HttpMethod::Post,
        path: "/api/conv/{to_conv}/forward",
        body: &["from_conv", "to_conv", "msg_id", "idempotency_key"],
    },
    Route {
        id: "group.create",
        method: HttpMethod::Post,
        path: "/api/groups",
        body: &["name", "member_ids"],
    },
    Route {
        id: "group.add_member",
        method: HttpMethod::Post,
        path: "/api/groups/{conv}/members",
        body: &["user_ids"],
    },
    Route {
        id: "video.job.retry",
        method: HttpMethod::Post,
        path: "/api/video/jobs/{job_id}/retry",
        body: &[],
    },
    Route {
        id: "video.view.report",
        method: HttpMethod::Post,
        path: "/api/video/{video_id}/view",
        body: &[
            "viewer",
            "watched_ms",
            "position_ms",
            "completed",
            "session_id",
        ],
    },
    Route {
        id: "msg.recall",
        method: HttpMethod::Post,
        path: "/api/conv/{conv}/recall",
        body: &["msg_id"],
    },
    Route {
        id: "group.kick",
        method: HttpMethod::Delete,
        path: "/api/groups/{conv}/members/{user_id}",
        body: &[],
    },
];

/// 参数默认值：GET 命令的可选参数在这里补齐，模板里就不用写分支。
fn defaults(id: &str) -> &'static [(&'static str, i64)] {
    match id {
        "conv.history" => &[("before_seq", 0), ("limit", 100)],
        "conv.sync" => &[("version", 0)],
        "msg.search" => &[("limit", 50)],
        "community.threads" => &[("limit", 50)],
        _ => &[],
    }
}

/// 补默认值（不改调用方的对象）。
fn with_defaults(id: &str, params: &Value) -> Value {
    let mut out = params.clone();
    let Some(object) = out.as_object_mut() else {
        return out;
    };
    for (key, value) in defaults(id) {
        object.entry((*key).to_string()).or_insert(json!(value));
    }
    out
}

/// 找路由。
#[must_use]
pub fn find(id: &ActionId) -> Option<&'static Route> {
    ROUTES.iter().find(|route| route.id == id.as_str())
}

/// 把参数填进路径模板。
fn fill(template: &str, params: &Value) -> Result<String, DomainError> {
    let mut out = String::with_capacity(template.len() + 32);
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('}') else {
            return Err(DomainError::invalid(format!(
                "路径模板缺少右括号：{template}"
            )));
        };
        let name = &tail[..end];
        out.push_str(&scalar(params, name)?);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// 取一个标量参数并转成 URL 片段。
fn scalar(params: &Value, name: &str) -> Result<String, DomainError> {
    let value = params
        .get(name)
        .ok_or_else(|| DomainError::invalid_field(name, format!("缺少参数 {name}")))?;
    match value {
        Value::String(text) => Ok(percent_encode(text)),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(flag) => Ok(flag.to_string()),
        _ => Err(DomainError::invalid_field(
            name,
            format!("参数 {name} 必须是标量"),
        )),
    }
}

/// 最小 percent-encoding：只处理会破坏 URL 结构的字符。
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b' ' => out.push_str("%20"),
            b'&' => out.push_str("%26"),
            b'#' => out.push_str("%23"),
            b'+' => out.push_str("%2B"),
            b'?' => out.push_str("%3F"),
            b'%' => out.push_str("%25"),
            b'/' => out.push_str("%2F"),
            other => out.push(other as char),
        }
    }
    out
}

/// 按 `body` 字段清单从参数里挑出请求体。
fn body_of(route: &Route, params: &Value) -> Option<Vec<u8>> {
    if route.body.is_empty() {
        return None;
    }
    let mut object = Map::new();
    for field in route.body {
        if let Some(value) = params.get(*field) {
            object.insert((*field).to_string(), value.clone());
        }
    }
    serde_json::to_vec(&Value::Object(object)).ok()
}

/// 把一条命令变成待发请求。
///
/// # Errors
/// 命令是复合命令、缺参数、或路径模板有问题时返回 [`DomainError`]。
pub fn route(action: &ActionId, params: &Value) -> Result<TransportRequest, DomainError> {
    let route = find(action).ok_or_else(|| {
        let hint = COMPOSITE
            .iter()
            .find(|(id, _)| *id == action.as_str())
            .map(|(_, why)| (*why).to_string())
            .unwrap_or_else(|| "未登记的命令".to_string());
        DomainError::invalid(format!("{} 不能直接路由：{hint}", action.as_str()))
    })?;
    let filled = with_defaults(route.id, params);
    let path = fill(route.path, &filled)?;
    let mut request = TransportRequest {
        method: route.method,
        path,
        body: body_of(route, &filled),
        content_type: None,
        headers: Vec::new(),
        command: Some(action.clone()),
        deadline: None,
    };
    if request.body.is_some() {
        request.content_type = Some("application/json".to_string());
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(id: &str) -> ActionId {
        ActionId::parse(id).expect("action id")
    }

    #[test]
    fn every_catalog_action_is_either_routed_or_declared_composite() {
        // 防漂移：catalog 加一条命令，这里必须跟着表态
        for spec in wxlike_cmd::CATALOG {
            let routed = ROUTES.iter().any(|r| r.id == spec.id);
            let composite = COMPOSITE.iter().any(|(id, _)| *id == spec.id);
            assert!(
                routed ^ composite,
                "{} 既没路由也没登记为复合命令（或两边都登记了）",
                spec.id
            );
        }
        // 反向：路由表里不许有 catalog 没有的命令
        for route in ROUTES {
            assert!(
                wxlike_cmd::CATALOG.iter().any(|s| s.id == route.id),
                "{} 在路由表里但 catalog 里没有",
                route.id
            );
        }
    }

    #[test]
    fn path_placeholders_are_substituted_and_encoded() {
        let params = json!({"conv": "d-a~b", "before_seq": 10, "limit": 20});
        let request = route(&action("conv.history"), &params).expect("route");
        assert_eq!(request.path, "/api/history/d-a~b?before_seq=10&limit=20");
        assert_eq!(request.method, HttpMethod::Get);
    }

    #[test]
    fn optional_params_get_defaults() {
        let request = route(&action("conv.history"), &json!({"conv": "c1"})).expect("route");
        assert_eq!(request.path, "/api/history/c1?before_seq=0&limit=100");
    }

    #[test]
    fn query_metacharacters_do_not_break_the_path() {
        let request = route(&action("msg.search"), &json!({"q": "a b&c#d"})).expect("route");
        assert_eq!(
            request.path,
            "/api/search/messages?q=a%20b%26c%23d&limit=50"
        );
    }

    #[test]
    fn write_body_keeps_only_declared_fields() {
        let params = json!({
            "conv": "c1", "text": "hi", "idempotency_key": "k1",
            "extra": "should be dropped"
        });
        let request = route(&action("msg.send"), &params).expect("route");
        let body: Value =
            serde_json::from_slice(request.body.as_ref().expect("body")).expect("json");
        assert_eq!(body["text"], "hi");
        assert!(body.get("extra").is_none(), "未声明字段不该进 body");
        assert_eq!(
            request.command.as_ref().map(|c| c.as_str()),
            Some("msg.send"),
            "审计标记必须带上"
        );
    }

    #[test]
    fn missing_required_param_is_a_field_error() {
        let err = route(&action("msg.get"), &json!({"conv": "c1"})).expect_err("缺 msg_id");
        assert_eq!(err.code(), "invalid_argument");
        assert!(err.message().contains("msg_id"), "{}", err.message());
    }

    #[test]
    fn composite_commands_fail_with_a_reason() {
        let err = route(&action("conv.list"), &json!({})).expect_err("复合命令");
        assert!(err.message().contains("会话列表"), "{}", err.message());
        let err = route(&action("msg.unknown"), &json!({})).expect_err("未登记");
        assert!(err.message().contains("未登记"), "{}", err.message());
    }
}
