//! 浏览器端验收（wasm-bindgen-test）：这些用例**必须在真实浏览器里跑**。
//!
//!   wasm-pack test --headless --chrome crates/client-web
//!
//! 为什么不能只在原生上测：`wasm` 编译绿不等于能跑。IndexedDB 的事务时序、
//! WebCrypto 的随机数、`fetch` 的 AbortSignal、页面冻结恢复，这些行为只在
//! 浏览器里成立——把它们写成"能编译就过"的测试，等于没测。
//!
//! 覆盖的验收项：
//!
//! 1. 缓存加解密（含"密钥换了必须解不开"）；
//! 2. IndexedDB 建库 / 升级 / 读写 / 幂等事件 / 游标不回退；
//! 3. 断网恢复：fetch 失败被映射成 `Offline`（可重试），不是 `Protocol`；
//! 4. 刷新重放：游标之后的本地事件能读回来；
//! 5. 跨 tab 单实例：第二次抢锁失败。

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::*;
use wxlike_client_core::ports::{LocalStore, Secret, Secrets, StoreError, Transport};
use wxlike_client_web::crypto::{WebCacheCipher, WebSecrets};
use wxlike_client_web::fetch::FetchTransport;
use wxlike_client_web::idb::IdbStore;
use wxlike_client_web::policy::{classify, is_sensitive_cache, PageState, StorageMode};
use wxlike_client_web::ws::WebSocketEvents;
use wxlike_types::{Cursor, CursorKind, EventEnvelope, EventId, EventKind, Seq};

wasm_bindgen_test_configure!(run_in_browser);

fn event(id: &str, seq: u64) -> EventEnvelope {
    EventEnvelope::new(
        EventId::parse(id).expect("event id"),
        Seq::new(seq),
        EventKind::MessageNew,
        serde_json::json!({"text": "你好"}),
        1_700_000_000_000,
    )
}

#[wasm_bindgen_test]
async fn cache_cipher_round_trips_and_rejects_a_foreign_key() {
    let cipher = WebCacheCipher::new(&[7u8; 32]).expect("cipher");
    let sealed = cipher.seal(b"hello").expect("加密");
    assert_ne!(sealed, b"hello".to_vec());
    assert_eq!(cipher.open(&sealed).expect("解密"), b"hello");
    let other = WebCacheCipher::new(&[8u8; 32]).expect("另一个 cipher");
    assert!(other.open(&sealed).is_err(), "换密钥必须解不开");
}

#[wasm_bindgen_test]
async fn secrets_round_trip_through_local_storage() {
    let secrets = WebSecrets::new().expect("localStorage");
    secrets
        .put("test-token", Secret::from_text("abc"))
        .await
        .expect("写");
    let got = secrets.get("test-token").await.expect("读").expect("存在");
    assert_eq!(got.expose(), b"abc");
    secrets.delete("test-token").await.expect("删");
    assert!(secrets.get("test-token").await.expect("读").is_none());
}

#[wasm_bindgen_test]
async fn idb_is_idempotent_on_event_id_and_cursor_never_rewinds() {
    let store = IdbStore::open().await.expect("打开 IndexedDB");
    store.purge().await.expect("清空");
    assert!(store.append_event(&event("evt-1", 1)).await.expect("首次"));
    assert!(!store.append_event(&event("evt-1", 1)).await.expect("重复"));
    assert!(store
        .append_event(&event("evt-2", 2))
        .await
        .expect("第二条"));
    let rows = store.events_after(None, 10).await.expect("读");
    assert_eq!(rows.len(), 2);

    store
        .put_cursor(&Cursor::at(CursorKind::Event, Seq::new(2)))
        .await
        .expect("游标前进");
    let err = store
        .put_cursor(&Cursor::at(CursorKind::Event, Seq::new(1)))
        .await
        .expect_err("回退");
    assert!(matches!(err, StoreError::Corrupt(_)), "{err:?}");
    store.purge().await.expect("收尾");
}

#[wasm_bindgen_test]
async fn idb_survives_a_reopen_which_is_what_refresh_replay_needs() {
    {
        let store = IdbStore::open().await.expect("打开");
        store.purge().await.expect("清空");
        store.append_event(&event("evt-9", 9)).await.expect("写");
    }
    // 模拟刷新：重新 open，数据还在
    let reopened = IdbStore::open().await.expect("重开");
    let rows = reopened.events_after(None, 10).await.expect("读");
    assert_eq!(rows.len(), 1, "刷新之后事件必须还在");
    assert_eq!(rows[0].event_id.as_str(), "evt-9");
    reopened.purge().await.expect("收尾");
}

#[wasm_bindgen_test]
async fn offline_fetch_is_retryable_not_a_protocol_error() {
    // 指向一个必然连不上的地址：浏览器报 TypeError → Offline
    let transport = FetchTransport::new("http://127.0.0.1:1");
    let request = wxlike_client_core::ports::TransportRequest::get("/api/me");
    let error = transport.send(request).await.expect_err("必须失败");
    let domain = error.into_domain();
    assert_eq!(domain.code(), "unavailable");
    assert!(domain.is_retryable(), "断网必须可重试");
}

#[wasm_bindgen_test]
fn websocket_url_swaps_the_scheme() {
    assert_eq!(
        WebSocketEvents::url_for("https://im.example.com", "/api/ws"),
        "wss://im.example.com/api/ws"
    );
    assert_eq!(
        WebSocketEvents::url_for("http://127.0.0.1:8080/", "/api/ws"),
        "ws://127.0.0.1:8080/api/ws"
    );
}

#[wasm_bindgen_test]
async fn second_tab_cannot_take_the_lease_while_the_first_holds_it() {
    use wxlike_client_core::ports::Platform;
    let first = wxlike_client_web::platform::WebPlatform::new();
    let lock = first.acquire_instance_lock().await.expect("第一把");
    let second = wxlike_client_web::platform::WebPlatform::new();
    let err = second.acquire_instance_lock().await.expect_err("第二把");
    assert_eq!(
        err,
        wxlike_client_core::ports::PlatformError::AlreadyRunning
    );
    drop(lock);
    second.acquire_instance_lock().await.expect("释放后可再抢");
}

#[wasm_bindgen_test]
fn policy_covers_the_browser_specific_failures() {
    assert_eq!(
        classify("QuotaExceededError", "quota"),
        StoreError::QuotaExceeded
    );
    assert!(!StorageMode::Unavailable.can_queue());
    assert!(is_sensitive_cache("msg:d-a~b"));
    assert!(PageState::Frozen.needs_resync(PageState::Frozen) == false);
    assert!(PageState::Visible.needs_resync(PageState::Frozen));
}
