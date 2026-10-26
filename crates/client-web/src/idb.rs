//! IndexedDB 实现：[`LocalStore`] 的浏览器侧唯一实现。
//!
//! 键的设计（一个库、两个 store）：
//!
//! | store | 键 | 值 |
//! |---|---|---|
//! | `kv` | `o:<幂等键>` / `m:cursor` / `c:<缓存键>` | JSON 文本 / 字符串 / Uint8Array |
//! | `events` | `e:<event_id>` | 事件 JSON 对象（附 `_seq` 排序字段） |
//!
//! 事件**按 `event_id` 键**：`seq` 是**每会话**的序列号，两个会话的 seq=7
//! 是两条不同的事件——曾经"只用补零 seq 当键"的写法会把后者静默顶掉
//! （跨会话事件丢失，且 dedup 判定跟着错）。
//!
//! 修剪（丢最旧）走 `by_seq` 索引：值里带 20 位补零的 `_seq` 排序字段，
//! `getAllKeys` 按索引序返回主键，删头部即可，不必全表扫描。
//!
//! 已知取舍：`events_after` 取全量再过滤，所以**必须限长**（[`MAX_EVENTS`]）。
//! 事件是"可重建的缓存"（真相在服务端），超过上限就丢最旧的。
//!
//! 失败语义按 [`crate::policy::classify`] 映射：配额用尽 → `QuotaExceeded`；
//! 私有模式 / 被策略禁用 → `Unavailable`（**不可重试**，界面据此降级）。

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{IdbDatabase, IdbOpenDbRequest, IdbRequest, IdbTransactionMode};

use wxlike_client_core::ports::{
    check_cursor_advance, cursor_floor, page_events, LocalStore, OutboxRecord, StoreError,
};
use wxlike_types::{Cursor, EventEnvelope, IdempotencyKey};

use crate::policy::classify;

/// 库名。
const DB_NAME: &str = "wxlike-client";
/// 结构版本。v2：事件改按 `event_id` 键、值带 `_seq` 排序字段并建索引。
const DB_VERSION: u32 = 2;
/// 键值 store。
const STORE_KV: &str = "kv";
/// 事件 store。
const STORE_EVENTS: &str = "events";
/// 事件按 seq 修剪用的索引名（keyPath = 值里的 `_seq` 排序字段）。
const INDEX_BY_SEQ: &str = "by_seq";
/// 事件保留上限（超出丢最旧）。
const MAX_EVENTS: usize = 500;
/// seq 补零宽度：20 位足够覆盖 u64，补零后字符串序 = 数值序。
const SEQ_PAD: usize = 20;

/// IndexedDB 本地库。
pub struct IdbStore {
    db: IdbDatabase,
    /// 打开失败后的降级模式（私有模式等）。
    degraded: Rc<RefCell<Option<StoreError>>>,
    /// 敏感缓存的加解密器。密钥来自 `WebSecrets`（localStorage），
    /// load-or-create；拿不到密钥存储时为 `None`——敏感缓存会明确报
    /// "不可用"而不是明文落盘。
    cipher: Option<crate::crypto::WebCacheCipher>,
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
        // 敏感缓存的密钥：load-or-create（localStorage 里的 WebSecrets）。
        // 失败不炸库：普通缓存照用，敏感缓存明确报不可用。
        let cipher = match crate::crypto::WebSecrets::new() {
            Ok(secrets) => crate::crypto::WebCacheCipher::load_or_create(&secrets)
                .await
                .ok(),
            Err(_) => None,
        };
        Ok(Self {
            db,
            degraded: Rc::new(RefCell::new(None)),
            cipher,
        })
    }

    /// 降级模式（只读/不可用）时返回原因。
    #[must_use]
    pub fn degraded_reason(&self) -> Option<StoreError> {
        self.degraded
            .try_borrow()
            .ok()
            .and_then(|slot| slot.clone())
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

/// 建库 / 升级时整理 store 与索引（幂等）。
///
/// v1 → v2：事件 store 从"补零 seq 当键"改成"event_id 当键 + `_seq` 索引"。
/// 事件是**可重建的缓存**（真相在服务端），升级时直接清掉旧 store 重建，
/// 不做逐条迁移——下次订阅 / 重放会重新落盘。
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
        if db.object_store_names().contains(STORE_EVENTS) {
            // v1 的事件按补零 seq 键、值是字符串——索引不了也键不对，
            // 丢掉重建（缓存语义，见函数注释）
            let _ = db.delete_object_store(STORE_EVENTS);
        }
        if let Ok(store) = db.create_object_store(STORE_EVENTS) {
            let _ = store.create_index_with_str(INDEX_BY_SEQ, "_seq");
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

/// 事件主键：**event_id**。seq 是每会话的，跨会话会撞键（见模块注释）。
fn event_key(event_id: &wxlike_types::EventId) -> String {
    format!("e:{}", event_id.as_str())
}

/// `_seq` 排序字段：20 位补零，字符串序 = 数值序（索引按它修剪最旧）。
fn seq_sort_key(seq: u64) -> String {
    format!("{seq:0width$}", width = SEQ_PAD)
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
        retry_at_ms: i64,
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
        record.next_attempt_at_ms = retry_at_ms;
        record.last_error = Some(error.to_string());
        let text =
            serde_json::to_string(&record).map_err(|err| StoreError::Corrupt(err.to_string()))?;
        self.put(STORE_KV, &outbox_key(key), &JsValue::from_str(&text))
            .await
    }

    async fn append_event(&self, event: &EventEnvelope) -> Result<bool, StoreError> {
        let key = event_key(&event.event_id);
        if self.get(STORE_EVENTS, &key).await?.is_some() {
            return Ok(false);
        }
        let text =
            serde_json::to_string(event).map_err(|err| StoreError::Corrupt(err.to_string()))?;
        // 值存成 JSON **对象**（不是字符串）：索引的 keyPath 要从值里取
        // `_seq`。额外注入 `_seq` = 补零排序字段，供 `by_seq` 索引修剪用。
        let value = js_sys::JSON::parse(&text)
            .map_err(|err| StoreError::Corrupt(format!("事件不是合法 JSON：{err:?}")))?;
        let _ = js_sys::Reflect::set(
            &value,
            &JsValue::from_str("_seq"),
            &JsValue::from_str(&seq_sort_key(event.seq.get())),
        );
        self.put(STORE_EVENTS, &key, &value).await?;
        self.prune_events().await?;
        Ok(true)
    }

    async fn events_after(
        &self,
        cursor: Option<&Cursor>,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>, StoreError> {
        let after = cursor_floor(cursor)?;
        let mut rows = Vec::new();
        for value in self.all(STORE_EVENTS).await? {
            let Some(text) = value_as_event_text(&value) else {
                continue;
            };
            if let Ok(event) = serde_json::from_str::<EventEnvelope>(&text) {
                rows.push(event);
            }
        }
        Ok(page_events(rows, after, limit))
    }

    async fn put_cursor(&self, cursor: &Cursor) -> Result<(), StoreError> {
        let current = self
            .get(STORE_KV, cursor_key())
            .await?
            .and_then(|value| value.as_string());
        check_cursor_advance(current.as_deref(), cursor)?;
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
        // 敏感内容**先加密再落盘**（AEAD 与原生侧同一套，密钥在 WebSecrets）。
        // 密钥存储不可用时明确报错：明文落盘不是可接受的降级。
        let body = if sensitive {
            let cipher = self.cipher.as_ref().ok_or_else(|| {
                StoreError::Unavailable("敏感缓存不可用：没有密钥存储".to_string())
            })?;
            cipher
                .seal(value)
                .map_err(|err| StoreError::Unavailable(err.to_string()))?
        } else {
            value.to_vec()
        };
        // 首字节标记敏感与否：读取时据此决定要不要解密
        let mut payload = Vec::with_capacity(body.len() + 1);
        payload.push(u8::from(sensitive));
        payload.extend_from_slice(&body);
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
        let sensitive = raw.remove(0) != 0;
        if !sensitive {
            return Ok(Some(raw));
        }
        // 解不开（换机 / 清了 localStorage 的密钥）是错误，不是把密文当明文还
        let cipher = self
            .cipher
            .as_ref()
            .ok_or_else(|| StoreError::Unavailable("敏感缓存不可用：没有密钥存储".to_string()))?;
        let plain = cipher
            .open(&raw)
            .map_err(|err| StoreError::Corrupt(err.to_string()))?;
        Ok(Some(plain))
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
    /// 事件超过上限时丢最旧的几条：走 `by_seq` 索引按序取主键，
    /// 删头部即可——不需要把全部事件读回内存再排序。
    ///
    /// 注意 IndexedDB 的事务会在"控制权回到事件循环且没有新请求"时自动
    /// 提交：`get_all_keys` 与后续 `delete` 必须落在**同一个事务**里，中间
    /// 只能经由 promise 的微任务续体（`request_promise` 在 onsuccess 里
    /// resolve，续体仍在同一轮任务里发出 delete——与 js 侧 `idb` 库同一套
    /// 约定），跨 `await` 再开新事务是删不动已提交事务的。
    async fn prune_events(&self) -> Result<(), StoreError> {
        let object = self.store(STORE_EVENTS, IdbTransactionMode::Readwrite)?;
        let index = object.index(INDEX_BY_SEQ).map_err(|err| dom_error(&err))?;
        let keys_request = index.get_all_keys().map_err(|err| dom_error(&err))?;
        let keys = JsFuture::from(request_promise(&keys_request))
            .await
            .map_err(|err| dom_error(&err))?;
        let all = js_sys::Array::from(&keys);
        let total = all.length() as usize;
        if total <= MAX_EVENTS {
            return Ok(());
        }
        // 索引序（补零 seq 字符串序）= 旧→新；删掉头部多余的主键
        let excess = total - MAX_EVENTS;
        for key in all.iter().take(excess) {
            let request = object.delete(&key).map_err(|err| dom_error(&err))?;
            JsFuture::from(request_promise(&request))
                .await
                .map_err(|err| dom_error(&err))?;
        }
        Ok(())
    }
}

impl IdbStore {
    /// 记下"已经降级"的原因，供界面提示（例如"当前浏览模式不能离线保存"）。
    ///
    /// `try_borrow_mut`：重入（正在读降级原因的调用栈里又写）时丢弃这条
    /// 记录而不是 panic——降级提示是尽力而为的信息，不是不变量。
    pub fn note_degraded(&self, error: StoreError) {
        if let Ok(mut slot) = self.degraded.try_borrow_mut() {
            *slot = Some(error);
        }
    }
}

/// 事件值 → JSON 文本。v2 起值是 JSON 对象（带 `_seq` 排序字段），
/// 读回时 stringify；防御性地兼容字符串值（万一有旧数据残留）。
fn value_as_event_text(value: &JsValue) -> Option<String> {
    if let Some(text) = value.as_string() {
        return Some(text);
    }
    let stringified = js_sys::JSON::stringify(value).ok()?;
    stringified.as_string()
}
