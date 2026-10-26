//! 原生核心的装配层：把端口、会话、补发泵拼起来。
//!
//! 宿主（Tauri / Flutter FFI）只需要一个 [`NativeCore`]：
//!
//! ```text
//!   open(config) ──> 抢单实例锁 + 打开 SQLite + 取缓存密钥
//!   dispatch(req) ──> 读写命令（写命令先落盘）
//!   start_pump()  ──> 后台按节拍补发（退出时 abort）
//!   put/get_cache ──> 敏感内容加密后落盘
//!   shutdown()    ──> 取消在飞任务 + 释放锁
//! ```
//!
//! 敏感缓存的键约定：明文缓存用 `p:<key>`、加密缓存用 `s:<key>`。
//! 键前缀放在这一层而不是存储层：存储层只管字节，不该知道"这条要不要解密"。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use wxlike_client_core::ports::Ports;
use wxlike_client_core::runtime::{PumpReport, Session, SessionError};
use wxlike_types::{ActionRequest, ActionResult, DomainError};

use crate::clock::SystemClock;
use crate::notify::ChannelNotifications;
use crate::platform::FileLockPlatform;
use crate::secrets::{CacheCipher, FileSecrets};
use crate::store::SqliteStore;
use crate::transport::HttpTransport;

/// 打开核心需要的配置。
#[derive(Clone, Debug)]
pub struct NativeConfig {
    /// 服务端 base URL（`http://host:port`）。
    pub base_url: String,
    /// 应用数据目录（库文件、锁文件、密钥文件都在这里）。
    pub data_dir: PathBuf,
}

impl NativeConfig {
    /// 构造。
    #[must_use]
    pub fn new(base_url: impl Into<String>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_url: base_url.into(),
            data_dir: data_dir.into(),
        }
    }

    fn store_path(&self) -> PathBuf {
        self.data_dir.join("client.db")
    }

    fn lock_path(&self) -> PathBuf {
        self.data_dir.join("client.lock")
    }

    fn secrets_path(&self) -> PathBuf {
        self.data_dir.join("secrets.tsv")
    }
}

/// 打开失败。
#[derive(Debug)]
pub enum NativeError {
    /// 会话层失败（已有实例、本地库不可用）。
    Session(SessionError),
    /// 密钥存储失败。
    Secrets(String),
    /// 目录建不出来。
    Io(String),
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NativeError::Session(err) => write!(f, "{err}"),
            NativeError::Secrets(why) => write!(f, "密钥存储不可用：{why}"),
            NativeError::Io(why) => write!(f, "数据目录不可用：{why}"),
        }
    }
}

impl std::error::Error for NativeError {}

/// 原生客户端核心。
pub struct NativeCore {
    session: Arc<Session>,
    store: Arc<SqliteStore>,
    transport: Arc<HttpTransport>,
    platform: Arc<FileLockPlatform>,
    notifications: Arc<ChannelNotifications>,
    cipher: CacheCipher,
    pump: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl NativeCore {
    /// 打开：建目录 → 抢锁 → 开库 → 取密钥 → 开会话。
    ///
    /// # Errors
    /// 见 [`NativeError`]。
    pub async fn open(config: NativeConfig) -> Result<Self, NativeError> {
        std::fs::create_dir_all(&config.data_dir)
            .map_err(|err| NativeError::Io(err.to_string()))?;
        let store = Arc::new(
            SqliteStore::open(config.store_path())
                .map_err(|err| NativeError::Session(SessionError::Store(err)))?,
        );
        let platform = Arc::new(FileLockPlatform::new(config.lock_path()));
        let notifications = Arc::new(ChannelNotifications::new());
        let secrets = Arc::new(
            FileSecrets::open(config.secrets_path())
                .map_err(|err| NativeError::Secrets(err.to_string()))?,
        );
        let cipher = CacheCipher::load_or_create(secrets.as_ref())
            .await
            .map_err(|err| NativeError::Secrets(err.to_string()))?;
        let transport = Arc::new(HttpTransport::new(config.base_url));
        let ports = Ports::new(
            transport.clone(),
            store.clone(),
            Arc::new(SystemClock::new()),
            secrets,
            platform.clone(),
            notifications.clone(),
        );
        let session = Session::open(ports).await.map_err(NativeError::Session)?;
        Ok(Self {
            session: Arc::new(session),
            store,
            transport,
            platform,
            notifications,
            cipher,
            pump: Mutex::new(None),
        })
    }

    /// 登录后换 token。
    ///
    /// 曾经这里是静默 no-op：`Arc::get_mut` 拿不到唯一引用（`Ports` 还持着
    /// 另一个 `Arc`），token 根本没换、请求继续带旧的。现在 token 在
    /// `HttpTransport` 的共享槽里，`&self` 即换、立即生效。
    pub fn set_token(&self, token: Option<String>) {
        self.transport.set_token(token);
    }

    /// 执行一条命令。
    pub async fn dispatch(&self, request: ActionRequest) -> ActionResult {
        self.session.dispatch(request).await
    }

    /// 立刻补发一轮。
    ///
    /// # Errors
    /// 读本地队列失败。
    pub async fn pump_now(&self, limit: u32) -> Result<PumpReport, DomainError> {
        self.session.pump(limit).await
    }

    /// 起一个后台补发任务。重复调用不会起第二个。
    pub fn start_pump(&self, interval_ms: u64) {
        let mut slot = self.pump.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return;
        }
        let session = self.session.clone();
        let handle = tokio::spawn(async move {
            loop {
                if session.cancellation().is_cancelled() {
                    break;
                }
                let _ = session.pump(BATCH).await;
                tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
            }
        });
        *slot = Some(handle);
    }

    /// 写缓存。`sensitive = true` 时先加密再落盘。
    ///
    /// # Errors
    /// 加密失败或本地库不可用。
    pub async fn put_cache(
        &self,
        key: &str,
        plaintext: &[u8],
        sensitive: bool,
    ) -> Result<(), DomainError> {
        use wxlike_client_core::ports::LocalStore as _;
        if !sensitive {
            return self
                .store
                .put_cache(&format!("p:{key}"), plaintext, false)
                .await
                .map_err(|err| DomainError::unavailable(err.to_string()));
        }
        let sealed = self
            .cipher
            .seal(plaintext)
            .map_err(|err| DomainError::internal(err.to_string()))?;
        self.store
            .put_cache(&format!("s:{key}"), &sealed, true)
            .await
            .map_err(|err| DomainError::unavailable(err.to_string()))
    }

    /// 读缓存。加密过的会自动解密。
    ///
    /// # Errors
    /// 本地库不可用；密文解不开（换机/重装后应清缓存重拉）。
    pub async fn get_cache(&self, key: &str) -> Result<Option<Vec<u8>>, DomainError> {
        use wxlike_client_core::ports::LocalStore as _;
        if let Some(sealed) = self
            .store
            .get_cache(&format!("s:{key}"))
            .await
            .map_err(|err| DomainError::unavailable(err.to_string()))?
        {
            let plain = self
                .cipher
                .open(&sealed)
                .map_err(|err| DomainError::internal(err.to_string()))?;
            return Ok(Some(plain));
        }
        self.store
            .get_cache(&format!("p:{key}"))
            .await
            .map_err(|err| DomainError::unavailable(err.to_string()))
    }

    /// 宿主切换前台/后台（决定要不要弹通知）。
    pub fn set_foreground(&self, foreground: bool) {
        self.notifications.set_foreground(foreground);
    }

    /// 宿主取通知通道。
    #[must_use]
    pub fn notifications(&self) -> Arc<ChannelNotifications> {
        self.notifications.clone()
    }

    /// 宿主投递生命周期事件。
    pub fn notify_lifecycle(&self, event: wxlike_client_core::ports::Lifecycle) {
        self.platform.notify_lifecycle(event);
    }

    /// 本地待发条数（界面角标）。
    ///
    /// # Errors
    /// 本地库不可用。
    pub async fn pending(&self) -> Result<usize, DomainError> {
        use wxlike_client_core::ports::LocalStore as _;
        let clock = SystemClock::new();
        let now = wxlike_client_core::ports::Clock::now_ms(&clock);
        let rows = self
            .store
            .due_outbox(now, u32::MAX)
            .await
            .map_err(|err| DomainError::unavailable(err.to_string()))?;
        Ok(rows.len())
    }

    /// 退出：停补发、取消订阅、释放锁。幂等。
    pub fn shutdown(&self) {
        if let Some(handle) = self.pump.lock().unwrap_or_else(|e| e.into_inner()).take() {
            handle.abort();
        }
        self.session.shutdown();
    }
}

impl Drop for NativeCore {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 一轮补发最多处理几条。
const BATCH: u32 = 20;

/// 数据目录下的默认文件名（宿主拼路径时用）。
#[must_use]
pub fn default_data_dir(base: impl AsRef<Path>) -> PathBuf {
    base.as_ref().join("wxlike")
}
