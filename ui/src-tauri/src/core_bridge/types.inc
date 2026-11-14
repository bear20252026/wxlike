
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tauri::Manager as _;

use super::{AppState, Http, LocalStore};

/// 新 UI 的请求信封（core-contract.ts: ActionRequest）。
#[derive(Debug, Deserialize)]
pub struct CoreRequest {
    pub action_id: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub params: Value,
}

/// 新 UI 的响应信封（core-contract.ts: ActionResponse）。
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub enum CoreReply {
    Ok { ok: bool, data: Value },
    Err { ok: bool, error: CoreError },
}

#[derive(Debug, serde::Serialize)]
pub struct CoreError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    pub request_id: String,
}

/// 桥内错误：显式带契约错误码；AppState 的 `String` 错误经
/// [`BridgeError::from_string`] 分类（分类表由 e2e 测试兜底防漂移）。
#[derive(Debug)]
enum BridgeError {
    Unauth(String),
    /// 服务端认得这个人，但这件事他不许做（403）。与 401 分开是契约要求的：
    /// 401 要重新登录，403 重新登录也没用（`core-contract.ts` 的 `ErrorCode`
    /// 本来就有 `FORBIDDEN` 这一档，桥一直没用过它）。
    Forbidden(String),
    Validation(String),
    Conflict(String),
    Confirmation(String),
    /// 429：限流。与 `OFFLINE` 同为可重试，但原因不同（服务端在挡，不是链路断）。
    RateLimited(String),
    NotImpl(String),
    Dependency(String),
    Offline(String),
    Other(String),
}

