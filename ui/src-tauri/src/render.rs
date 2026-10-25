//! 渲染与解码辅助：事件渲染、密文解密、outbox 任务解码、媒体 blob。
//!
//! G00-02 纯移动拆分：内容自 lib.rs 原样搬出，签名零改动；
//! 可见性经 lib.rs 的 pub(crate) 再导出 + `use super::*` 保持。

use super::*;

/// A downloaded object for the webview: base64 (data-URL ready) + type.
#[derive(Serialize, Debug)]
pub struct MediaBlob {
    pub base64: String,
    pub content_type: String,
}

/// Maps an envelope to the event payload the UI renders. Sealed secret-chat
/// payloads are decrypted here (the fallback path marks failures instead of
/// dropping the card).
///
/// `own_text` 是**自己发出的秘密消息的明文**：发送方不能走接收路径去解自己
/// 的密文（对端是自己、没有会话），旧实现因此把每条自己发的秘密消息都渲染成
/// "无法解密"。
pub(crate) fn render_event_with(
    env: &Envelope,
    secret: &Mutex<Option<SecretStore>>,
    own_text: Option<&str>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "sender": env.sender,
        "conv_id": env.conv_id,
        "seq": env.seq,
        "sent_at_ms": env.sent_at_ms,
        "msg_id": hex_str(&env.msg_id),
        "quote_of": env.quote_of.map(|m| hex_str(&m)),
    });
    if let Some(receipt) = env.receipt {
        payload["kind"] = "receipt".into();
        payload["receipt"] = serde_json::json!({
            "state": match receipt {
                wxlike_core::Receipt::Delivered => "delivered",
                wxlike_core::Receipt::Read { .. } => "read",
            },
            "target": env
                .refers_to()
                .map(|m| hex_str(&m))
                .unwrap_or_default(),
        });
        return payload;
    }
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
            payload["target"] = hex_str(target).into();
        }
        MsgBody::Custom { kind, payload: p } => {
            payload["kind"] = kind.as_str().into();
            payload["payload"] = p.clone();
            if kind == custom_kind::E2EE_DM {
                payload["kind"] = "e2ee".into();
                if let Some(text) = own_text {
                    // 自己发的：明文就在手里，不需要（也不能）解密
                    payload["text"] = text.into();
                    payload["encrypted"] = true.into();
                    return payload;
                }
                match try_decrypt_secret(secret, p, &env.sender) {
                    Some(Ok(text)) => {
                        payload["text"] = text.into();
                        payload["encrypted"] = true.into();
                    }
                    Some(Err(err)) => {
                        eprintln!("[e2ee] inbound decrypt failed: {err}");
                        payload["text"] = "[加密消息·无法解密]".into();
                        payload["decrypt_failed"] = true.into();
                    }
                    None => {
                        payload["text"] = "[加密消息·本端未启用或格式不符]".into();
                        payload["decrypt_failed"] = true.into();
                    }
                }
            }
        }
    }
    payload
}

/// Standalone decryption used by [`render_event_with`] (kept separate so the
/// borrow of the secret lock stays short).
pub(crate) fn try_decrypt_secret(
    secret: &Mutex<Option<SecretStore>>,
    payload: &serde_json::Value,
    sender: &str,
) -> Option<Result<String, String>> {
    // **对端是发送者**，不是 payload 里的 `peer`。
    //
    // `peer` 是"发送者眼中的收件人"——对我们来说就是**自己**。拿它当对端去
    // 解密，必然找不到会话（或者找到一条通向自己的空会话），结果是
    // `decryption failed (wrong session, ...)`。
    let peer = sender.to_string();
    let sealed = wxlike_core::e2ee::Sealed {
        pre_key: payload.get("pre_key")?.as_bool()?,
        body: payload.get("body")?.as_str()?.to_string(),
    };
    let mut guard = secret.lock().unwrap_or_else(PoisonError::into_inner);
    let store = guard.as_mut()?;
    let opened = store
        .crypto
        .decrypt_dm(&peer, &sealed)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|err| format!("{err}"));
    if opened.is_ok() {
        // 收到一条 → 棘轮前进 → 立刻落盘（崩溃后仍能继续这条会话）
        if let Err(err) = store.persist() {
            eprintln!("[e2ee] 保存会话状态失败：{err}");
        }
    }
    Some(opened)
}

/// 队列项解码：裸 envelope（普通消息）或 `{env, echo}`（秘密消息，
/// `echo` 是回显要用的明文）。坏 payload 直接判永久失败——重试一万次
/// 也不会变好。
pub(crate) fn decode_outbox_task(
    payload: &serde_json::Value,
) -> Result<(Envelope, Option<String>), wxlike_core::outbox::SendFailure> {
    if let Ok(env) = serde_json::from_value::<Envelope>(payload.clone()) {
        return Ok((env, None));
    }
    let Some(inner) = payload.get("env") else {
        return Err(wxlike_core::outbox::SendFailure::permanent(
            "payload 解不开: 既不是 envelope 也不是 {env, echo}".to_string(),
        ));
    };
    let env = serde_json::from_value::<Envelope>(inner.clone())
        .map_err(|e| wxlike_core::outbox::SendFailure::permanent(format!("payload 解不开: {e}")))?;
    let echo = payload
        .get("echo")
        .and_then(|e| e.as_str())
        .map(str::to_string);
    Ok((env, echo))
}

/// 消失消息判定：`ttl_ms == 0` 表示不消失。
pub(crate) fn expired(sent_at_ms: Option<&serde_json::Value>, ttl_ms: u64, now: u64) -> bool {
    if ttl_ms == 0 {
        return false;
    }
    match sent_at_ms.and_then(serde_json::Value::as_u64) {
        // 时间戳缺失时**不隐藏**：宁可多显示一条，也不要凭空吞掉历史
        None => false,
        Some(sent) => sent.saturating_add(ttl_ms) <= now,
    }
}

/// Minimal percent-encoder for one query value (the search box can carry
/// anything the user typed, including `&` or CJK).
pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tauri command wrappers (thin: parse args, call AppState, flatten errors).
//
// They live in a submodule on purpose: tauri marks `pub` commands'
// helper macros with `#[macro_export]`, which hoists them to the crate
