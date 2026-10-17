//! wxlike UI backend (WP-11): Tauri commands bridging the webview to the
//! wxlike server.
//!
//! Architecture rule (task book §0.1.1): all business logic lives in Rust.
//! The frontend only renders and calls `invoke(...)` — it never opens an
//! HTTP connection or a QUIC socket itself. Every server interaction is a
//! command in this crate:
//!
//! * platform data plane (accounts, friends, groups, wallet, media) over
//!   HTTP ([`http`]);
//! * message transport over the QUIC hub ([`wxlike_net::Client`]), with
//!   WP-12 TOFU certificate pinning — first connect pins the hub cert
//!   fingerprint under the app data dir, a changed cert is refused.
//!
//! Command handlers are thin wrappers; the real logic lives in inherent
//! methods on [`AppState`], which the integration tests drive against a
//! **real** in-process server (platform + hub) — mocks are not allowed at
//! this boundary.

// the MSVC linker prints localized progress lines on non-English Windows;
// rustc surfaces that as a lint which is pure noise here
#![allow(linker_messages)]

use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use tauri::Manager as _;
use wxlike_core::{Envelope, MsgBody};
use wxlike_net::Client;

/// Deterministic 1:1 conversation id (mirror of the platform's rule; kept
/// local so the UI crate does not need the server's data plane).
pub fn direct_conv_id(a: &str, b: &str) -> String {
    let (mut x, mut y) = (a.to_string(), b.to_string());
    if x > y {
        std::mem::swap(&mut x, &mut y);
    }
    format!("d-{x}~{y}")
}

/// HTTP client state: base URL + bearer token.
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

    fn req(&self, method: &str, path: &str) -> ureq::Request {
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
    fn json(
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

    fn expect_json(
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

    /// Uploads bytes as the platform's multipart form (`bytes` + `sha256`).
    fn upload(
        &self,
        bytes: &[u8],
        content_type: &str,
        sha256: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        use sha2::Digest as _;
        let digest = hex(sha2::Sha256::digest(bytes).as_slice());
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
    fn download(&self, media_id: &str, thumb: bool) -> Result<(Vec<u8>, String), String> {
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

/// Runs `fut` to completion from sync command code. Commands execute on
/// tauri's blocking pool (no ambient runtime) so plain `block_on` is fine
/// there; tests call the same methods from inside a tokio runtime, where
/// `block_in_place` keeps the worker alive.
fn wait<F: std::future::Future>(fut: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| tauri::async_runtime::block_on(fut))
    } else {
        tauri::async_runtime::block_on(fut)
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// One window's whole backend state. Managed by Tauri; tests drive it via `Arc`.
pub struct AppState {
    http: Mutex<Http>,
    client: Mutex<Option<Client>>,
    hub_addr: Mutex<Option<String>>,
    seq: AtomicU64,
}

impl AppState {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: Mutex::new(Http::new(base_url)),
            client: Mutex::new(None),
            hub_addr: Mutex::new(None),
            seq: AtomicU64::new(1),
        }
    }

    fn http(&self) -> MutexGuard<'_, Http> {
        self.http.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    // ---- server / auth ---------------------------------------------------

    pub fn set_server(&self, base_url: &str) {
        let token = self.http().token.clone();
        *self.http() = Http::new(base_url);
        self.http().token = token;
    }

    pub fn register(&self, name: &str, password: &str) -> Result<serde_json::Value, String> {
        let reply = self.http().expect_json(
            "POST",
            "/api/register",
            Some(&serde_json::json!({"name": name, "password": password})),
        )?;
        self.http().token = Some(reply["token"].as_str().unwrap_or_default().to_string());
        Ok(reply)
    }

    pub fn login(&self, name: &str, password: &str) -> Result<serde_json::Value, String> {
        let reply = self.http().expect_json(
            "POST",
            "/api/login",
            Some(&serde_json::json!({"name": name, "password": password})),
        )?;
        self.http().token = Some(reply["token"].as_str().unwrap_or_default().to_string());
        Ok(reply)
    }

    pub fn logout(&self) -> Result<(), String> {
        self.http().expect_json("POST", "/api/logout", None)?;
        self.http().token = None;
        Ok(())
    }

    pub fn me(&self) -> Result<serde_json::Value, String> {
        self.http().expect_json("GET", "/api/me", None)
    }

    pub fn update_profile(
        &self,
        name: Option<&str>,
        avatar: Option<&str>,
        signature: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        self.http().expect_json(
            "PUT",
            "/api/profile",
            Some(&serde_json::json!({
                "name": name, "avatar": avatar, "signature": signature
            })),
        )
    }

    pub fn user_id(&self) -> Result<String, String> {
        Ok(self.me()?["user_id"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    // ---- friends ----------------------------------------------------------

    pub fn friends(&self) -> Result<serde_json::Value, String> {
        self.http().expect_json("GET", "/api/friends", None)
    }

    pub fn friend_request(&self, to_user_id: &str, message: &str) -> Result<(), String> {
        self.http().expect_json(
            "POST",
            "/api/friends/requests",
            Some(&serde_json::json!({"to_user_id": to_user_id, "message": message})),
        )?;
        Ok(())
    }

    pub fn friend_requests_incoming(&self) -> Result<serde_json::Value, String> {
        self.http()
            .expect_json("GET", "/api/friends/requests/incoming", None)
    }

    pub fn friend_requests_outgoing(&self) -> Result<serde_json::Value, String> {
        self.http()
            .expect_json("GET", "/api/friends/requests/outgoing", None)
    }

    pub fn friend_accept(&self, req_id: &str) -> Result<(), String> {
        self.http().expect_json(
            "POST",
            &format!("/api/friends/requests/{req_id}/accept"),
            None,
        )?;
        Ok(())
    }

    pub fn friend_decline(&self, req_id: &str) -> Result<(), String> {
        self.http().expect_json(
            "POST",
            &format!("/api/friends/requests/{req_id}/decline"),
            None,
        )?;
        Ok(())
    }

    pub fn friend_delete(&self, user_id: &str) -> Result<(), String> {
        self.http()
            .expect_json("DELETE", &format!("/api/friends/{user_id}"), None)?;
        Ok(())
    }

    // ---- groups -----------------------------------------------------------

    pub fn groups_create(&self, name: &str, member_ids: &[String]) -> Result<String, String> {
        let reply = self.http().expect_json(
            "POST",
            "/api/groups",
            Some(&serde_json::json!({"name": name, "member_ids": member_ids})),
        )?;
        Ok(reply["group_id"].as_str().unwrap_or_default().to_string())
    }

    pub fn groups_mine(&self) -> Result<serde_json::Value, String> {
        self.http().expect_json("GET", "/api/groups/mine", None)
    }

    pub fn group_info(&self, group_id: &str) -> Result<serde_json::Value, String> {
        self.http()
            .expect_json("GET", &format!("/api/groups/{group_id}"), None)
    }

    // ---- wallet ------------------------------------------------------------

    pub fn wallet_balance(&self) -> Result<i64, String> {
        let v = self
            .http()
            .expect_json("GET", "/api/wallet/balance", None)?;
        Ok(v["balance"].as_i64().unwrap_or(0))
    }

    pub fn wallet_transfer(&self, to_uid: &str, amount: i64, memo: &str) -> Result<String, String> {
        let reply = self.http().expect_json(
            "POST",
            "/api/wallet/transfer",
            Some(&serde_json::json!({"to_uid": to_uid, "amount": amount, "memo": memo})),
        )?;
        Ok(reply["transfer_id"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    pub fn wallet_redpacket(&self, amount: i64, count: i64, memo: &str) -> Result<String, String> {
        let reply = self.http().expect_json(
            "POST",
            "/api/wallet/redpacket",
            Some(&serde_json::json!({"amount": amount, "count": count, "memo": memo})),
        )?;
        Ok(reply["redpacket_id"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    pub fn wallet_claim(&self, redpacket_id: &str) -> Result<i64, String> {
        let v = self.http().expect_json(
            "POST",
            &format!("/api/wallet/redpacket/{redpacket_id}/claim"),
            None,
        )?;
        Ok(v["amount"].as_i64().unwrap_or(0))
    }

    // ---- media --------------------------------------------------------------

    pub fn upload_media(
        &self,
        bytes: Vec<u8>,
        content_type: &str,
        sha256: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        self.http().upload(&bytes, content_type, sha256)
    }

    pub fn download_media(&self, media_id: &str, thumb: bool) -> Result<MediaBlob, String> {
        let (bytes, content_type) = self.http().download(media_id, thumb)?;
        Ok(MediaBlob {
            base64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes),
            content_type,
        })
    }

    // ---- hub / messages -------------------------------------------------------

    /// Connects to the QUIC hub with TOFU pinning. `pins_path` is where the
    /// hub certificate fingerprint is remembered between runs.
    pub fn connect_hub(
        &self,
        addr: &str,
        user_id: &str,
        pins_path: &std::path::Path,
    ) -> Result<String, String> {
        let sock: std::net::SocketAddr = addr.parse().map_err(|e| format!("bad addr: {e}"))?;
        let client = wait(Client::connect_verified(
            sock,
            "localhost",
            user_id,
            pins_path,
        ))
        .map_err(|e| format!("connect: {e:#}"))?;
        *self.client.lock().unwrap_or_else(PoisonError::into_inner) = Some(client);
        *self.hub_addr.lock().unwrap_or_else(PoisonError::into_inner) = Some(addr.to_string());
        Ok(format!("quic {sock} (tofu pinned)"))
    }

    pub fn is_connected(&self) -> bool {
        self.client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    fn client(&self) -> Result<Client, String> {
        self.client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or_else(|| "未连接到服务器".to_string())
    }

    fn send_body(&self, conv_id: &str, to: Vec<String>, body: MsgBody) -> Result<Envelope, String> {
        let client = self.client()?;
        let me = self.user_id()?;
        let env = Envelope::with_body(conv_id, me, to, self.next_seq(), body);
        wait(client.send(&env)).map_err(|e| format!("send: {e:#}"))?;
        Ok(env)
    }

    pub fn send_text(&self, conv_id: &str, to: Vec<String>, text: &str) -> Result<(), String> {
        self.send_body(conv_id, to, MsgBody::Text(text.to_string()))?;
        Ok(())
    }

    /// Image message: `MsgBody::Bytes(media_id)` — the reference, never the
    /// bytes (WP-05); the peer downloads by id.
    pub fn send_image(&self, conv_id: &str, to: Vec<String>, media_id: &str) -> Result<(), String> {
        self.send_body(conv_id, to, MsgBody::Bytes(media_id.as_bytes().to_vec()))?;
        Ok(())
    }

    pub fn send_redpacket(
        &self,
        conv_id: &str,
        to: Vec<String>,
        redpacket_id: &str,
        amount: i64,
        count: i64,
        memo: &str,
    ) -> Result<(), String> {
        self.send_body(
            conv_id,
            to,
            MsgBody::Custom {
                kind: "wallet.redpacket".into(),
                payload: serde_json::json!({
                    "redpacket_id": redpacket_id,
                    "amount": amount,
                    "count": count,
                    "memo": memo,
                }),
            },
        )?;
        Ok(())
    }

    /// The next envelope from the hub, as the structured event payload the
    /// frontend renders. `None` when not connected. (The live pump in the
    /// running app loops on this; tests call it directly.)
    pub async fn next_event(&self) -> Result<Option<serde_json::Value>, String> {
        let Some(client) = self
            .client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return Ok(None);
        };
        let env = client
            .next_envelope()
            .await
            .map_err(|e| format!("recv: {e:#}"))?;
        Ok(Some(event_payload(&env)))
    }

    /// The receiver pump: forwards hub envelopes to the webview as `im-in`.
    pub fn start_recv_pump(&self, app: tauri::AppHandle) -> Result<(), String> {
        let Some(client) = self
            .client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        else {
            return Err("未连接到服务器".into());
        };
        tauri::async_runtime::spawn(async move {
            use tauri::Emitter as _;
            loop {
                match client.next_envelope().await {
                    Ok(env) => {
                        let _ = app.emit("im-in", event_payload(&env));
                    }
                    Err(_) => {
                        let _ = app.emit("im-down", serde_json::json!({"reason": "hub 连接断开"}));
                        break;
                    }
                }
            }
        });
        Ok(())
    }
}

/// A downloaded object for the webview: base64 (data-URL ready) + type.
#[derive(Serialize, Debug)]
pub struct MediaBlob {
    pub base64: String,
    pub content_type: String,
}

/// Maps an envelope to the event payload the UI renders.
fn event_payload(env: &Envelope) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "sender": env.sender,
        "conv_id": env.conv_id,
        "seq": env.seq,
        "sent_at_ms": env.sent_at_ms,
    });
    match &env.body {
        MsgBody::Text(t) => {
            payload["kind"] = "text".into();
            payload["text"] = t.as_str().into();
        }
        MsgBody::Bytes(b) => {
            payload["kind"] = "image".into();
            payload["media_id"] = String::from_utf8_lossy(b).into_owned().into();
        }
        MsgBody::Notice(n) => {
            payload["kind"] = "system".into();
            payload["text"] = n.as_str().into();
        }
        MsgBody::Recall { target } => {
            payload["kind"] = "recall".into();
            payload["target"] = hex(target).into();
        }
        MsgBody::Custom { kind, payload: p } => {
            payload["kind"] = kind.as_str().into();
            payload["payload"] = p.clone();
        }
    }
    payload
}

// ---------------------------------------------------------------------------
// Tauri command wrappers (thin: parse args, call AppState, flatten errors).
//
// They live in a submodule on purpose: tauri marks `pub` commands'
// helper macros with `#[macro_export]`, which hoists them to the crate
// root — for a command defined AT the root that collides with its own
// textual definition (E0255). A nested module keeps the two apart.
// ---------------------------------------------------------------------------

pub mod commands {

    use super::*;

    #[tauri::command]
    pub fn set_server(state: tauri::State<'_, AppState>, base_url: String) -> Result<(), String> {
        state.set_server(&base_url);
        Ok(())
    }

    #[tauri::command]
    pub fn register(
        state: tauri::State<'_, AppState>,
        name: String,
        password: String,
    ) -> Result<serde_json::Value, String> {
        state.register(&name, &password)
    }

    #[tauri::command]
    pub fn login(
        state: tauri::State<'_, AppState>,
        name: String,
        password: String,
    ) -> Result<serde_json::Value, String> {
        state.login(&name, &password)
    }

    #[tauri::command]
    pub fn logout(state: tauri::State<'_, AppState>) -> Result<(), String> {
        state.logout()
    }

    #[tauri::command]
    pub fn me(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
        state.me()
    }

    #[tauri::command]
    pub fn update_profile(
        state: tauri::State<'_, AppState>,
        name: Option<String>,
        avatar: Option<String>,
        signature: Option<String>,
    ) -> Result<serde_json::Value, String> {
        state.update_profile(name.as_deref(), avatar.as_deref(), signature.as_deref())
    }

    #[tauri::command]
    pub fn friends(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
        state.friends()
    }

    #[tauri::command]
    pub fn friend_request(
        state: tauri::State<'_, AppState>,
        to_user_id: String,
        message: String,
    ) -> Result<(), String> {
        state.friend_request(&to_user_id, &message)
    }

    #[tauri::command]
    pub fn friend_requests_incoming(
        state: tauri::State<'_, AppState>,
    ) -> Result<serde_json::Value, String> {
        state.friend_requests_incoming()
    }

    #[tauri::command]
    pub fn friend_requests_outgoing(
        state: tauri::State<'_, AppState>,
    ) -> Result<serde_json::Value, String> {
        state.friend_requests_outgoing()
    }

    #[tauri::command]
    pub fn friend_accept(state: tauri::State<'_, AppState>, req_id: String) -> Result<(), String> {
        state.friend_accept(&req_id)
    }

    #[tauri::command]
    pub fn friend_decline(state: tauri::State<'_, AppState>, req_id: String) -> Result<(), String> {
        state.friend_decline(&req_id)
    }

    #[tauri::command]
    pub fn friend_delete(state: tauri::State<'_, AppState>, user_id: String) -> Result<(), String> {
        state.friend_delete(&user_id)
    }

    #[tauri::command]
    pub fn groups_create(
        state: tauri::State<'_, AppState>,
        name: String,
        member_ids: Vec<String>,
    ) -> Result<String, String> {
        state.groups_create(&name, &member_ids)
    }

    #[tauri::command]
    pub fn groups_mine(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
        state.groups_mine()
    }

    #[tauri::command]
    pub fn group_info(
        state: tauri::State<'_, AppState>,
        group_id: String,
    ) -> Result<serde_json::Value, String> {
        state.group_info(&group_id)
    }

    #[tauri::command]
    pub fn wallet_balance(state: tauri::State<'_, AppState>) -> Result<i64, String> {
        state.wallet_balance()
    }

    #[tauri::command]
    pub fn wallet_transfer(
        state: tauri::State<'_, AppState>,
        to_uid: String,
        amount: i64,
        memo: String,
    ) -> Result<String, String> {
        state.wallet_transfer(&to_uid, amount, &memo)
    }

    #[tauri::command]
    pub fn wallet_redpacket_cmd(
        state: tauri::State<'_, AppState>,
        amount: i64,
        count: i64,
        memo: String,
    ) -> Result<String, String> {
        state.wallet_redpacket(amount, count, &memo)
    }

    #[tauri::command]
    pub fn wallet_claim(
        state: tauri::State<'_, AppState>,
        redpacket_id: String,
    ) -> Result<i64, String> {
        state.wallet_claim(&redpacket_id)
    }

    #[tauri::command]
    pub fn upload_media(
        state: tauri::State<'_, AppState>,
        bytes: Vec<u8>,
        content_type: String,
        sha256: Option<String>,
    ) -> Result<serde_json::Value, String> {
        state.upload_media(bytes, &content_type, sha256.as_deref())
    }

    #[tauri::command]
    pub fn download_media(
        state: tauri::State<'_, AppState>,
        media_id: String,
        thumb: bool,
    ) -> Result<MediaBlob, String> {
        state.download_media(&media_id, thumb)
    }

    #[tauri::command]
    pub fn conversation_id(
        state: tauri::State<'_, AppState>,
        peer: String,
    ) -> Result<String, String> {
        let me = state.user_id()?;
        Ok(direct_conv_id(&me, &peer))
    }

    #[tauri::command]
    pub fn connect_hub(
        app: tauri::AppHandle,
        state: tauri::State<'_, AppState>,
        addr: String,
    ) -> Result<String, String> {
        let user_id = state.user_id()?;
        let pins = app
            .path()
            .app_data_dir()
            .map(|dir| dir.join("tofu-pins.json"))
            .unwrap_or_else(|_| std::env::temp_dir().join("wxlike-tofu-pins.json"));
        state.connect_hub(&addr, &user_id, &pins)
    }

    #[tauri::command]
    pub fn send_text(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        to: Vec<String>,
        text: String,
    ) -> Result<(), String> {
        state.send_text(&conv_id, to, &text)
    }

    #[tauri::command]
    pub fn send_image(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        to: Vec<String>,
        media_id: String,
    ) -> Result<(), String> {
        state.send_image(&conv_id, to, &media_id)
    }

    #[tauri::command]
    pub fn send_redpacket_msg(
        state: tauri::State<'_, AppState>,
        conv_id: String,
        to: Vec<String>,
        redpacket_id: String,
        amount: i64,
        count: i64,
        memo: String,
    ) -> Result<(), String> {
        state.send_redpacket(&conv_id, to, &redpacket_id, amount, count, &memo)
    }

    #[tauri::command]
    pub fn start_recv_pump(
        app: tauri::AppHandle,
        state: tauri::State<'_, AppState>,
    ) -> Result<(), String> {
        state.start_recv_pump(app)
    }
} // mod commands

/// Runs the app. Kept in the library so `main.rs` stays a two-liner and the
/// command registry is testable next to the code it calls.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new("http://127.0.0.1:8099"))
        .invoke_handler(tauri::generate_handler![
            commands::set_server,
            commands::register,
            commands::login,
            commands::logout,
            commands::me,
            commands::update_profile,
            commands::friends,
            commands::friend_request,
            commands::friend_requests_incoming,
            commands::friend_requests_outgoing,
            commands::friend_accept,
            commands::friend_decline,
            commands::friend_delete,
            commands::groups_create,
            commands::groups_mine,
            commands::group_info,
            commands::wallet_balance,
            commands::wallet_transfer,
            commands::wallet_redpacket_cmd,
            commands::wallet_claim,
            commands::upload_media,
            commands::download_media,
            commands::conversation_id,
            commands::connect_hub,
            commands::send_text,
            commands::send_image,
            commands::send_redpacket_msg,
            commands::start_recv_pump,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
