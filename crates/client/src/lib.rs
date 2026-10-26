//! 客户端协议层（`wxlike-client`）。
//!
//! 这里装的是**与 UI 无关**的客户端逻辑：HTTP 传输、鉴权头、错误消息提取、
//! 上传/下载。桌面端（Tauri）与将来的移动端（Flutter + UniFFI）共用这一份，
//! 避免"协议实现存在两份"——审计 P2-5 点名的问题。
//!
//! 边界：本 crate **不依赖 Tauri、不依赖任何 UI 类型**，也不碰本地存储
//! （那是 `wxlike-core::Store` 的事）。它只负责"把请求发出去、把错误翻译成
//! 人能读的话"。

pub mod execute;

use std::io::{Read as _, Write as _};

/// 平台 HTTP 客户端：base URL + bearer token。
#[derive(Clone)]
pub struct Http {
    pub base: String,
    /// 一次性设置的 token（CLI/MCP/FFI 启动时从配置注入）。
    ///
    /// 登录/刷新之后要**原地换 token** 的调用方（`client-native` 的
    /// `HttpTransport`，它被 `Arc` 共享、拿不到 `&mut`）改走
    /// [`Http::enable_live_token`] 的共享槽；两处读都合并进
    /// [`Http::effective_token`]。
    pub token: Option<String>,
    /// 共享 token 槽：`enable_live_token` 之后 [`Http::set_live_token`]
    /// 用 `&self` 就能换值（登录成功 → 后续请求立即带上新 token）。
    live_token: Option<std::sync::Arc<std::sync::Mutex<Option<String>>>>,
    /// 当前请求代表哪条命令（`msg.send` 之类）。有值时随请求发出，
    /// 服务端据此落审计——**命令 id 是调用方的语义，服务端猜不出来**。
    pub command: Option<String>,
    /// 共享 agent：连接池 + 全局超时都在这。
    ///
    /// ureq 3 把"超时""4xx 算不算错误"这类设置从请求上挪到了 agent/请求级
    /// config 上；顺带白拿一个连接池（2.x 的 `ureq::get()` 每次新建）。
    agent: ureq::Agent,
}

/// 单次请求超时（秒）。与旧版一致：10 秒，移动端 FFI 靠它兜底。
const REQUEST_TIMEOUT_SECS: u64 = 10;

/// 响应的 content-type；缺省按八位组流算。
///
/// 3.x 的 `Response` 是 `http::Response<Body>`，没有 2.x 的 `content_type()`
/// 便捷方法。客户端与桌面端都要取这个值，所以收成一处。
pub fn content_type_of(resp: &http::Response<ureq::Body>) -> String {
    resp.headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string()
}

/// 4xx/5xx → 人话。**保留 2.x 的分类语义**（issue #22 的验收项）。
///
/// 3.x 把"HTTP 状态码也是错误"关掉了（`http_status_as_error(false)`），于是
/// 调用方自己看 status：4xx 带服务端的 `error` 字段（用户要知道为什么），
/// 5xx 只说"服务端出错"（细节是服务端的事，别泄给界面）。
///
/// 做成模块级函数而不是 `Http` 的方法：桌面端（`ui/src-tauri`）也要用同一套
/// 文案，那边不经过 `Http` 的封装。
pub fn upload_status_error(mut resp: http::Response<ureq::Body>) -> String {
    let status = resp.status();
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    if status.is_client_error() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(message) = value.get("error").and_then(|v| v.as_str()) {
                return format!("{message} ({status})");
            }
        }
        return format!("HTTP {status}: {}", body.trim());
    }
    format!("server error ({status})")
}

/// ureq 3 的错误 Display **直接透出操作系统文案**（Windows 上是
/// "由于目标计算机积极拒绝，无法连接"），而 `crates/ffi` 的分类器靠稳定的
/// 英文特征串判断"这是传输层失败还是业务失败"。
///
/// 所以这里按**变体**翻译，不让本地语言环境渗进协议层：
/// 出来的文案沿用 2.x 的形状（`Connection Failed: Connect error: …`），
/// `NETWORK_MARKERS` 一条都不用改。
fn transport_text(err: &ureq::Error) -> String {
    match err {
        ureq::Error::Timeout(_) => "Connection Failed: Connect error: timed out".to_string(),
        ureq::Error::HostNotFound => "Connection Failed: DNS failed".to_string(),
        ureq::Error::ConnectionFailed => "Connection Failed: connection failed".to_string(),
        ureq::Error::Io(io) => format!(
            "Connection Failed: Connect error: {} (os error {})",
            io_kind_text(io.kind()),
            io.raw_os_error().unwrap_or_default()
        ),
        ureq::Error::StatusCode(code) => format!("HTTP {code}"),
        other => format!("Connection Failed: {other}"),
    }
}

/// `io::ErrorKind` → 稳定英文（`ErrorKind::as_str` 不稳定，自己列）。
fn io_kind_text(kind: std::io::ErrorKind) -> &'static str {
    use std::io::ErrorKind as K;
    match kind {
        K::ConnectionRefused => "connection refused",
        K::ConnectionReset => "connection reset",
        K::ConnectionAborted => "connection aborted",
        K::NotConnected => "not connected",
        K::AddrInUse | K::AddrNotAvailable => "address unavailable",
        K::BrokenPipe => "broken pipe",
        K::AlreadyExists | K::NotFound | K::PermissionDenied => "io error",
        K::TimedOut => "timed out",
        K::Interrupted => "interrupted",
        K::UnexpectedEof => "connection closed",
        K::NetworkUnreachable | K::HostUnreachable => "network unreachable",
        _ => "io error",
    }
}

impl Http {
    pub fn new(base: impl Into<String>) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS)))
            // 4xx/5xx **不是**错误：调用方要按 status 分流（有些端点靠 404/409
            // 表达业务结果），还要读服务端的 `error` 字段给用户看原因。
            .http_status_as_error(false)
            // IM 的控制面直连用户配置的服务器（多数是局域网/本机）。
            // ureq 3 默认读 HTTP(S)_PROXY 环境变量——系统代理会把回环/内网
            // 连接劫走，表现为莫名超时（本机调试与 CI 都踩过）；QUIC 传输
            // 本来就不走代理，HTTP 面保持一致直连。
            .proxy(None)
            .build();
        Self {
            base: base.into(),
            token: None,
            live_token: None,
            command: None,
            agent: config.into(),
        }
    }

    /// 开启共享 token 槽：之后 [`Http::set_live_token`] 用 `&self` 换值，
    /// 所有克隆（含被 `Arc` 包住的传输层）共享同一个槽。
    ///
    /// 这是"登录成功后换 token"的活更新路径；CLI/MCP 这类启动时定死
    /// token 的调用方不需要它（直接给 `token` 字段赋值即可）。
    pub fn enable_live_token(&mut self) {
        if self.live_token.is_none() {
            self.live_token = Some(std::sync::Arc::new(std::sync::Mutex::new(
                self.token.clone(),
            )));
        }
    }

    /// 换 token（需先 [`Http::enable_live_token`]）。`&self`：调用方往往
    /// 只有共享句柄（`Arc<HttpTransport>`），拿不到 `&mut`。
    pub fn set_live_token(&self, token: Option<String>) {
        if let Some(slot) = &self.live_token {
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = token;
        }
    }

    /// 当前生效的 token：共享槽优先，其次一次性字段。
    #[must_use]
    pub fn effective_token(&self) -> Option<String> {
        if let Some(slot) = &self.live_token {
            return slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        }
        self.token.clone()
    }

    /// 登录/注册成功后存 token：两个位置都写，读侧永远一致。
    fn store_token(&mut self, token: Option<String>) {
        if let Some(slot) = &self.live_token {
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = token.clone();
        }
        self.token = token;
    }

    /// URL 拼接 + 公共头（命令 id、bearer token）。
    ///
    /// ureq 3 按"带不带 body"把 builder 分成两个类型（typestate），
    /// `GET` 与 `POST` 不再是同一个类型——所以读请求和写请求分成两个入口，
    /// 见 [`Self::req`] 与 [`Self::req_write`]。公共头两边都要加，
    /// 所以抽成这个泛型函数。
    fn prepare<B>(&self, req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        let req = match &self.command {
            Some(id) => req.header("X-Wxlike-Command", id),
            None => req,
        };
        match self.effective_token() {
            Some(token) => req.header("Authorization", &format!("Bearer {token}")),
            None => req,
        }
    }

    /// 读请求（GET / DELETE）：没有 body，直接 `.call()`。
    pub fn req(
        &self,
        method: &str,
        path: &str,
    ) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let url = format!("{}{}", self.base.trim_end_matches('/'), path);
        let req = match method {
            "GET" => self.agent.get(&url),
            "DELETE" => self.agent.delete(&url),
            other => unreachable!("req() is for GET/DELETE, got {other}; use req_write()"),
        };
        self.prepare(req)
    }

    /// 写请求（POST / PUT）：带 body，用 `.send()` / `.send_json()`。
    pub fn req_write(
        &self,
        method: &str,
        path: &str,
    ) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let url = format!("{}{}", self.base.trim_end_matches('/'), path);
        let req = match method {
            "POST" => self.agent.post(&url),
            "PUT" => self.agent.put(&url),
            other => unreachable!("req_write() is for POST/PUT, got {other}"),
        };
        self.prepare(req)
    }

    /// 4xx/5xx → 人话。**保留 2.x 的分类语义**（issue #22 的验收项）。
    ///
    /// 3.x 把"HTTP 状态码也是错误"关掉了（`http_status_as_error(false)`），
    /// 于是这里自己看 status：4xx 带服务端的 `error` 字段（用户要知道为什么），
    /// 5xx 只说"服务端出错"（细节是服务端的事，别泄给界面）。
    fn status_error(resp: http::Response<ureq::Body>) -> String {
        upload_status_error(resp)
    }

    /// 发出去的请求统一走这一层：非 2xx → 带原因的错误。
    fn ok(resp: http::Response<ureq::Body>) -> Result<(), String> {
        if resp.status().is_success() {
            return Ok(());
        }
        Err(Self::status_error(resp))
    }

    /// JSON request -> (status, parsed body). Errors carry the server's
    /// `error` message so the UI can show why, not just that.
    pub fn json(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(u16, serde_json::Value), String> {
        // 写方法**即使没有 body** 也要走 req_write：`send_empty()` 才是
        // "POST/PUT 带一个空体"的正确形状。走 req() 会踩 typestate 的
        // unreachable（GET/DELETE 才是无体请求）。
        let resp = match (method, body) {
            ("GET" | "DELETE", None) => self.req(method, path).call(),
            ("GET" | "DELETE", Some(_)) => {
                return Err(format!("{method} {path}: 读方法不该有 body"))
            }
            (_, Some(v)) => self.req_write(method, path).send_json(v),
            (_, None) => self.req_write(method, path).send_empty(),
        };
        // 3.x 关掉了"状态码即错误"：4xx/5xx 也是 Ok。**调用方仍然要能按 status
        // 分流**（有些端点就是靠 404/409 表达业务结果），所以这里照旧返回 status，
        // 只是错误体不再由 ureq 替我们吞掉。
        match resp {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let value = resp
                    .body_mut()
                    .read_json::<serde_json::Value>()
                    .unwrap_or(serde_json::json!({"error": format!("HTTP {status}")}));
                Ok((status, value))
            }
            Err(err) => Err(format!("{method} {path}: {}", transport_text(&err))),
        }
    }

    pub fn expect_json(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let (status, value) = self.json(method, path, body)?;
        if (200..300).contains(&status) {
            Ok(value)
        } else {
            Err(format!(
                "{} ({status})",
                value["error"].as_str().unwrap_or("request failed")
            ))
        }
    }

    /// 2xx-or-bust for endpoints that return no body (204 and friends).
    pub fn expect_empty(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(), String> {
        // 同 `json()`：写方法无 body 也要走 `send_empty()`，`req()` 只收 GET/DELETE。
        let resp = match (method, body) {
            ("GET" | "DELETE", None) => self.req(method, path).call(),
            ("GET" | "DELETE", Some(_)) => {
                return Err(format!("{method} {path}: 读方法不该有 body"))
            }
            (_, Some(v)) => self.req_write(method, path).send_json(v),
            (_, None) => self.req_write(method, path).send_empty(),
        };
        match resp {
            Ok(resp) => Self::ok(resp),
            Err(err) => Err(format!("{method} {path}: {}", transport_text(&err))),
        }
    }

    /// Uploads bytes as the platform's multipart form (`bytes` + `sha256`).
    ///
    /// `conv` 声明归属会话（issue #14）：下载时只有会话成员能取。
    /// `public` 用于头像这类所有登录用户都要看的资源。
    pub fn upload(
        &self,
        bytes: &[u8],
        content_type: &str,
        sha256: Option<&str>,
        conv: Option<&str>,
        public: bool,
    ) -> Result<serde_json::Value, String> {
        use sha2::Digest as _;
        let digest = hex::encode(sha2::Sha256::digest(bytes).as_slice());
        let boundary = "wxlike-ui-form-7f3c9";
        let mut body = Vec::with_capacity(bytes.len() + 256);
        let part = |field: &str, value: &str, body: &mut Vec<u8>| {
            let _ = write!(
                body,
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"\r\n\r\n{value}\r\n"
            );
        };
        part(
            "sha256",
            &sha256.map(str::to_string).unwrap_or(digest),
            &mut body,
        );
        if let Some(conv) = conv {
            part("conv", conv, &mut body);
        }
        if public {
            part("visibility", "public", &mut body);
        }
        let _ = write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"bytes\"; filename=\"blob\"\r\nContent-Type: {content_type}\r\n\r\n"
        );
        body.extend_from_slice(bytes);
        let _ = write!(body, "\r\n--{boundary}--\r\n");

        let mut resp = self
            .req_write("POST", "/api/media")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .send(&body)
            .map_err(|e| format!("upload: {}", transport_text(&e)))?;
        // 3.x 的 4xx/5xx 不是错误：自己看 status 并保留服务端的 error 字段
        if !resp.status().is_success() {
            return Err(Self::status_error(resp));
        }
        resp.body_mut()
            .read_json::<serde_json::Value>()
            .map_err(|e| format!("upload decode: {e}"))
    }

    /// 提交一个转码作业（multipart：`bytes` 文件 + 可选 `callback_url`）。
    ///
    /// 与 [`Self::upload`] 的区别只是目标路由和字段名——两处都用 `send_bytes` 手搭
    /// multipart，因为 `ureq` 的 `multipart` feature 会拖进 `mime` 依赖树，
    /// 而这里只需要两个字段。
    pub fn post_video_job(
        &self,
        bytes: &[u8],
        callback_url: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let boundary = "wxlike-video-form-4b1d2";
        let mut body = Vec::with_capacity(bytes.len() + 256);
        if let Some(url) = callback_url {
            let _ = write!(
                body,
                "--{boundary}\r\nContent-Disposition: form-data; name=\"callback_url\"\r\n\r\n{url}\r\n"
            );
        }
        let _ = write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"bytes\"; filename=\"source\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        );
        body.extend_from_slice(bytes);
        let _ = write!(body, "\r\n--{boundary}--\r\n");

        let mut resp = self
            .req_write("POST", "/api/video/jobs")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .send(&body)
            .map_err(|e| format!("video job submit: {}", transport_text(&e)))?;
        if !resp.status().is_success() {
            return Err(Self::status_error(resp));
        }
        resp.body_mut()
            .read_json::<serde_json::Value>()
            .map_err(|e| format!("video job decode: {e}"))
    }

    /// Downloads an object; returns (bytes, content type).
    pub fn download(&self, media_id: &str, thumb: bool) -> Result<(Vec<u8>, String), String> {
        let suffix = if thumb { "?thumb=1" } else { "" };
        let resp = self
            .req("GET", &format!("/api/media/{media_id}{suffix}"))
            .call()
            .map_err(|e| format!("download: {}", transport_text(&e)))?;
        if resp.status() == http::StatusCode::NOT_FOUND {
            return Err("media not found".to_string());
        }
        if !resp.status().is_success() {
            return Err(Self::status_error(resp));
        }
        let content_type = content_type_of(&resp);
        let mut bytes = Vec::new();
        resp.into_body()
            .into_reader()
            .read_to_end(&mut bytes)
            .map_err(|e| format!("download read: {e}"))?;
        Ok((bytes, content_type))
    }
}

/// Deterministic 1:1 conversation id (mirror of the platform's rule).
///
/// 放在客户端层而不是 UI 里：会话 id 的算法是协议的一部分，移动端也要用。
pub fn direct_conv_id(a: &str, b: &str) -> String {
    let (mut x, mut y) = (a.to_string(), b.to_string());
    if x > y {
        std::mem::swap(&mut x, &mut y);
    }
    format!("d-{x}~{y}")
}

impl Http {
    /// 登录并记住 token。
    pub fn register(&mut self, name: &str, password: &str) -> Result<serde_json::Value, String> {
        let reply = self.expect_json(
            "POST",
            "/api/register",
            Some(&serde_json::json!({ "name": name, "password": password })),
        )?;
        let token = reply["token"].as_str().map(str::to_string);
        self.store_token(token);
        Ok(reply)
    }

    pub fn login(&mut self, name: &str, password: &str) -> Result<serde_json::Value, String> {
        let reply = self.expect_json(
            "POST",
            "/api/login",
            Some(&serde_json::json!({ "name": name, "password": password })),
        )?;
        let token = reply["token"].as_str().map(str::to_string);
        self.store_token(token);
        Ok(reply)
    }

    pub fn me(&self) -> Result<serde_json::Value, String> {
        self.expect_json("GET", "/api/me", None)
    }

    /// P3 未读（信箱是唯一真相）。
    pub fn conv_unread(&self, conv: &str) -> Result<serde_json::Value, String> {
        self.expect_json("GET", &format!("/api/conv/{conv}/unread"), None)
    }

    /// 把已读游标推到最新。
    pub fn mark_conv_read(&self, conv: &str) -> Result<serde_json::Value, String> {
        self.expect_json(
            "POST",
            &format!("/api/conv/{conv}/read"),
            Some(&serde_json::json!({})),
        )
    }

    /// 复制一份并打上命令标记：这一次调用（以及它发的每个请求）都会带
    /// `X-Wxlike-Command`，服务端据此落审计。
    pub fn with_command(&self, id: &str) -> Self {
        let mut next = self.clone();
        next.command = Some(id.to_string());
        next
    }

    /// 带幂等键的 POST：服务端用同一个 key 重放时返回**第一次**的结果。
    ///
    /// 为什么只给写命令开这个口子：agent 会重试，而"建群"重试两次就是两个群。
    pub fn post_idempotent(
        &self,
        path: &str,
        body: &serde_json::Value,
        key: &str,
    ) -> Result<serde_json::Value, String> {
        let mut resp = self
            .req_write("POST", path)
            .header("Idempotency-Key", key)
            .send_json(body)
            .map_err(|e| format!("POST {path}: {}", transport_text(&e)))?;
        if !resp.status().is_success() {
            return Err(Self::status_error(resp));
        }
        resp.body_mut()
            .read_json::<serde_json::Value>()
            .map_err(|e| format!("decode {path}: {e}"))
    }

    /// 把已读游标推到指定 seq。
    pub fn mark_conv_read_upto(&self, conv: &str, seq: u64) -> Result<serde_json::Value, String> {
        self.expect_json(
            "POST",
            &format!("/api/conv/{conv}/read"),
            Some(&serde_json::json!({ "seq": seq })),
        )
    }

    /// 五层差分指令（客户端据此决定全量拉还是增量拉）。
    pub fn conv_sync(&self, conv: &str, version: u64) -> Result<serde_json::Value, String> {
        self.expect_json(
            "GET",
            &format!("/api/conv/{conv}/sync?version={version}"),
            None,
        )
    }
}
