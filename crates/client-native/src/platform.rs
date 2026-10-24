//! 原生平台能力：单实例锁、生命周期、能力声明。
//!
//! **单实例锁用文件**（`create_new` 是原子的）：两个进程同时启动时，
//! 只有一个能创建成功。锁文件里写 pid 与时间戳，进程崩溃留下的陈旧锁由
//! [`STALE_AFTER_MS`] 兜底清理——这是"宁可多等一会儿也别永远打不开"的取舍，
//! 真正的解决方案是操作系统级的 advisory lock，那要引入 `fs2` 一类依赖，
//! 留给后续卡评估。
//!
//! 生命周期事件由宿主（Tauri 窗口事件 / Flutter 的 AppLifecycleState）
//! 通过 [`FileLockPlatform::notify_lifecycle`] 投进来，核心只消费。

use std::path::PathBuf;

use tokio::sync::broadcast;
use wxlike_client_core::ports::{
    InstanceLock, Lifecycle, Platform, PlatformCaps, PlatformError, Release,
};

/// 陈旧锁的判定阈值：超过这个时间没更新就认为持有者已经不在了。
pub const STALE_AFTER_MS: u64 = 10 * 60 * 1_000;

/// 生命周期广播的容量。核心只需要"最新状态"，积压没有意义。
const LIFECYCLE_BUFFER: usize = 8;

/// 基于锁文件的平台实现。
pub struct FileLockPlatform {
    caps: PlatformCaps,
    lock_path: PathBuf,
    lifecycle: broadcast::Sender<Lifecycle>,
    /// 固定一个接收端：每次 `next_lifecycle` 新建接收端会漏掉两次调用之间的事件。
    lifecycle_rx: tokio::sync::Mutex<broadcast::Receiver<Lifecycle>>,
}

impl FileLockPlatform {
    /// 构造。`lock_path` 通常放在应用数据目录里。
    #[must_use]
    pub fn new(lock_path: impl Into<PathBuf>) -> Self {
        let (lifecycle, receiver) = broadcast::channel(LIFECYCLE_BUFFER);
        Self {
            caps: PlatformCaps::native(),
            lock_path: lock_path.into(),
            lifecycle,
            lifecycle_rx: tokio::sync::Mutex::new(receiver),
        }
    }

    /// 覆盖能力声明（某些端没有推送 / 后台任务）。
    #[must_use]
    pub fn with_caps(mut self, caps: PlatformCaps) -> Self {
        self.caps = caps;
        self
    }

    /// 宿主投递生命周期事件。
    pub fn notify_lifecycle(&self, event: Lifecycle) {
        // 没有订阅者时发送失败是正常的（没人关心），不是错误
        let _ = self.lifecycle.send(event);
    }
}

/// 释放动作：删掉锁文件。
struct FileRelease {
    path: PathBuf,
}

impl Release for FileRelease {
    fn release(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 判断已有锁文件是否陈旧。
fn is_stale(path: &PathBuf) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    modified
        .elapsed()
        .map(|age| age.as_millis() > u128::from(STALE_AFTER_MS))
        .unwrap_or(false)
}

fn write_lock(path: &PathBuf) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let pid = std::process::id();
    write!(file, "{pid}\n")?;
    file.sync_all()
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Platform for FileLockPlatform {
    fn caps(&self) -> PlatformCaps {
        self.caps
    }

    async fn acquire_instance_lock(&self) -> Result<InstanceLock, PlatformError> {
        match write_lock(&self.lock_path) {
            Ok(()) => Ok(InstanceLock::new(Box::new(FileRelease {
                path: self.lock_path.clone(),
            }))),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if !is_stale(&self.lock_path) {
                    return Err(PlatformError::AlreadyRunning);
                }
                // 陈旧锁：清掉再抢一次。抢不到就如实报"已有实例"
                let _ = std::fs::remove_file(&self.lock_path);
                match write_lock(&self.lock_path) {
                    Ok(()) => Ok(InstanceLock::new(Box::new(FileRelease {
                        path: self.lock_path.clone(),
                    }))),
                    Err(_) => Err(PlatformError::AlreadyRunning),
                }
            }
            Err(err) => Err(PlatformError::Unsupported(format!("锁文件不可写：{err}"))),
        }
    }

    async fn next_lifecycle(&self) -> Option<Lifecycle> {
        let mut receiver = self.lifecycle_rx.lock().await;
        receiver.recv().await.ok()
    }
}
