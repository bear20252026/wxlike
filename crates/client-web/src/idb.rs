//! IndexedDB 实现：[`LocalStore`] 的浏览器侧唯一实现。
//!
//! 键的设计（一个库、两个 store）：
//!
//! | store | 键 | 值 |
//! |---|---|---|
//! | `kv` | `o:<幂等键>` / `m:cursor` / `c:<缓存键>` | JSON 文本 / 字符串 / Uint8Array |
//! | `events` | `e:<20 位补零 seq>` | 事件 JSON 文本 |
//!
//! 事件键**补零**是为了让 IndexedDB 的字符串排序等价于 seq 排序——
//! 这样 `events_after` 可以靠 `getAll` + 前缀过滤，不需要额外索引。
//!
//! 已知取舍：`events_after` 取全量再过滤，所以**必须限长**（[`MAX_EVENTS`]）。
//! 事件是"可重建的缓存"（真相在服务端），超过上限就丢最旧的。要做真正的
//! 分页索引得再加一个 `seq` 索引 + 游标，那是后续卡的事——现在这条路径
//! 在 500 条以内是 O(n) 且 n 有界，比"假装有索引但其实全表扫"诚实。
//!
//! 失败语义按 [`crate::policy::classify`] 映射：配额用尽 → `QuotaExceeded`；
//! 私有模式 / 被策略禁用 → `Unavailable`（**不可重试**，界面据此降级）。

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{IdbDatabase, IdbOpenDbRequest, IdbRequest, IdbTransactionMode};

use wxlike_client_core::ports::{LocalStore, OutboxRecord, StoreError};
use wxlike_types::{Cursor, EventEnvelope, IdempotencyKey};

use crate::policy::classify;

/// 库名。
const DB_NAME: &str = "wxlike-client";
/// 结构版本。加 store / 加索引才 +1。
const DB_VERSION: u32 = 1;
/// 键值 store。
const STORE_KV: &str = "kv";
/// 事件 store。
const STORE_EVENTS: &str = "events";
/// 事件保留上限（超出丢最旧）。
const MAX_EVENTS: usize = 500;
/// seq 补零宽度：20 位足够覆盖 u64。
const SEQ_PAD: usize = 20;

/// IndexedDB 本地库。
pub struct IdbStore {
    db: IdbDatabase,
    /// 打开失败后的降级模式（私有模式等）。
    degraded: Rc<RefCell<Option<StoreError>>>,
}

impl IdbStore {
    /// 打开（必要时建库）。
    ///
    /// # Errors
    /// 打不开时返回映射后的 [`StoreError`]；调用方应据此进入
    /// [`crate::policy::StorageMode`] 的降级路径。
    pub async fn open() -> Result<Self, StoreError> {
        let window =
            web_sys::window().ok_or_else(|| StoreError::Unavailable("没有 window".to_string()))?;
        let factory = window
            .indexed_db()
            .map_err(|err| classify("SecurityError", &format!("{err:?}")))?
            .ok_or_else(|| StoreError::Unavailable("IndexedDB 被禁用（私有模式？）".to_string()))?;
        let request: IdbOpenDbRequest = factory
            .open_with_u32(DB_NAME, DB_VERSION)
            .map_err(|err| classify("InvalidStateError", &format!("{err:?}")))?;
        attach_upgrade(&request);
        let value = JsFuture::from(request_promise(&request))
            .await
            .map_err(|err| dom_error(&err))?;
        let db: IdbDatabase = value
            .dyn_into()
            .map_err(|_| StoreError::Unavailable("open 结果不是数据库".to_string()))?;
        Ok(Self {
            db,
            degraded: Rc::new(RefCell::new(None)),
        })
    }

    /// 降级模式（只读/不可用）时返回原因。
    #[must_use]
    pub fn degraded_reason(&self) -> Option<StoreError> {
        self.degraded.borrow().clone()
    }

    fn store(
        &self,
        name: &str,
        mode: IdbTransactionMode,
    ) -> Result<web_sys::IdbObjectStore, StoreError> {
        let tx = self
            .db
            .transaction_with_str_and_mode(name, mode)
            .map_err(|err| dom_error(&err))?;
        tx.object_store(name).map_err(|err| dom_error(&err))
    }

    async fn put(&self, store: &str, key: &str, value: &JsValue) -> Result<(), StoreError> {
        let object = self.store(store, IdbTransactionMode::Readwrite)?;
        let request = object
            .put_with_key(value, &JsValue::from_str(key))
            .map_err(|err| dom_error(&err))?;
        JsFuture::from(request_promise(&request))
            .await
            .map(|_| ())
            .map_err(|err| dom_error(&err))
    }

    async fn get(&self, store: &str, key: &str) -> Result<Option<JsValue>, StoreError> {
        let object = self.store(store, IdbTransactionMode::Readonly)?;
        let request = object
            .get(&JsValue::from_str(key))
            .map_err(|err| dom_error(&err))?;
        let value = JsFuture::from(request_promise(&request))
            .await
            .map_err(|err| dom_error(&err))?;
        if value.is_undefined() || value.is_null() {
            return Ok(None);
        }
        Ok(Some(value))
    }

    async fn delete(&self, store: &str, key: &str) -> Result<(), StoreError> {
        let object = self.store(store, IdbTransactionMode::Readwrite)?;
        let request = object
            .delete(&JsValue::from_str(key))
            .map_err(|err| dom_error(&err))?;
        JsFuture::from(request_promise(&request))
            .await
            .map(|_| ())
            .map_err(|err| dom_error(&err))
    }

    async fn all(&self, store: &str) -> Result<Vec<JsValue>, StoreError> {
        let object = self.store(store, IdbTransactionMode::Readonly)?;
        let request = object.get_all().map_err(|err| dom_error(&err))?;
        let value = JsFuture::from(request_promise(&request))
            .await
            .map_err(|err| dom_error(&err))?;
        let array = js_sys::Array::from(&value);
        Ok(array.iter().collect())
    }
}

/// 建库时创建 store（幂等）。
fn attach_upgrade(request: &IdbOpenDbRequest) {
    let upgrade = Closure::<dyn FnMut(web_sys::Event)>::new(|event: web_sys::Event| {
        let Some(target) = event.target() else {
            return;
        };
        let Ok(request) = target.dyn_into::<IdbOpenDbRequest>() else {
            return;
        };
        let Ok(result) = request.result() else {
            return;
        };
        let Ok(db) = result.dyn_into::<IdbDatabase>() else {
            return;
        };
        if !db.object_store_names().contains(STORE_KV) {
            let _ = db.create_object_store(STORE_KV);
        }
        if !db.object_store_names().contains(STORE_EVENTS) {
            let _ = db.create_object_store(STORE_EVENTS);
        }
    });
    request.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
    upgrade.forget();
}

/// 把 `IdbRequest` 包成 promise（success 时 resolve 结果，error 时 reject）。
fn request_promise(request: &IdbRequest) -> js_sys::Promise {
    js_sys::Promise::new(&mut |resolve, reject| {
        let on_success = {
            let resolve = resolve.clone();
            Closure::once_into_js(move |event: web_sys::Event| {
                let value = event
                    .target()
                    .and_then(|target| target.dyn_into::<IdbRequest>().ok())
                    .and_then(|request| request.result().ok())
                    .unwrap_or(JsValue::NULL);
                let _ = resolve.call1(&JsValue::UNDEFINED, &value);
            })
        };
        request.set_onsuccess(Some(on_success.unchecked_ref()));
        let on_error = {
            let reject = reject.clone();
            Closure::once_into_js(move |event: web_sys::Event| {
                let error = event
                    .target()
                    .and_then(|target| target.dyn_into::<IdbRequest>().ok())
                    .and_then(|request| request.error().ok())
                    .flatten()
                    .map(JsValue::from)
                    .unwrap_or_else(|| JsValue::from_str("IndexedDB 操作失败"));
                let _ = reject.call1(&JsValue::UNDEFINED, &error);
            })
        };
        request.set_onerror(Some(on_error.unchecked_ref()));
    })
}

/// `DOMException` → [`StoreError`]。
fn dom_error(error: &JsValue) -> StoreError {
    let name = js_sys::Reflect::get(error, &JsValue::from_str("name"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| "UnknownError".to_string());
    let message = js_sys::Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default();
    classify(&name, &message)
}

fn outbox_key(key: &IdempotencyKey) -> String {
    format!("o:{}", key.as_str())
}

fn event_key(seq: u64) -> String {
    format!("e:{seq:0width$}", width = SEQ_PAD)
}

fn cursor_key() -> &'static str {
    "m:cursor"
}

fn cache_key(key: &str) -> String {
    format!("c:{key}")
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl LocalStore for IdbStore {
    async fn put_outbox(&self, record: &OutboxRecord) -> Result<(), StoreError> {
        let text =
            serde_json::to_string(record).map_err(|err| StoreError::Corrupt(err.to_string()))?;
        self.put(
            STORE_KV,
            &outbox_key(&record.idempotency_key),
            &JsValue::from_str(&text),
        )
        .await
    }

    async fn due_outbox(&self, now_ms: i64, limit: u32) -> Result<Vec<OutboxRecord>, StoreError> {
        let mut rows: Vec<OutboxRecord> = Vec::new();
        for value in self.all(STORE_KV).await? {
            let Some(text) = value.as_string() else {
                continue;
            };
            let Ok(record) = serde_json::from_str::<OutboxRecord>(&text) else {
                continue;
            };
            if record.next_attempt_at_ms <= now_ms {
                rows.push(record);
            }
        }
        rows.sort_by_key(|record| record.created_at_ms);
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(rows)
    }

    async fn mark_outbox_done(&self, key: &IdempotencyKey) -> Result<(), StoreError> {
        self.delete(STORE_KV, &outbox_key(key)).await
    }

    async fn defer_outbox(
        &self,
        key: &IdempotencyKey,
        next_attempt_at_ms: i64,
        error: &str,
    ) -> Result<(), StoreError> {
        let Some(value) = self.get(STORE_KV, &outbox_key(key)).await? else {
            return Ok(());
        };
        let Some(text) = value.as_string() else {
            return Ok(());
        };
        let mut record: OutboxRecord =
            serde_json::from_str(&text).map_err(|err| StoreError::Corrupt(err.to_string()))?;
        record.attempts += 1;
        record.next_attempt_at_ms = next_attempt_at_ms;
        record.last_error = Some(error.to_string());
        let text =
            serde_json::to_string(&record).map_err(|err| StoreError::Corrupt(err.to_string()))?;
        self.put(STORE_KV, &outbox_key(key), &JsValue::from_str(&text))
            .await
    }

    async fn append_event(&self, event: &EventEnvelope) -> Result<bool, StoreError> {
        let key = event_key(event.seq.get());
        if self.get(STORE_EVENTS, &key).await?.is_some() {
            return Ok(false);
        }
        let text =
            serde_json::to_string(event).map_err(|err| StoreError::Corrupt(err.to_string()))?;
        self.put(STORE_EVENTS, &key, &JsValue::from_str(&text))
            .await?;
        self.prune_events().await?;
        Ok(true)
    }

    async fn events_after(
        &self,
        cursor: Option<&Cursor>,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>, StoreError> {
        let after = match cursor {
            Some(cursor) => cursor
                .decode()
                .map_err(|err| StoreError::Corrupt(err.to_string()))?
                .seq
                .get(),
            None => 0,
        };
        let mut rows: Vec<EventEnvelope> = Vec::new();
        for value in self.all(STORE_EVENTS).await? {
            let Some(text) = value.as_string() else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<EventEnvelope>(&text) else {
                continue;
            };
            if event.seq.get() > after {
                rows.push(event);
            }
        }
        rows.sort_by_key(|event| event.seq.get());
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(rows)
    }

    async fn put_cursor(&self, cursor: &Cursor) -> Result<(), StoreError> {
        let incoming = cursor
            .decode()
            .map_err(|err| StoreError::Corrupt(err.to_string()))?
            .seq
            .get();
        if let Some(value) = self.get(STORE_KV, cursor_key()).await? {
            if let Some(text) = value.as_string() {
                let previous = Cursor::decode_seq(&text)
                    .map_err(|err| StoreError::Corrupt(err.to_string()))?;
                if incoming < previous {
                    return Err(StoreError::Corrupt(format!(
                        "游标不许回退：{incoming} < {previous}"
                    )));
                }
            }
        }
        self.put(STORE_KV, cursor_key(), &JsValue::from_str(cursor.as_str()))
            .await
    }

    async fn cursor(&self) -> Result<Option<Cursor>, StoreError> {
        let Some(value) = self.get(STORE_KV, cursor_key()).await? else {
            return Ok(None);
        };
        let Some(text) = value.as_string() else {
            return Ok(None);
        };
        Cursor::parse_text(&text)
            .map(Some)
            .map_err(|err| StoreError::Corrupt(err.to_string()))
    }

    async fn put_cache(&self, key: &str, value: &[u8], sensitive: bool) -> Result<(), StoreError> {
        // 敏感标记写进值里：读取时据此决定要不要解密
        let mut payload = Vec::with_capacity(value.len() + 1);
        payload.push(u8::from(sensitive));
        payload.extend_from_slice(value);
        let bytes = js_sys::Uint8Array::from(payload.as_slice());
        self.put(STORE_KV, &cache_key(key), &bytes.buffer()).await
    }

    async fn get_cache(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let Some(value) = self.get(STORE_KV, &cache_key(key)).await? else {
            return Ok(None);
        };
        let Ok(bytes) = value.dyn_into::<js_sys::Uint8Array>() else {
            return Ok(None);
        };
        let mut raw = bytes.to_vec();
        if raw.is_empty() {
            return Ok(None);
        }
        // 去掉敏感标记
        raw.remove(0);
        Ok(Some(raw))
    }

    async fn purge(&self) -> Result<(), StoreError> {
        for store in [STORE_KV, STORE_EVENTS] {
            let object = self.store(store, IdbTransactionMode::Readwrite)?;
            let request = object.clear().map_err(|err| dom_error(&err))?;
            JsFuture::from(request_promise(&request))
                .await
                .map_err(|err| dom_error(&err))?;
        }
        Ok(())
    }
}

impl IdbStore {
    /// 事件超过上限时丢最旧的几条。
    async fn prune_events(&self) -> Result<(), StoreError> {
        let all = self.all(STORE_EVENTS).await?;
        if all.len() <= MAX_EVENTS {
            return Ok(());
        }
        // 键是补零 seq，取全量后按 key 排序即可定位最旧的
        let mut keys: Vec<(String, JsValue)> = Vec::new();
        for value in &all {
            if let Ok(event) =
                serde_json::from_str::<EventEnvelope>(&value.as_string().unwrap_or_default())
            {
                keys.push((event_key(event.seq.get()), value.clone()));
            }
        }
        keys.sort_by(|a, b| a.0.cmp(&b.0));
        let excess = keys.len() - MAX_EVENTS;
        for (key, _) in keys.into_iter().take(excess) {
            self.delete(STORE_EVENTS, &key).await?;
        }
        Ok(())
    }
}

impl IdbStore {
    /// 记下"已经降级"的原因，供界面提示（例如"当前浏览模式不能离线保存"）。
    pub fn note_degraded(&self, error: StoreError) {
        *self.degraded.borrow_mut() = Some(error);
    }
}
