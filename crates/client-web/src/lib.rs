//! `wxlike-client-web`：客户端核心在**浏览器**里的落地（wasm32）。
//!
//! 分工与原生侧一致：业务逻辑全在 `wxlike-client-core`，这里只补平台能力。
//!
//! | 端口 | 这里的实现 | 浏览器特有的坑 |
//! |---|---|---|
//! | `Transport` | `fetch::FetchTransport` | 只有 `AbortSignal` 能真正取消请求 |
//! | `EventSource` | `ws::WebSocketEvents` | 回调式 API，要接成队列 |
//! | `LocalStore` | `idb::IdbStore` | 配额用尽 / 私有模式是两种不同的失败 |
//! | `Secrets` | `crypto::WebSecrets` | `localStorage` 不是安全存储（见文档） |
//! | `Clock` | `clock::WebClock` | 后台标签页定时器被节流 |
//! | `Platform` | `platform::WebPlatform` | 跨 tab 互斥只能靠 Web Locks |
//!
//! **没有"TS 业务同步引擎"**：浏览器里也不写第二套同步逻辑。`ui/` 只负责
//! 渲染与事件绑定，所有 dispatch / 重试 / 落盘都回到这个 crate 背后的核心。
//!
//! 纯策略（配额判定、冻结恢复、缓存敏感度）在 [`policy`]，与平台无关，
//! 所以在原生上也能跑单测——浏览器行为由 `tests/browser.rs` 覆盖。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod policy;

#[cfg(target_arch = "wasm32")]
pub mod clock;
#[cfg(target_arch = "wasm32")]
pub mod crypto;
#[cfg(target_arch = "wasm32")]
pub mod fetch;
#[cfg(target_arch = "wasm32")]
pub mod idb;
#[cfg(target_arch = "wasm32")]
pub mod platform;
#[cfg(target_arch = "wasm32")]
pub mod ws;

#[cfg(target_arch = "wasm32")]
pub use clock::WebClock;
#[cfg(target_arch = "wasm32")]
pub use crypto::{WebCacheCipher, WebSecrets};
#[cfg(target_arch = "wasm32")]
pub use fetch::FetchTransport;
#[cfg(target_arch = "wasm32")]
pub use idb::IdbStore;
#[cfg(target_arch = "wasm32")]
pub use platform::WebPlatform;
#[cfg(target_arch = "wasm32")]
pub use ws::WebSocketEvents;

#[cfg(target_arch = "wasm32")]
use std::sync::Arc;

/// 组装浏览器端端口。
///
/// 注意返回的是 `Ports`，不是"另一个核心"：核心只有一个
/// （`wxlike_client_core::runtime::Session`），这里只是把平台实现塞进去。
///
/// # Errors
/// IndexedDB 打不开（私有模式）时返回原因；调用方应进入
/// `policy::StorageMode` 的降级路径而不是白屏。
// wasm 是单线程：这些实现持有 `Rc` / JS 句柄，本来就不是 `Send + Sync`。
// 端口类型用 `Arc` 只是为了和原生侧同一个形状；在浏览器里 `Arc` 的原子操作
// 退化成普通读写，没有额外代价，也没有跨线程语义要维护。
#[allow(clippy::arc_with_non_send_sync)]
#[cfg(target_arch = "wasm32")]
pub async fn browser_ports(
    base_url: &str,
) -> Result<wxlike_client_core::ports::Ports, wxlike_client_core::ports::StoreError> {
    let store = idb::IdbStore::open().await?;
    let transport = fetch::FetchTransport::new(base_url);
    let secrets = crypto::WebSecrets::new()
        .map_err(|err| wxlike_client_core::ports::StoreError::Unavailable(err.to_string()))?;
    let notifications = crate::platform::WebPlatform::new();
    Ok(wxlike_client_core::ports::Ports::new(
        Arc::new(transport),
        Arc::new(store),
        Arc::new(clock::WebClock::new()),
        Arc::new(secrets),
        Arc::new(notifications),
        Arc::new(NoNotifications),
    ))
}

/// 浏览器端通知：走 `Notification` API 是宿主（页面）的事，核心只投递。
#[cfg(target_arch = "wasm32")]
struct NoNotifications;

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl wxlike_client_core::ports::NotificationSink for NoNotifications {
    fn is_foreground(&self) -> bool {
        true
    }

    async fn notify(
        &self,
        _notification: wxlike_client_core::ports::Notification,
    ) -> Result<(), wxlike_client_core::ports::NotificationError> {
        Ok(())
    }
}

/// 浏览器宿主（页面胶水）的 wasm 出口：JS 只面对 JSON 字符串。
///
/// 形状对齐 `apps/desktop-web/src/shell/core-contract.ts`：
///
/// * `dispatch(action, paramsJson) -> Promise<string>`：
///   `{ok:true,data}` / `{ok:false,error{code,message,retryable}}`，
///   `code` 用 [`wxlike_types::DomainError::to_wire_code`] 映到 TS 端码表；
/// * `subscribe(onEvent)`：每条事件以 JSON 字符串喂给回调；
/// * `pump_once()`：宿主定时器（如 5s）驱动的 outbox 补发；
/// * `capabilities()`：`CoreCapabilities` 形状的能力上报。
#[cfg(target_arch = "wasm32")]
mod bindgen_entry {
    use std::sync::Arc;

    use wasm_bindgen::prelude::*;

    use wxlike_client_core::cancel::Cancellation;
    use wxlike_client_core::ports::Ports;
    use wxlike_client_core::runtime::{action_ids, EventSubscription, Session};
    use wxlike_types::{
        ActionId, ActionOutcome, ActionRequest, DomainError, IdempotencyKey, TraceId,
    };

    use crate::{clock, crypto, fetch, idb, platform, NoNotifications};

    /// 一轮补发最多处理几条（与原生侧 `NativeCore` 一致）。
    const PUMP_BATCH: u32 = 20;

    /// trace id 的单调计数（同一毫秒内也不同；见 `next_trace`）。
    static REQUEST_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    fn next_trace() -> TraceId {
        let seq = REQUEST_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        TraceId::derive(&format!("web-{seq}"))
    }

    /// 请求参数 / action id 不合法时的 TS 形状校验错误（与 [`ts_error`] 同壳）。
    fn validation_error(message: String) -> String {
        serde_json::json!({
            "ok": false,
            "error": {
                "code": "VALIDATION",
                "message": message,
                "retryable": false,
            }
        })
        .to_string()
    }

    /// 把一段 JSON 文本喂给 JS 回调；回调抛错不终止流（下一次事件还会送）。
    fn deliver_text(on_event: &js_sys::Function, text: &str) {
        let payload = JsValue::from_str(text);
        let _ = on_event.call1(&JsValue::UNDEFINED, &payload);
    }

    /// 传输错误 → 控制消息（事件形状之外，宿主据此提示 / 重订）。
    fn stream_error_text(error: &DomainError) -> String {
        serde_json::json!({
            "__wxlike": "error",
            "code": error.to_wire_code(),
            "message": error.message(),
        })
        .to_string()
    }

    /// 事件泵：把订阅产出的事件逐条喂给 JS 回调，直到流结束或被退订。
    ///
    /// 独立成函数是为了把嵌套压回闸门线内，逻辑与拆分前逐行一致。
    /// `Closure` 由任务持有：任务结束（取消 / 流结束）即释放，无 forget 泄漏。
    async fn pump_events(
        mut subscription: EventSubscription,
        on_event: js_sys::Function,
        task_cancel: Cancellation,
    ) {
        loop {
            match subscription.next().await {
                Ok(Some(event)) => {
                    let text = serde_json::to_string(&event).unwrap_or_default();
                    deliver_text(&on_event, &text);
                }
                // 服务端要求换连接：宿主应重新订阅
                Ok(None) => {
                    deliver_text(&on_event, r#"{"__wxlike":"stream-end"}"#);
                    break;
                }
                Err(error) => {
                    deliver_text(&on_event, &stream_error_text(&error));
                    break;
                }
            }
            if task_cancel.is_cancelled() {
                break;
            }
        }
    }

    /// wasm 核心。
    #[wasm_bindgen]
    pub struct WxlikeCore {
        session: Arc<Session>,
        transport: fetch::FetchTransport,
        /// 实时通道全量地址（`http(s)://` 换 `ws(s)://` + `/ws`，服务端网关）。
        ws_url: String,
    }

    fn ts_error(error: &DomainError) -> serde_json::Value {
        let mut out = serde_json::json!({
            "ok": false,
            "error": {
                "code": error.to_wire_code(),
                "message": if error.is_user_facing() {
                    error.message()
                } else {
                    // 内部错误不透出原文（可能带服务端路径 / SQL）
                    "内部错误，请稍后重试"
                },
                "retryable": error.is_retryable(),
            }
        });
        if let DomainError::RateLimited { retry_after_ms, .. } = error {
            out["error"]["retry_after_ms"] = serde_json::json!(retry_after_ms);
        }
        out
    }

    #[wasm_bindgen]
    impl WxlikeCore {
        /// 打开核心（async 工厂：要开 IndexedDB、抢跨 tab 租约）。
        ///
        /// JS：`const core = await WxlikeCore.new(baseUrl)`。
        /// 失败时 reject（私有模式 / 已有实例）——页面应走
        /// `policy::StorageMode` 的降级路径，而不是白屏。
        // wasm 单线程：实现持有 JS 句柄本来就不是 Send+Sync（与
        // `browser_ports` 同一说明），Arc 只是为了与原生侧同形。
        #[allow(clippy::arc_with_non_send_sync)]
        pub async fn new(base_url: String) -> Result<WxlikeCore, JsValue> {
            let store = idb::IdbStore::open()
                .await
                .map_err(|err| JsValue::from_str(&err.to_string()))?;
            let transport = fetch::FetchTransport::new(&base_url);
            let secrets =
                crypto::WebSecrets::new().map_err(|err| JsValue::from_str(&err.to_string()))?;
            let ports = Ports::new(
                Arc::new(transport.clone()),
                Arc::new(store),
                Arc::new(clock::WebClock::new()),
                Arc::new(secrets),
                Arc::new(platform::WebPlatform::new()),
                Arc::new(NoNotifications),
            );
            let session = Session::open(ports)
                .await
                .map_err(|err| JsValue::from_str(&err.to_string()))?;
            Ok(Self {
                session: Arc::new(session),
                transport,
                ws_url: crate::ws::WebSocketEvents::url_for(&base_url, "/ws"),
            })
        }

        /// 执行一条命令，返回 TS 形状的 ActionResponse（JSON 字符串）。
        ///
        /// `params_json` 里带 `idempotency_key` 字符串时自动挂上
        /// （写命令的硬要求，与 TS 端 `newRequest` 的约定一致）。
        pub async fn dispatch(&self, action: String, params_json: String) -> String {
            let params: serde_json::Value = match serde_json::from_str(&params_json) {
                Ok(value) => value,
                Err(err) => return validation_error(format!("params 不是合法 JSON：{err}")),
            };
            let action_id = match ActionId::parse(&action) {
                Ok(id) => id,
                Err(err) => return validation_error(format!("action id 不合法：{err}")),
            };
            let mut request = ActionRequest::new(action_id, params.clone(), next_trace());
            if let Some(key) = params
                .get("idempotency_key")
                .and_then(serde_json::Value::as_str)
            {
                if let Ok(key) = IdempotencyKey::parse(key) {
                    request = request.with_idempotency(key);
                }
            }
            let result = self.session.dispatch(request).await;
            match &result.outcome {
                // 三态折两态：empty 是成功，data 为 null（UI 端口径）
                ActionOutcome::Ok { data } => {
                    serde_json::json!({"ok": true, "data": data}).to_string()
                }
                ActionOutcome::Empty => serde_json::json!({"ok": true, "data": null}).to_string(),
                ActionOutcome::Failed { error } => ts_error(error).to_string(),
            }
        }

        /// 能力上报（`CoreCapabilities` 形状）。
        #[wasm_bindgen(js_name = capabilities)]
        pub fn capabilities_json(&self) -> String {
            serde_json::json!({
                "contract_version": wxlike_types::CONTRACT_VERSION.to_string(),
                "transport": "wasm",
                "actions": action_ids(),
                "qr_login": false,
                "secrets_os_backed": false,
                "sync_range": false,
                "notes": ["本地真相=IndexedDB；跨 tab 互斥=localStorage 租约"],
            })
            .to_string()
        }

        /// 驱动一轮 outbox 补发（宿主用 `setInterval` 定时调，如 5s）。
        /// 返回 `{attempted,sent,deferred,failed}` 的 JSON。
        pub async fn pump_once(&self) -> String {
            match self.session.pump(PUMP_BATCH).await {
                Ok(report) => serde_json::json!({
                    "attempted": report.attempted,
                    "sent": report.sent,
                    "deferred": report.deferred,
                    "failed": report.failed,
                })
                .to_string(),
                Err(error) => ts_error(&error).to_string(),
            }
        }

        /// 手动重试一条被搁置的待发记录（UI 的"重试"按钮）。
        pub async fn retry_outbox(&self, key: String) -> bool {
            self.session.retry_outbox(&key).await.unwrap_or(false)
        }

        /// 登录/刷新后换 token：立即对后续请求生效。
        pub fn set_token(&self, token: Option<String>) {
            self.transport.set_token(token);
        }

        /// 订阅事件流：每条事件以 `EventEnvelope` 的 JSON 字符串喂给 `on_event`。
        ///
        /// 实时通道直接开 WebSocket（`ws(s)://<host>/ws`，与服务端网关对齐）；
        /// 核心的订阅管线（先落盘、去重、按请求过滤）照常生效。
        /// 回调收到的还有两种控制消息（事件形状之外）：
        /// `{"__wxlike":"stream-end"}`（服务端要求换连接，宿主应重新订阅）
        /// 与 `{"__wxlike":"error","code":...}`（传输错误）。
        pub async fn subscribe(
            &self,
            on_event: js_sys::Function,
        ) -> Result<WxlikeSubscription, JsValue> {
            // 取消信号挂在**订阅自己**上——退订单条订阅不该杀掉整个会话。
            let source = crate::ws::WebSocketEvents::open(&self.ws_url)
                .map_err(|err| JsValue::from_str(&err.into_domain().to_string()))?;
            let cancel = Cancellation::new();
            let subscription = EventSubscription::new(
                Box::new(source),
                self.session.ports().clone(),
                wxlike_types::SubscribeRequest::all(),
                cancel.clone(),
            );
            let task_cancel = cancel.clone();
            wasm_bindgen_futures::spawn_local(pump_events(subscription, on_event, task_cancel));
            Ok(WxlikeSubscription { cancel })
        }

        /// 退出：取消在飞任务、释放租约。幂等。
        pub fn shutdown(&self) {
            self.session.shutdown();
        }
    }

    /// 一条事件订阅的句柄。
    #[wasm_bindgen]
    pub struct WxlikeSubscription {
        cancel: Cancellation,
    }

    #[wasm_bindgen]
    impl WxlikeSubscription {
        /// 退订：唤醒等待中的拉取，任务随之结束。幂等。
        pub fn unsubscribe(&self) {
            self.cancel.cancel();
        }

        /// 是否已退订。
        #[wasm_bindgen(js_name = isUnsubscribed)]
        pub fn is_unsubscribed(&self) -> bool {
            self.cancel.is_cancelled()
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use bindgen_entry::{WxlikeCore, WxlikeSubscription};
