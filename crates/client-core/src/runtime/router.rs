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
use wxlike_types::{ActionId, Deadline, DomainError};

use crate::ports::{HttpMethod, Transport, TransportRequest};

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

/// 分页命令的默认页大小。
const DEFAULT_PAGE_LIMIT: i64 = 100;
/// 搜索命令的默认返回条数（搜索结果比历史更贵）。
const DEFAULT_SEARCH_LIMIT: i64 = 50;
/// 拼路径时预留的额外容量（参数替换会变长）。
const PATH_SLACK: usize = 32;

/// 参数默认值：GET 命令的可选参数在这里补齐，模板里就不用写分支。
fn defaults(id: &str) -> &'static [(&'static str, i64)] {
    match id {
        "conv.history" => &[("before_seq", 0), ("limit", DEFAULT_PAGE_LIMIT)],
        "conv.sync" => &[("version", 0)],
        "msg.search" => &[("limit", DEFAULT_SEARCH_LIMIT)],
        "community.threads" => &[("limit", DEFAULT_SEARCH_LIMIT)],
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
    let mut out = String::with_capacity(template.len() + PATH_SLACK);
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

// ---------------------------------------------------------------------------
// 复合命令：核心内的最小实现
//
// 依赖方向说明（先查再写，见会话层的调用）：`wxlike-client` 的 `Executor`
// 依赖 `wxlike-cmd`，而 `wxlike-client-core` 也只依赖 `wxlike-cmd`——
// **核心不能反过来调 Executor**。所以这里用 [`Transport`] 端口把
// "一个 action = 多个请求"的最小拼装实现一遍（与 Executor::conv_list
// 同一套端点），核心三端都能用；宿主想要会话白名单过滤时仍可走 Executor。
// ---------------------------------------------------------------------------

/// 这条命令是不是复合命令（`route` 不认识、但 catalog 登记）。
#[must_use]
pub fn is_composite(action: &ActionId) -> bool {
    COMPOSITE.iter().any(|(id, _)| *id == action.as_str())
}

/// 在核心内执行一条复合命令：一次 dispatch = 多个 [`Transport`] 请求。
///
/// * `conv.list`：`/api/me` + `/api/groups/mine` + `/api/friends` + 逐会话未读；
/// * `conv.ack`：带 seq 走 `{"seq": n}`、不带走 `{}`，都 POST 到 `/read`；
/// * `msg.list` / `video.job.create`：核心**不做**——返回不可重试的明确错误，
///   写明宿主该走哪条路（Executor 的区间拼接 / 二进制上传通道）。宁可
///   立刻报清楚，也不悄悄返回 `invalid_argument` 或让 outbox 无限空转。
///
/// # Errors
/// 命令不在 [`COMPOSITE`] 里、或底层请求失败。
pub async fn composite(
    transport: &dyn Transport,
    action: &ActionId,
    params: &Value,
    deadline: Option<Deadline>,
) -> Result<Value, DomainError> {
    match action.as_str() {
        "conv.list" => conv_list(transport, action, deadline).await,
        "conv.ack" => conv_ack(transport, action, params, deadline).await,
        other => Err(host_only_error(other)),
    }
}

/// 核心未实现的复合命令：不可重试（重试也不会变好），点名宿主回退路径。
fn host_only_error(id: &str) -> DomainError {
    let why = COMPOSITE
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, why)| *why)
        .unwrap_or("未登记的命令");
    DomainError::invalid(format!(
        "{id} 是复合命令（{why}），客户端核心未实现；宿主请改用 \
         wxlike-client 的 Executor / Http::post_video_job"
    ))
}

/// 发一个 GET 并解析 JSON（复合命令的构件）。
///
/// 审计标记带**复合命令**的 id：服务端看到的是"这次请求属于 conv.list"，
/// 而不是它内部拼的某个子请求。
async fn get_json(
    transport: &dyn Transport,
    command: &ActionId,
    path: String,
    deadline: Option<Deadline>,
) -> Result<Value, DomainError> {
    let mut request = TransportRequest::get(path).with_command(command.clone());
    request.deadline = deadline;
    let response = transport
        .send(request)
        .await
        .map_err(|err| err.into_domain())?;
    response.json()
}

/// 会话清单：群 + 直聊（由好友列表推导），并附未读。
///
/// 与 `wxlike-client::Executor::conv_list` 同一套端点与字段；差别只有一处：
/// 核心没有会话白名单（那是 CLI/MCP 的策略，不是三端契约），所有会话都返回。
async fn conv_list(
    transport: &dyn Transport,
    command: &ActionId,
    deadline: Option<Deadline>,
) -> Result<Value, DomainError> {
    let me = get_json(transport, command, "/api/me".to_string(), deadline).await?;
    let me_id = me["user_id"].as_str().unwrap_or_default().to_string();

    let mut out: Vec<Value> = Vec::new();
    if let Ok(groups) = get_json(transport, command, "/api/groups/mine".to_string(), deadline).await
    {
        for group in groups.as_array().cloned().unwrap_or_default() {
            let id = group["id"].as_str().unwrap_or_default().to_string();
            out.push(json!({
                "conv": id,
                "kind": "group",
                "title": group["name"],
                "members": group["member_count"],
            }));
        }
    }
    if let Ok(friends) = get_json(transport, command, "/api/friends".to_string(), deadline).await {
        for friend in friends.as_array().cloned().unwrap_or_default() {
            let uid = friend["friend_id"].as_str().unwrap_or_default().to_string();
            if uid.is_empty() {
                continue;
            }
            out.push(json!({
                "conv": wxlike_types::direct_conv_id(&me_id, &uid),
                "kind": "direct",
                "title": uid,
            }));
        }
    }

    // 未读：逐个问信箱。单条失败不炸整个列表（会话还在，未读未知）。
    for item in out.iter_mut() {
        let conv = item["conv"].as_str().unwrap_or_default().to_string();
        let path = format!("/api/conv/{}/unread", percent_encode(&conv));
        match get_json(transport, command, path, deadline).await {
            Ok(unread) => {
                item["unread"] = unread["unread"].clone();
            }
            Err(err) => {
                item["unread"] = Value::Null;
                item["unread_error"] = json!(err.code());
            }
        }
    }
    Ok(Value::Array(out))
}

/// 已读确认：带 seq 推到 seq，不带走到最新（同一个端点，两种 body）。
async fn conv_ack(
    transport: &dyn Transport,
    command: &ActionId,
    params: &Value,
    deadline: Option<Deadline>,
) -> Result<Value, DomainError> {
    let conv = params
        .get("conv")
        .and_then(Value::as_str)
        .ok_or_else(|| DomainError::invalid_field("conv", "conv.ack 缺少 conv 参数"))?;
    let seq = params.get("seq").and_then(Value::as_u64);
    let body = match seq {
        Some(seq) => json!({"seq": seq}),
        None => json!({}),
    };
    let mut request = TransportRequest::post_json(
        format!("/api/conv/{}/read", percent_encode(conv)),
        serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec()),
    )
    .with_command(command.clone());
    request.deadline = deadline;
    let response = transport
        .send(request)
        .await
        .map_err(|err| err.into_domain())?;
    response.json()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{TransportError, TransportResponse};
    use std::collections::VecDeque;

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

    /// 复合命令测试替身：按脚本吐响应，记录路径。
    struct ScriptTransport {
        script: std::sync::Mutex<VecDeque<Result<TransportResponse, TransportError>>>,
        paths: std::sync::Mutex<Vec<String>>,
    }

    impl ScriptTransport {
        fn with(script: Vec<(u16, Value)>) -> Self {
            Self {
                script: std::sync::Mutex::new(
                    script
                        .into_iter()
                        .map(|(status, body)| {
                            Ok(TransportResponse {
                                status,
                                body: body.to_string().into_bytes(),
                            })
                        })
                        .collect(),
                ),
                paths: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn paths(&self) -> Vec<String> {
            self.paths.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    #[async_trait::async_trait]
    impl Transport for ScriptTransport {
        async fn send(
            &self,
            request: TransportRequest,
        ) -> Result<TransportResponse, TransportError> {
            self.paths
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(request.path);
            self.script
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front()
                .unwrap_or(Ok(TransportResponse {
                    status: 503,
                    body: b"{\"error\":\"no script\"}".to_vec(),
                }))
        }

        async fn subscribe(
            &self,
            _request: wxlike_types::SubscribeRequest,
        ) -> Result<Box<dyn crate::ports::EventSource>, TransportError> {
            Err(TransportError::Protocol("测试替身不支持订阅".to_string()))
        }
    }

    /// `conv.list` 复合：一次 dispatch 打多个端点，字段与 Executor 的版本一致。
    #[tokio::test]
    async fn conv_list_composite_fans_out_to_groups_friends_and_unread() {
        let transport = ScriptTransport::with(vec![
            (200, json!({"user_id": "alice"})), // /api/me
            (200, json!([{"id": "g-1", "name": "群", "member_count": 3}])), // /api/groups/mine
            (200, json!([{"friend_id": "bob"}])), // /api/friends
            (200, json!({"unread": 2})),        // 群未读
            (200, json!({"unread": 5})),        // 直聊未读
        ]);
        let value = composite(&transport, &action("conv.list"), &json!({}), None)
            .await
            .expect("复合 conv.list");
        let list = value.as_array().expect("数组");
        assert_eq!(list.len(), 2, "{value}");
        assert_eq!(list[0]["conv"], "g-1");
        assert_eq!(list[0]["kind"], "group");
        assert_eq!(list[0]["unread"], 2);
        // 直聊 id 是确定性推导（d-小~大）
        assert_eq!(list[1]["conv"], "d-alice~bob");
        assert_eq!(list[1]["unread"], 5);
        let paths = transport.paths();
        assert_eq!(
            paths,
            vec![
                "/api/me",
                "/api/groups/mine",
                "/api/friends",
                "/api/conv/g-1/unread",
                "/api/conv/d-alice~bob/unread",
            ]
        );
    }

    /// 未读单条失败不炸列表：会话仍在，未读置空并带原因码。
    #[tokio::test]
    async fn conv_list_tolerates_unread_failures() {
        let transport = ScriptTransport::with(vec![
            (200, json!({"user_id": "a"})),
            (200, json!([])), // 没有群
            (200, json!([{"friend_id": "b"}])),
            (403, json!({"error": "not a member"})),
        ]);
        let value = composite(&transport, &action("conv.list"), &json!({}), None)
            .await
            .expect("复合 conv.list");
        let list = value.as_array().expect("数组");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["unread"], Value::Null);
        assert_eq!(list[0]["unread_error"], "forbidden");
    }

    /// `conv.ack` 复合：带 seq / 不带 seq 两种 body，同一端点。
    #[tokio::test]
    async fn conv_ack_composite_picks_the_body_by_seq() {
        let with_seq = ScriptTransport::with(vec![(200, json!({"ok": true}))]);
        let value = composite(
            &with_seq,
            &action("conv.ack"),
            &json!({"conv": "g-1", "seq": 42}),
            None,
        )
        .await
        .expect("带 seq");
        assert_eq!(value["ok"], true);
        assert_eq!(with_seq.paths(), vec!["/api/conv/g-1/read"]);

        let no_seq = ScriptTransport::with(vec![(200, json!({"ok": true}))]);
        composite(&no_seq, &action("conv.ack"), &json!({"conv": "g-1"}), None)
            .await
            .expect("不带 seq");
        assert_eq!(no_seq.paths(), vec!["/api/conv/g-1/read"]);
    }

    /// 核心未实现的复合命令：立刻、明确地报错，且**不可重试**——
    /// 写命令的可重试错误会被 outbox 当成"已排队"，那就是静默丢给后台空转。
    #[tokio::test]
    async fn host_only_composites_fail_fast_with_a_named_fallback() {
        for id in ["msg.list", "video.job.create"] {
            let err = composite(
                &ScriptTransport::with(vec![]),
                &action(id),
                &json!({}),
                None,
            )
            .await
            .expect_err(id);
            assert_eq!(err.code(), "invalid_argument", "{id}");
            assert!(!err.is_retryable(), "{id} 不可重试");
            assert!(
                err.message().contains("wxlike-client"),
                "{id} 必须点名宿主回退路径：{}",
                err.message()
            );
        }
    }
}
