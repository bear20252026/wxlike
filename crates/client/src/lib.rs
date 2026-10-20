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
}

impl Http {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            token: None,
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
    pub fn upload(
        &self,
        bytes: &[u8],
        content_type: &str,
        sha256: Option<&str>,
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

    /// 五层差分指令（客户端据此决定全量拉还是增量拉）。
    pub fn conv_sync(&self, conv: &str, version: u64) -> Result<serde_json::Value, String> {
        self.expect_json(
            "GET",
            &format!("/api/conv/{conv}/sync?version={version}"),
            None,
        )
    }
}
