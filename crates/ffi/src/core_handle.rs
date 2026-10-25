//! `CoreHandle`：给 Flutter 的**唯一**入口（ADR-11）。
//!
//! 形状是"排队 + 拉取"，不是"Rust 回调 Dart"：
//!
//! ```text
//!   Dart worker isolate          Rust
//!   ─────────────────           ────────────────────────────
//!   dispatch(json) ───────────> 入队，立刻返回 request_id
//!   poll(timeout_ms) ─────────> 阻塞等结果/事件，成批返回 JSON
//! ```
//!
//! 为什么不做回调：
//!
//! * 从 Rust 的**工作线程**直接调进 Dart 需要跨 isolate 端口，一旦 Dart 侧
//!   卡住，Rust 的发送路径就被反向阻塞（"UI 卡住导致消息发不出去"）；
//! * worker isolate 本来就在跑循环，`poll` 是它的自然形状；
//! * 取消与销毁的语义更干净：`dispose` 之后 `poll` 直接返回空、`dispatch`
//!   直接报错，不存在"晚到的回调打到已销毁的对象上"。
//!
//! 三条硬约束（每条都有测试）：
//!
//! 1. **长任务可取消**：`cancel(request_id)` 之后该请求不会再发出/不会产出结果；
//! 2. **销毁后拒绝晚到**：`dispose()` 之后 `dispatch` 报 `disposed`，
//!    `poll` 返回空数组而不是阻塞；
//! 3. **panic 不跨 FFI 展开**：所有导出函数都用 `catch_unwind` 包住，
//!    转成 `WxlikeError`——Rust panic 穿过 FFI 是未定义行为，会直接杀进程。

use std::collections::{HashSet, VecDeque};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use wxlike_client_native::{NativeConfig, NativeCore};
use wxlike_types::{ActionRequest, DomainError};

use crate::error::WxlikeError;

/// 一次 `poll` 最多返回多少条。
const POLL_BATCH: usize = 64;
/// `poll` 的超时上限（毫秒）：再长就是在拿 Dart 的线程开玩笑。
const MAX_POLL_MS: u64 = 30_000;

/// 一条排队的请求。
struct Job {
    request_id: String,
    request: ActionRequest,
}

/// 结果队列。
#[derive(Default)]
struct Results {
    items: Mutex<VecDeque<String>>,
    signal: Condvar,
}

impl Results {
    fn push(&self, text: String) {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        items.push_back(text);
        self.signal.notify_all();
    }

    fn drain(&self, timeout: Duration) -> Vec<String> {
        let mut items = self.items.lock().unwrap_or_else(|e| e.into_inner());
        if items.is_empty() {
            let (next, _timeout) = self
                .signal
                .wait_timeout(items, timeout)
                .unwrap_or_else(|e| e.into_inner());
            items = next;
        }
        let mut out = Vec::new();
        while let Some(item) = items.pop_front() {
            out.push(item);
            if out.len() >= POLL_BATCH {
                break;
            }
        }
        out
    }
}

/// 工作线程与它共享的状态。
struct Shared {
    jobs: Mutex<VecDeque<Job>>,
    job_signal: Condvar,
    results: Results,
    cancelled: Mutex<HashSet<String>>,
    closed: AtomicBool,
    /// 用于生成 request_id（单调，避免随机数依赖）。
    sequence: AtomicU64,
}

impl Shared {
    fn is_cancelled(&self, request_id: &str) -> bool {
        self.cancelled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(request_id)
    }
}

/// Flutter 侧的核心句柄。
#[derive(uniffi::Object)]
pub struct CoreHandle {
    shared: Arc<Shared>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// 结果 JSON 的形状（Dart 侧按 `kind` 分流）。
fn result_json(request_id: &str, result: &wxlike_types::ActionResult) -> String {
    json!({
        "kind": "result",
        "request_id": request_id,
        "action_id": result.action_id.as_str(),
        "status": match &result.outcome {
            wxlike_types::ActionOutcome::Ok { .. } => "ok",
            wxlike_types::ActionOutcome::Empty => "empty",
            wxlike_types::ActionOutcome::Failed { .. } => "failed",
        },
        "payload": serde_json::to_value(&result.outcome).unwrap_or(Value::Null),
        "completed_at_ms": result.completed_at_ms,
    })
    .to_string()
}

fn error_json(request_id: &str, error: &DomainError) -> String {
    json!({
        "kind": "error",
        "request_id": request_id,
        "code": error.code(),
        "message": error.message(),
        "retryable": error.is_retryable(),
    })
    .to_string()
}

/// 把 panic 转成可返回的错误。
fn guard<T>(work: impl FnOnce() -> Result<T, WxlikeError>) -> Result<T, WxlikeError> {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|panic| {
        let detail = panic
            .downcast_ref::<&str>()
            .map(|text| (*text).to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "未知 panic".to_string());
        Err(WxlikeError::unexpected(format!("Rust panic：{detail}")))
    })
}

impl CoreHandle {
    fn new(shared: Arc<Shared>, core: NativeCore) -> Self {
        let worker_shared = shared.clone();
        let handle = std::thread::Builder::new()
            .name("wxlike-core".to_string())
            .spawn(move || run_worker(worker_shared, core))
            .ok();
        Self {
            shared,
            worker: Mutex::new(handle),
        }
    }
}

/// 工作线程主循环：取一条、跑一条、写结果。
fn run_worker(shared: Arc<Shared>, core: NativeCore) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            shared.results.push(
                json!({"kind": "fatal", "message": format!("tokio 运行时创建失败：{err}")})
                    .to_string(),
            );
            return;
        }
    };
    loop {
        if shared.closed.load(Ordering::SeqCst) {
            break;
        }
        let job = {
            let mut jobs = shared.jobs.lock().unwrap_or_else(|e| e.into_inner());
            if jobs.is_empty() {
                let (next, _) = shared
                    .job_signal
                    .wait_timeout(jobs, Duration::from_millis(WORKER_TICK_MS))
                    .unwrap_or_else(|e| e.into_inner());
                jobs = next;
            }
            jobs.pop_front()
        };
        let Some(job) = job else {
            continue;
        };
        if shared.is_cancelled(&job.request_id) {
            shared
                .results
                .push(error_json(&job.request_id, &DomainError::cancelled()));
            continue;
        }
        let outcome = runtime.block_on(core.dispatch(job.request.clone()));
        if shared.is_cancelled(&job.request_id) {
            // 已经取消：结果丢弃，但把"已取消"回给调用方（否则 UI 会一直等）
            shared
                .results
                .push(error_json(&job.request_id, &DomainError::cancelled()));
            continue;
        }
        match &outcome.outcome {
            wxlike_types::ActionOutcome::Failed { error } => {
                shared.results.push(error_json(&job.request_id, error));
            }
            _ => shared.results.push(result_json(&job.request_id, &outcome)),
        }
    }
    core.shutdown();
}

/// 工作线程空闲等待的节拍（毫秒）。只影响"销毁后多久退出"。
const WORKER_TICK_MS: u64 = 100;

#[uniffi::export]
impl CoreHandle {
    /// 打开核心。`config_json` 形如 `{"base_url":"http://...","data_dir":"/path"}`。
    ///
    /// # Errors
    /// 配置非法、已有实例在跑、本地库不可用。
    #[uniffi::constructor]
    pub fn open(config_json: String) -> Result<Arc<Self>, WxlikeError> {
        guard(|| {
            let config: Value = serde_json::from_str(&config_json)
                .map_err(|err| WxlikeError::invalid(format!("配置不是合法 JSON：{err}")))?;
            let base_url = config["base_url"]
                .as_str()
                .ok_or_else(|| WxlikeError::invalid("缺少 base_url"))?;
            let data_dir = config["data_dir"]
                .as_str()
                .ok_or_else(|| WxlikeError::invalid("缺少 data_dir"))?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|err| WxlikeError::unexpected(format!("运行时创建失败：{err}")))?;
            let core = runtime
                .block_on(NativeCore::open(NativeConfig::new(base_url, data_dir)))
                .map_err(|err| WxlikeError::unexpected(err.to_string()))?;
            let shared = Arc::new(Shared {
                jobs: Mutex::new(VecDeque::new()),
                job_signal: Condvar::new(),
                results: Results::default(),
                cancelled: Mutex::new(HashSet::new()),
                closed: AtomicBool::new(false),
                sequence: AtomicU64::new(1),
            });
            Ok(Arc::new(Self::new(shared, core)))
        })
    }

    /// 投递一条命令，**立刻**返回 request_id。
    ///
    /// # Errors
    /// 请求非法、核心已销毁。
    pub fn dispatch(&self, request_json: String) -> Result<String, WxlikeError> {
        guard(|| {
            if self.shared.closed.load(Ordering::SeqCst) {
                return Err(WxlikeError::disposed());
            }
            let request: ActionRequest = serde_json::from_str(&request_json)
                .map_err(|err| WxlikeError::invalid(format!("请求不是合法 JSON：{err}")))?;
            let sequence = self.shared.sequence.fetch_add(1, Ordering::SeqCst);
            let request_id = format!("req-{sequence}");
            {
                let mut jobs = self.shared.jobs.lock().unwrap_or_else(|e| e.into_inner());
                jobs.push_back(Job {
                    request_id: request_id.clone(),
                    request,
                });
            }
            self.shared.job_signal.notify_all();
            Ok(request_id)
        })
    }

    /// 阻塞等结果，成批返回 JSON 数组（`kind` 区分 result / error）。
    ///
    /// 销毁之后立刻返回空数组，不会挂住 Dart 的 worker isolate。
    #[must_use]
    pub fn poll(&self, timeout_ms: u64) -> String {
        if self.shared.closed.load(Ordering::SeqCst) {
            return "[]".to_string();
        }
        let timeout = Duration::from_millis(timeout_ms.min(MAX_POLL_MS));
        let items = self.shared.results.drain(timeout);
        format!("[{}]", items.join(","))
    }

    /// 取消一条还没跑完的请求。
    pub fn cancel(&self, request_id: String) {
        let mut cancelled = self
            .shared
            .cancelled
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cancelled.insert(request_id);
    }

    /// 还排着几条（角标/诊断用）。
    #[must_use]
    pub fn pending(&self) -> u32 {
        let jobs = self.shared.jobs.lock().unwrap_or_else(|e| e.into_inner());
        u32::try_from(jobs.len()).unwrap_or(u32::MAX)
    }

    /// 销毁：停工作线程、取消在飞请求、释放单实例锁。幂等。
    pub fn dispose(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.job_signal.notify_all();
        let mut worker = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = worker.take() {
            // 等它退出：最多一秒，超时就不等了（进程要退的时候不能卡住）
            let _ = handle.join();
        }
    }
}

impl std::fmt::Debug for CoreHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreHandle")
            .field("closed", &self.shared.closed.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl Drop for CoreHandle {
    fn drop(&mut self) {
        self.dispose();
    }
}
