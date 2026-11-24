//! G02-09 账户治理的平台联动端口（集成请求 REQ-G00-governance-session-linkage）。
//!
//! identity/relations 域只发事件与回执，进程内效果由平台执行（IR 的 rationale：
//! 绕过平台改库会留下"后台封了、服务还通"的静默不一致）。本模块是那两个
//! 端口的**唯一生产实现**，装配在 [`crate::Platform`] 上：
//!
//! * `mirror_user_state`：把治理状态镜像到平台 `users` 表补列
//!   （`suspended/disabled/suspended_until_ms`，迁移见 `db.rs`）。登录/签发
//!   路径的 [`wxlike_identity::governance::ensure_user_active`] 读的是域自有表，
//!   镜像列供平台侧运维/报表查询，两侧以域表为准、镜像向域收敛。
//! * `dispatch`：投递治理事件。`CacheClear{scope:"group"}` 真清群成员缓存；
//!   `SessionsRevoked`/`DeviceRevoked` 通过 [`GovernanceDeviceSink`] 断掉进程内
//!   的 WS/QUIC 泵（句柄由 `server` 装配时注入，见 [`crate::Platform::set_governance_sink`]）；
//!   未注入句柄时（测试/嵌入式/无 hub 进程）落日志留痕，事件不丢。
//! * `UserMetadataPort`：管理员元数据视图的跨域计数（群数来自 relations
//!   表、举报数来自平台 `reports` 表）。
//!
//! # 为什么端口自己开第二条连接（ADR-28 相邻的锁纪律）
//!
//! handler 把整段同步域调用包进 `spawn_blocking`，闭包里**持有** `Db` 的
//! 连接锁；端口在域函数内部被回调，若再取同一把锁立即死锁（std Mutex 不可
//! 重入）。所以文件库的联动走**第二条独立连接**（WAL + busy_timeout，与主
//! 库同文件）；内存库没有第二条连接可开——联动显式跳过并落日志，内存平台
//! 是测试/单进程演示形态，治理镜像本就没有跨进程读者。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;
use tracing::warn;

use wxlike_identity::governance::{GovernanceEvent, PlatformGovernancePort, UserMetadataPort};

use super::repo::governance as governance_repo;

/// 设备侧进程内联动端口（G02-09，**G00 集成面**）。
///
/// 域只发 [`GovernanceEvent`]，断连动作落在这里。`dispatch` 是**同步**的
/// （identity 域函数是纯同步 API，由 handler 包进 `spawn_blocking`），而
/// hub 侧的 `HubHandle::disconnect_user` 是 async——实现方负责桥接
/// （[`ChannelGovernanceSink`] 是现成方案）。本 trait 只承诺"投递出去"，
/// 不承诺动作已完成：这与域侧"事件不丢"的留痕纪律一致（宁可晚一点断，
/// 不能断错人）。
pub trait GovernanceDeviceSink: Send + Sync {
    /// 断开该用户在本节点上的**全部**连接（封禁 / 撤销会话）。
    fn disconnect_user(&self, user_id: &str);

    /// 只断开该设备上的连接（设备撤销）。返回是否由本实现接管了这条命令
    /// （用于平台侧决定要不要额外留痕）；不代表 hub 那边连接真的存在。
    fn disconnect_device(&self, user_id: &str, device_id: &str) -> bool;
}

/// [`GovernanceDeviceSink`] 的进程内通道实现：把同步 `dispatch` 里的断连
/// 命令丢给一个后台任务去 await（hub 断连是 async）。
///
/// 这是给装配面（未来的 `server`）的**现成桥**：
///
/// ```ignore
/// let (sink, mut cmds) = ChannelGovernanceSink::new();
/// platform.set_governance_sink(Arc::new(sink));
/// tokio::spawn(async move {
///     while let Some(cmd) = cmds.recv().await {
///         match cmd {
///             GovernanceDisconnect::User(u) => hub.disconnect_user(&u).await,
///             GovernanceDisconnect::Device { user_id, device_id } => {
///                 hub.disconnect_device(&user_id, &device_id).await;
///             }
///         };
///     }
/// });
/// ```
///
/// 通道无界：断连命令极低频（人工治理动作），而阻塞 `dispatch` 会卡住整个
/// 域事务，丢命令则会让"后台封了、前端还收消息"。
pub struct ChannelGovernanceSink {
    tx: tokio::sync::mpsc::UnboundedSender<GovernanceDisconnect>,
}

/// 一条待执行的断连命令（同步 `dispatch` 与 async hub 之间的桥）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GovernanceDisconnect {
    User(String),
    Device { user_id: String, device_id: String },
}

impl ChannelGovernanceSink {
    /// 返回 sink 与命令接收端；接收端交给后台任务，sink 交给平台。
    pub fn new() -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<GovernanceDisconnect>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { tx }, rx)
    }
}

impl Default for ChannelGovernanceSink {
    fn default() -> Self {
        Self::new().0
    }
}

impl GovernanceDeviceSink for ChannelGovernanceSink {
    fn disconnect_user(&self, user_id: &str) {
        let _ = self
            .tx
            .send(GovernanceDisconnect::User(user_id.to_string()));
    }

    fn disconnect_device(&self, user_id: &str, device_id: &str) -> bool {
        let _ = self.tx.send(GovernanceDisconnect::Device {
            user_id: user_id.to_string(),
            device_id: device_id.to_string(),
        });
        // 通道实现只知道"投递成功"，不知道 hub 那边还有没有这条连接；
        // 真正的存在性由 [`crate::Platform`] 之外的 hub 断言。
        true
    }
}

/// 平台联动端口的连接与缓存句柄。由 [`crate::Platform::open_with`] 装配。
pub(crate) struct GovernanceLinks {
    /// 独立连接（文件库才有）。`None` = 内存库，镜像与跨域计数不可达。
    sidecar: Option<Mutex<Connection>>,
    /// 与 `Platform::group_cache` 共享的同一把缓存（Arc 化后的字段本身）。
    group_cache: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// 设备侧联动句柄（断 WS/QUIC 泵）。`None` = 本进程没跑 hub（测试 /
    /// 嵌入式 / 未装配），断连事件只留痕。
    device_sink: Mutex<Option<Arc<dyn GovernanceDeviceSink>>>,
}

impl GovernanceLinks {
    /// 文件库开第二条连接（同文件、同 PRAGMA）；内存库显式无连接。
    pub(crate) fn open(
        path: Option<&str>,
        group_cache: Arc<Mutex<HashMap<String, Vec<String>>>>,
    ) -> crate::error::Result<Self> {
        let sidecar = match path {
            Some(p) => {
                let conn = Connection::open(p)?;
                conn.execute_batch(
                    "PRAGMA journal_mode = WAL;
                     PRAGMA foreign_keys = ON;
                     PRAGMA busy_timeout = 5000;",
                )?;
                Some(Mutex::new(conn))
            }
            None => None,
        };
        Ok(Self {
            sidecar,
            group_cache,
            device_sink: Mutex::new(None),
        })
    }

    pub(crate) fn set_device_sink(&self, sink: Option<Arc<dyn GovernanceDeviceSink>>) {
        *self
            .device_sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = sink;
    }

    fn device_sink(&self) -> Option<Arc<dyn GovernanceDeviceSink>> {
        self.device_sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn conn(&self) -> Option<MutexGuard<'_, Connection>> {
        self.sidecar
            .as_ref()
            .map(|m| m.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl PlatformGovernancePort for GovernanceLinks {
    /// 治理状态镜像到 `users` 补列。域表（`identity_user_governance`）是
    /// 权威；镜像失败不影响命令本身（返回 Err 让域留痕）。
    fn mirror_user_state(
        &self,
        user_id: &str,
        suspended: bool,
        disabled: bool,
        until_ms: Option<i64>,
    ) -> wxlike_identity::Result<()> {
        let Some(guard) = self.conn() else {
            warn!(%user_id, "内存平台无第二条连接，治理镜像跳过（域表仍为权威）");
            return Ok(());
        };
        let port = crate::storage::StorageConnection::sqlite_borrow(&guard);
        governance_repo::mirror_user_state(&port, user_id, suspended, disabled, until_ms)
            .map_err(wxlike_identity::IdError::Db)?;
        Ok(())
    }

    /// 进程内即时效果：群成员缓存清理 + 设备侧断连（[`GovernanceDeviceSink`]）。
    /// 未注入 sink 时退化为日志留痕，保证事件不丢。
    fn dispatch(&self, events: &[GovernanceEvent]) {
        for event in events {
            self.apply(event);
        }
    }
}

impl GovernanceLinks {
    /// 单个联动事件的效果。拆出来是为了让 `dispatch` 只保留“遍历”，
    /// 每个事件自己的分支各自成段，不再叠成五层嵌套。
    fn apply(&self, event: &GovernanceEvent) {
        match event {
            GovernanceEvent::CacheClear { scope, key } if scope == "group" => {
                self.clear_group_cache(key);
            }
            GovernanceEvent::CacheClear { scope, key } => {
                tracing::debug!(%scope, %key, "治理缓存清理事件（平台无对应缓存，留痕）");
            }
            GovernanceEvent::UserStateChanged {
                user_id,
                state,
                suspended_until_ms,
            } => {
                tracing::info!(%user_id, %state, ?suspended_until_ms, "用户治理状态变化");
            }
            GovernanceEvent::SessionsRevoked { user_id, count } => {
                self.revoke_sessions(user_id, *count);
            }
            GovernanceEvent::DeviceRevoked { user_id, device_id } => {
                self.revoke_device(user_id, device_id);
            }
        }
    }

    fn clear_group_cache(&self, key: &str) {
        self.group_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
    }

    fn revoke_sessions(&self, user_id: &str, count: u64) {
        // 域内已删 sessions/刷新链/step-up；这里把还连着的 WS/QUIC 泵拔掉，
        // 否则"后台封了、前端还收消息"。count == 0 = 幂等重放，不断。
        if count == 0 {
            tracing::info!(%user_id, %count, "治理撤销会话（域内已删 sessions/刷新链/step-up）");
            return;
        }
        match self.device_sink() {
            Some(sink) => sink.disconnect_user(user_id),
            None => tracing::info!(
                %user_id,
                %count,
                "治理撤销会话（本进程未装配 hub 句柄，断连留痕）"
            ),
        }
    }

    fn revoke_device(&self, user_id: &str, device_id: &str) {
        let done = self
            .device_sink()
            .is_some_and(|sink| sink.disconnect_device(user_id, device_id));
        if !done {
            tracing::info!(%user_id, %device_id, "设备被管理端撤销（无 hub 句柄或连接已不在，留痕）");
        }
    }
}

impl UserMetadataPort for GovernanceLinks {
    /// 参与的未解散群数（relations 域自有表，与平台同库）。
    fn group_count(&self, user_id: &str) -> wxlike_identity::Result<i64> {
        let Some(guard) = self.conn() else {
            return Err(wxlike_identity::IdError::Db(rusqlite::Error::InvalidQuery));
        };
        let port = crate::storage::StorageConnection::sqlite_borrow(&guard);
        governance_repo::group_count(&port, user_id).map_err(wxlike_identity::IdError::Db)
    }

    /// 针对该用户的举报工单数（平台 `reports` 表，按 target_kind='user'）。
    fn report_count(&self, user_id: &str) -> wxlike_identity::Result<i64> {
        let Some(guard) = self.conn() else {
            return Err(wxlike_identity::IdError::Db(rusqlite::Error::InvalidQuery));
        };
        let port = crate::storage::StorageConnection::sqlite_borrow(&guard);
        governance_repo::report_count(&port, user_id).map_err(wxlike_identity::IdError::Db)
    }
}
