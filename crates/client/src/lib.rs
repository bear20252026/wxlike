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
#[derive(Clone, Debug)]
pub struct Http {
    pub base: String,
    pub token: Option<String>,
    /// 当前请求代表哪条命令（`msg.send` 之类）。有值时随请求发出，
    /// 服务端据此落审计——**命令 id 是调用方的语义，服务端猜不出来**。
    pub command: Option<String>,
}

impl Http {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            token: None,
            command: None,
        }
    }

    pub fn req(&self, method: &str, path: &str) -> ureq::Request {
        let url = format!("{}{}", self.base.trim_end_matches('/'), path);
        let req = match method {
            "GET" => ureq::get(&url),
            "POST" => ureq::post(&url),
            "PUT" => ureq::put(&url),
            "DELETE" => ureq::delete(&url),
            other => unreachable!("ui only uses GET/POST/PUT/DELETE, got {other}"),
        };
        let req = req.timeout(std::time::Duration::from_secs(10));
        let req = match &self.command {
            Some(id) => req.set("X-Wxlike-Command", id),
            None => req,
        };
        match &self.token {
            Some(token) => req.set("Authorization", &format!("Bearer {token}")),
            None => req,
        }
    }

    /// JSON request -> (status, parsed body). Errors carry the server's
    /// `error` message so the UI can show why, not just that.
    pub fn json(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<(u16, serde_json::Value), String> {
        let resp = match body {
            Some(v) => self.req(method, path).send_json(v.clone()),
            None => self.req(method, path).call(),
        };
        match resp {
            Ok(resp) => {
                let status = resp.status();
                let value = resp
                    .into_json::<serde_json::Value>()
                    .map_err(|e| format!("decode {path}: {e}"))?;
                Ok((status, value))
            }
            Err(ureq::Error::Status(status, resp)) => {
                let value = resp
                    .into_json::<serde_json::Value>()
                    .unwrap_or(serde_json::json!({"error": format!("HTTP {status}")}));
                Ok((status, value))
            }
            Err(err) => Err(format!("{method} {path}: {err}")),
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
        let resp = match body {
            Some(v) => self.req(method, path).send_json(v.clone()),
            None => self.req(method, path).call(),
        };
        match resp {
            Ok(resp) if resp.status() < 300 => Ok(()),
            Ok(resp) => Err(format!("request failed ({})", resp.status())),
            Err(ureq::Error::Status(status, resp)) => {
                let msg = resp.into_string().unwrap_or_default();
                Err(format!("{msg} ({status})"))
            }
            Err(err) => Err(format!("{method} {path}: {err}")),
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

        let resp = self
            .req("POST", "/api/media")
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send_bytes(&body)
            .map_err(|e| match e {
                ureq::Error::Status(status, r) => {
                    let msg = r.into_string().unwrap_or_else(|_| format!("HTTP {status}"));
                    msg
                }
                other => format!("upload: {other}"),
            })?;
        resp.into_json::<serde_json::Value>()
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

        let resp = self
            .req("POST", "/api/video/jobs")
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send_bytes(&body)
            .map_err(|e| match e {
                ureq::Error::Status(status, r) => {
                    r.into_string().unwrap_or_else(|_| format!("HTTP {status}"))
                }
                other => format!("video job submit: {other}"),
            })?;
        resp.into_json::<serde_json::Value>()
            .map_err(|e| format!("video job decode: {e}"))
    }

    /// Downloads an object; returns (bytes, content type).
    pub fn download(&self, media_id: &str, thumb: bool) -> Result<(Vec<u8>, String), String> {
        let suffix = if thumb { "?thumb=1" } else { "" };
        let resp = self
            .req("GET", &format!("/api/media/{media_id}{suffix}"))
            .call()
            .map_err(|e| match e {
                ureq::Error::Status(404, _) => "media not found".to_string(),
                other => format!("download: {other}"),
            })?;
        let content_type = resp.content_type().to_string();
        let mut bytes = Vec::new();
        resp.into_reader()
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
        self.token = reply["token"].as_str().map(str::to_string);
        Ok(reply)
    }

    pub fn login(&mut self, name: &str, password: &str) -> Result<serde_json::Value, String> {
        let reply = self.expect_json(
            "POST",
            "/api/login",
            Some(&serde_json::json!({ "name": name, "password": password })),
        )?;
        self.token = reply["token"].as_str().map(str::to_string);
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
        let resp = self
            .req("POST", path)
            .set("Idempotency-Key", key)
            .send_json(body.clone())
            .map_err(|e| match e {
                ureq::Error::Status(status, r) => {
                    let msg = r.into_string().unwrap_or_else(|_| format!("HTTP {status}"));
                    format!("{msg} ({status})")
                }
                other => format!("POST {path}: {other}"),
            })?;
        resp.into_json::<serde_json::Value>()
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
