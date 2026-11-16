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

use wxlike_client_core::ports::{Ports, Secrets};
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
    /// 实时订阅不可用（S4）：无长连接能力（caps.websocket=false），或订阅
    /// 建立失败。宿主应改用推送 + 拉取（`conv.sync`），而不是反复重试订阅。
    RealtimeUnavailable(String),
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NativeError::Session(err) => write!(f, "{err}"),
            NativeError::Secrets(why) => write!(f, "密钥存储不可用：{why}"),
            NativeError::Io(why) => write!(f, "数据目录不可用：{why}"),
            NativeError::RealtimeUnavailable(why) => {
                write!(f, "实时订阅不可用（请改用推送 + 拉取）：{why}")
            }
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
        let secrets = Arc::new(
            FileSecrets::open(config.secrets_path())
                .map_err(|err| NativeError::Secrets(err.to_string()))?,
        );
        Self::open_with_secrets(config, secrets).await
    }

    /// 打开，但由宿主注入密钥后端（D4 平台端口）。
    ///
    /// 桌面默认走 [`FileSecrets`]（见 [`NativeCore::open`]）；移动端必须把
    /// 密钥放进系统安全存储——iOS Keychain / Android Keystore——而不是明文
    /// 落盘。`Secrets` 在 `Ports`/`Session` 层本就是 `Arc<dyn Secrets>` 注入
    /// （已有 `MemSecrets`/`WebSecrets` 两个替代实现为证），这里把同样的缝隙
    /// 开到 `NativeCore` 装配层：平台特化的后端从外面传进来，核心逻辑不动。
    ///
    /// # Errors
    /// 见 [`NativeError`]。
    pub async fn open_with_secrets(
        config: NativeConfig,
        secrets: Arc<dyn Secrets>,
    ) -> Result<Self, NativeError> {
        let platform = Arc::new(FileLockPlatform::new(config.lock_path()));
        Self::open_with_platform(config, secrets, platform).await
    }

    /// 打开，且由宿主注入密钥后端**与平台**（S4 能力驱动装配）。
    ///
    /// 平台决定 `caps()`（是否有长连接 / 安全存储 / 后台任务）。实时订阅
    /// [`Self::subscribe_events`] 读 `caps().websocket` 决定是否可用——所以
    /// 注入一个 `minimal()` caps 的平台就能让订阅走"无能力拒绝"路径。这是
    /// 能力从"只写声明"变成"驱动装配"的接缝。
    ///
    /// # Errors
    /// 见 [`NativeError`]。
    pub async fn open_with_platform(
        config: NativeConfig,
        secrets: Arc<dyn Secrets>,
        platform: Arc<FileLockPlatform>,
    ) -> Result<Self, NativeError> {
        std::fs::create_dir_all(&config.data_dir)
            .map_err(|err| NativeError::Io(err.to_string()))?;
        let store = Arc::new(
            SqliteStore::open(config.store_path())
                .map_err(|err| NativeError::Session(SessionError::Store(err)))?,
        );
        let notifications = Arc::new(ChannelNotifications::new());
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

    /// 订阅实时事件流（S4 / ADR-29），**由平台能力驱动**。
    ///
    /// 只有 `caps().websocket` 为真（桌面 / 有长连接的原生端）才提供实时流；
    /// 移动端若没有长连接能力，返回 [`NativeError::RealtimeUnavailable`]，
    /// 宿主据此改用推送 + 拉取（`conv.sync`），而不是反复重试订阅。
    ///
    /// 能力此前是"只写不读"的声明（仅 Debug 用到）；这里让它**真正驱动装配**，
    /// 闭合审计根因 2（传输能力未契约化、各宿主重写实时）。
    ///
    /// # Errors
    /// 无实时能力 → `RealtimeUnavailable`；订阅建立失败 → `Session`。
    pub async fn subscribe_events(
        &self,
        request: wxlike_types::SubscribeRequest,
    ) -> Result<wxlike_client_core::runtime::EventSubscription, NativeError> {
        use wxlike_client_core::ports::Platform as _;
        if !self.platform.caps().websocket {
            return Err(NativeError::RealtimeUnavailable(
                "本平台无实时长连接能力（caps.websocket=false）".to_string(),
            ));
        }
        self.session
            .subscribe(request)
            .await
            .map_err(|err| NativeError::RealtimeUnavailable(err.to_string()))
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
