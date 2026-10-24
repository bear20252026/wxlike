//! 写操作转发（D3 的落地方式）。
//!
//! adminx **不直接写平台库**：任何会改数据的动作都变成一次对平台管理端点的
//! HTTP 调用（`x-admin-token` + `x-admin-actor`）。这样：
//!
//! * 平台进程里的内存态（审核模式、风控窗口、群成员缓存、通话房间）仍然只有
//!   一个持有者——绕过它改库会出现"后台显示改了、服务其实没改"的静默降级；
//! * 业务规则（工单能不能这么流转、放行后要不要投递）只有一份实现；
//! * 谁做的：`x-admin-actor` 带的是**验过 MFA 的后台账号名**，平台的审计日志
//!   因此能落到具体的人，而不是一个匿名 "admin"。
//!
//! `ureq` 是阻塞客户端，所以调用放在 `spawn_blocking` 里：一次 10 秒的网络
//! 超时不该占住 tokio 的工作线程。

use std::time::Duration;

use crate::error::{AdminError, Result};

/// 单次转发上限。平台是本机进程，超过这个时间就是它卡住了，不是网络抖。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// 目标 id 的形状：平台生成的都是 hex，群/会话 id 允许 `-_`。
///
/// 为什么要验：`{id}` 会被拼进转发 URL 的路径段。axum 的 `Path<String>` 会做
///百分号解码，所以 `/api/op/reports/%2F..%2Fevil` 里的 `id` 是真字符串 `/../evil`。
/// 不验就等于把后台变成 SSRF/路径穿越的跳板。
pub fn check_target(id: &str) -> Result<String> {
    let id = id.trim();
    if id.is_empty() || id.len() > 64 {
        return Err(AdminError::bad_input("目标 id 长度需在 1..=64"));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(AdminError::bad_input(format!(
            "目标 id {id:?} 形状不对：只允许 ASCII 字母数字与 - _ .（这是路径段，不能含 / 或编码字符）"
        )));
    }
    Ok(id.to_string())
}

/// 把目录里的路径模板换成真实路径。
pub fn render_path(template: &str, target: Option<&str>) -> Result<String> {
    match (template.contains("{id}"), target) {
        (true, Some(t)) => Ok(template.replace("{id}", &check_target(t)?)),
        (true, None) => Err(AdminError::bad_input(format!(
            "操作需要目标 id，路径模板 {template} 里的 {{id}} 没得替换"
        ))),
        (false, _) => Ok(template.to_string()),
    }
}

#[derive(Clone, Debug)]
pub struct Upstream {
    base: String,
    token: String,
    timeout: Duration,
}

/// 一次转发的结论。
#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub body: serde_json::Value,
}

impl Reply {
    /// 平台返回的 `error` 字段（用于留痕与给运维的提示）。
    pub fn error_message(&self) -> Option<&str> {
        self.body.get("error").and_then(|v| v.as_str())
    }
}

impl Upstream {
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// 发一次管理请求。非 2xx 一律转成 [`AdminError::Upstream`]，
    /// 由调用方决定"留痕写什么状态"（写失败也必须留痕）。
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        actor: &str,
        body: Option<serde_json::Value>,
    ) -> Result<Reply> {
        let this = self.clone();
        let method = method.to_string();
        let path = path.to_string();
        let actor = actor.to_string();
        // 阻塞 I/O 挪到专用线程：10 秒超时不能占住工作线程
        tokio::task::spawn_blocking(move || this.call_blocking(&method, &path, &actor, body))
            .await
            .map_err(|e| AdminError::upstream(0, format!("转发任务被终止: {e}")))?
    }

    fn call_blocking(
        &self,
        method: &str,
        path: &str,
        actor: &str,
        body: Option<serde_json::Value>,
    ) -> Result<Reply> {
        let url = format!("{}{}", self.base, path);
        let mut req = match method {
            "POST" => ureq::post(&url),
            "PUT" => ureq::put(&url),
            other => {
                return Err(AdminError::bad_input(format!(
                    "目录里出现了不支持的方法 {other}"
                )))
            }
        };
        req = req
            .config()
            .timeout_global(Some(self.timeout))
            // 4xx/5xx 不当错误：平台的结论写在 body 的 `error` 字段里，
            // 当成错误就只剩一个状态码，运维看不到"去查哪个变量"。
            .http_status_as_error(false)
            // T-Z1（#60）：绝不跟随重定向。admin 请求带 x-admin-token，
            // 3xx 跟随 = 凭据外泄到 Location 指向的任何地方。0 且
            // will_error=false ⇒ 3xx 原样返回，由状态码检查显式拒绝。
            .max_redirects(0)
            .build();
        let req = req
            .header("x-admin-token", &self.token)
            .header("x-admin-actor", actor)
            .header("content-type", "application/json");
        let sent = match body {
            Some(v) => req.send_json(v),
            // 没有 body 的写请求（比如 204 的开关）：显式发空体
            None => req.send_empty(),
        };
        match sent {
            Ok(mut resp) => {
                let (status, value) = read_json(&mut resp);
                finish_reply(status, value)
            }
            Err(err) => Err(AdminError::upstream(0, format!("连不上平台 {url}: {err}"))),
        }
    }
}

/// 响应状态分类（T-Z1/#60）：3xx = 重定向被拒（凭据外泄面），4xx/5xx =
/// 平台错误（body 里带可行动的 error 字段），其余原样成 Reply。
///
/// ureq 3 关掉了"状态码即错误"，4xx/5xx 也走 Ok——但 admin 的对外契约
/// 没变：非 2xx 仍然是 `AdminError::upstream`，否则调用方拿到的是
/// Reply{status:403} 而不是错误。
fn finish_reply(status: u16, value: serde_json::Value) -> Result<Reply> {
    if (300..400).contains(&status) {
        return Err(AdminError::upstream(
            status,
            format!("upstream redirected ({status}): admin never follows redirects; check the platform address"),
        ));
    }
    if (400..600).contains(&status) {
        return Err(AdminError::upstream(status, error_text(&value, status)));
    }
    Ok(Reply {
        status,
        body: value,
    })
}

/// 读响应体。**空 body（204）解出 Null，不算错误**——平台的
/// `moderation.reject` / `moderation.mode` 就是返回 204 的。
fn read_json(resp: &mut http::Response<ureq::Body>) -> (u16, serde_json::Value) {
    let status = resp.status().as_u16();
    let value = resp
        .body_mut()
        .read_json::<serde_json::Value>()
        .unwrap_or(serde_json::Value::Null);
    (status, value)
}

/// 上游正文只在确实是 JSON 的 `error` 字段时转述，否则只说状态码——
/// 不把平台的内部文本原样透给浏览器。
fn error_text(body: &serde_json::Value, status: u16) -> String {
    match body.get("error").and_then(|v| v.as_str()) {
        Some(m) if m.len() <= 200 => m.to_string(),
        _ => format!("HTTP {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_ids_are_shape_checked() {
        assert_eq!(check_target("a1b2c3").unwrap(), "a1b2c3");
        // 波浪号不在允许集里：直聊 id 不该出现在工单/挂起目标里，拒掉比放行安全
        assert!(check_target("d-bob~alice").is_err());
        assert!(check_target("").is_err());
        assert!(check_target("/../../etc/passwd").is_err());
        assert!(check_target("a/b").is_err());
        assert!(check_target("a\nb").is_err());
        assert!(check_target(&"x".repeat(65)).is_err());
        assert_eq!(check_target("g-1_2.3").unwrap(), "g-1_2.3");
    }

    #[test]
    fn path_template_renders_only_for_valid_targets() {
        assert_eq!(
            render_path("/api/admin/reports/{id}/triage", Some("abc")).unwrap(),
            "/api/admin/reports/abc/triage"
        );
        assert!(render_path("/api/admin/reports/{id}/triage", None).is_err());
        let err = render_path("/api/admin/reports/{id}/triage", Some("..%2F")).unwrap_err();
        assert!(err.to_string().contains("形状不对"), "{err}");
        assert_eq!(
            render_path("/api/admin/reconcile", None).unwrap(),
            "/api/admin/reconcile"
        );
    }

    #[test]
    fn base_url_is_normalised() {
        let u = Upstream::new("http://127.0.0.1:8099/", "tok");
        assert_eq!(u.base(), "http://127.0.0.1:8099");
    }

    #[tokio::test]
    async fn transport_failure_is_an_upstream_error_not_a_panic() {
        // 端口 1 上一定没有平台：必须是干净的 502 语义，不能崩也不能当成功
        let u = Upstream::new("http://127.0.0.1:1", "tok").with_timeout(Duration::from_millis(300));
        let err = u
            .call("POST", "/api/admin/reconcile", "ops", None)
            .await
            .unwrap_err();
        match err {
            AdminError::Upstream { status, detail } => {
                assert_eq!(status, 0, "连不上就没有上游状态码");
                assert!(detail.contains("连不上平台"), "{detail}");
            }
            other => panic!("不该是 {other:?}"),
        }
    }

    #[tokio::test]
    async fn unsupported_method_is_refused_before_the_socket() {
        let u = Upstream::new("http://127.0.0.1:1", "tok");
        let err = u.call("DELETE", "/x", "ops", None).await.unwrap_err();
        assert!(err.to_string().contains("不支持的方法"), "{err}");
    }

    /// T-Z1（#60）：上游 3xx 不得跟随——admin 请求带 x-admin-token，
    /// 跟随即凭据外泄到 Location 指向的第三方。必须显式拒绝，且
    /// 重定向目标机收不到第二个请求。
    #[tokio::test]
    async fn redirect_is_rejected_and_never_followed() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc as StdArc;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let target_hits = StdArc::new(AtomicUsize::new(0));
        let hits_for_target = target_hits.clone();

        let redirector = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let redirector_addr = redirector.local_addr().unwrap();

        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match redirector.accept().await {
                    Ok(x) => x,
                    Err(_) => return,
                };
                // 先把请求读干净再回：不读就写+立刻关，ureq 侧是传输错误
                let mut buf = [0u8; 2048];
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(2), sock.read(&mut buf))
                        .await;
                let location = format!("http://{target_addr}/secret");
                let resp = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = target.accept().await else {
                    return;
                };
                hits_for_target.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(2), sock.read(&mut buf))
                        .await;
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n";
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });

        let u = Upstream::new(format!("http://{redirector_addr}"), "admin-secret-token");
        let err = match u.call("POST", "/api/admin/reconcile", "ops", None).await {
            Err(e) => e,
            Ok(reply) => panic!("302 不得当成功: {reply:?}"),
        };
        match &err {
            AdminError::Upstream { status, detail } => {
                assert!((300..400).contains(status), "302 必须显式报错: {status}");
                assert!(detail.contains("never follows redirects"), "{detail}");
                assert!(!detail.contains("admin-secret-token"), "错误里不得带令牌");
            }
            other => panic!("该是 Upstream 错误，却是 {other:?}"),
        }
        // 给可能被误跟随的第二个请求留一个调度窗口，再断言目标机零命中
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(
            target_hits.load(Ordering::SeqCst),
            0,
            "redirect target must never be contacted"
        );
    }
}
