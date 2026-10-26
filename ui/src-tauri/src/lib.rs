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

use std::io::{Read as _, Write as _};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use tauri::Manager as _;
use wxlike_core::custom_kind;
use wxlike_core::e2ee::{Crypto, DEVICE_KEY_BYTES};
use wxlike_core::{Envelope, MsgBody, MsgId};
use wxlike_net::Client;

pub mod callrtc;

pub use wxlike_client::{direct_conv_id, Http};

/// 把 `host:port` / `host` 拆开。IPv6 的 `[::1]:80` 也认。
// ── G00-02 纯移动拆分（feat/FP-G00-02）：内容原样搬出，签名零改动 ──
// 本文件保留组合根（run）与测试；其余内容经 pub(crate) 再导出，
// 子模块与测试的 `use super::*` 一次引入，外部路径（crate::AppState 等）不变。
mod app_state;
pub mod commands;
pub mod core_bridge;
mod render;
mod store;
mod util;

pub use app_state::AppState;
pub use core_bridge::CoreState;

/// 读本地库 kv（e2e/运维用：直接证明某键已持久化，不经 AppState）。
pub fn local_kv(path: &std::path::Path, key: &str) -> Option<String> {
    let store = store::LocalStore::open(path).ok()?;
    store.kv_get(key)
}
pub(crate) use render::*;
pub(crate) use store::*;
pub(crate) use util::*;

/// Runs the app. Kept in the library so `main.rs` stays a two-liner and the
/// command registry is testable next to the code it calls.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::new("http://127.0.0.1:8099"))
        .manage(core_bridge::CoreState::new())
        .manage(callrtc::CallEngine::default())
        .invoke_handler(tauri::generate_handler![
            commands::core_dispatch,
            commands::core_subscribe,
            commands::core_capabilities,
            commands::set_server,
            commands::conv_unread,
            commands::conv_mark_read,
            commands::call_state,
            commands::call_action,
            commands::call_signal_post,
            commands::call_signal_take,
            commands::call_rtc_start,
            commands::call_rtc_accept,
            commands::call_rtc_hangup,
            commands::call_rtc_status,
            commands::safety_number,
            commands::verify_peer,
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
            commands::outbox_status,
            commands::outbox_retry,
            commands::outbox_drop,
            commands::net_diag,
            commands::send_image,
            commands::send_redpacket_msg,
            commands::start_recv_pump,
            commands::send_typing,
            commands::forward_message,
            commands::recall_msg,
            commands::send_read_receipt,
            commands::merge_forward,
            commands::load_history,
            commands::search_messages,
            commands::favorite_add,
            commands::favorites_list,
            commands::sessions_list,
            commands::session_revoke,
            commands::sessions_revoke_others,
            commands::favorite_remove,
            commands::secret_enable,
            commands::secret_enabled,
            commands::secret_configure,
            commands::secret_state,
            commands::send_secret_text,
            commands::send_voice,
            commands::send_file,
            commands::send_sticker,
            commands::upload_video,
            commands::send_video,
            commands::download_video,
            commands::upload_avatar,
            commands::local_open,
            commands::local_autopen,
            commands::local_ready,
            commands::local_history,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `local_open` 的路径守卫：`..` 越界必须被挡，前缀相似但不同目录也要挡。
    #[test]
    fn normalize_path_blocks_traversal() {
        let base = normalize_path(std::path::Path::new("/data/app"));
        assert!(normalize_path(std::path::Path::new("/data/app/messages.db")).starts_with(&base));
        assert!(
            !normalize_path(std::path::Path::new("/data/app/../../etc/passwd")).starts_with(&base)
        );
        assert!(!normalize_path(std::path::Path::new("/data/application/x")).starts_with(&base));
    }

    /// 消失消息：只删到期的行，未到期和没设 TTL 的会话不动；TTL 能持久化。
    #[test]
    fn purge_expired_removes_only_due_messages() {
        let path = std::env::temp_dir().join(format!(
            "wxlike-purge-test-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = LocalStore::open(&path).expect("open");
        let row = |conv: &str, seq: i64, msg: &str, at: u64| {
            serde_json::json!({
                "conv_id": conv, "seq": seq, "msg_id": msg, "sender": "u1",
                "kind": "text", "text": "hi", "sent_at_ms": at
            })
        };
        let now = 10_000_000u64;
        store.append(&row("c1", 1, "old", now - 61_000)).unwrap();
        store.append(&row("c1", 2, "fresh", now - 1_000)).unwrap();
        store.append(&row("c2", 1, "other", now - 61_000)).unwrap();

        store.set_secret_ttl("c1", true, 60_000).unwrap();
        store.set_secret_ttl("c2", false, 0).unwrap();
        let ttls = store.load_secret_ttls().unwrap();
        assert_eq!(ttls.get("c1"), Some(&60_000));
        assert!(!ttls.contains_key("c2"));

        let removed = store
            .purge_expired(&[("c1".to_string(), 60_000)], now)
            .unwrap();
        assert_eq!(removed, 1);
        let left: Vec<Option<String>> = store
            .page("c1", 0, 10)
            .unwrap()
            .into_iter()
            .map(|r| r.msg_id)
            .collect();
        assert_eq!(left, vec![Some("fresh".to_string())]);
        assert_eq!(store.page("c2", 0, 10).unwrap().len(), 1);

        drop(store);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }
}
