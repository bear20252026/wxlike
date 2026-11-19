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
//! * `dispatch`：投递治理事件。平台真实够得着的是群成员缓存
//!   （`CacheClear{scope:"group"}`）；会话撤销在域内已删表完成，设备密钥与
//!   WS 泵的进程内联动目前只留痕——接入点在 `server` 的 hub/密钥面，见
//!   IR 的 `notes`。
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

/// 平台联动端口的连接与缓存句柄。由 [`crate::Platform::open_with`] 装配。
pub(crate) struct GovernanceLinks {
    /// 独立连接（文件库才有）。`None` = 内存库，镜像与跨域计数不可达。
    sidecar: Option<Mutex<Connection>>,
    /// 与 `Platform::group_cache` 共享的同一把缓存（Arc 化后的字段本身）。
    group_cache: Arc<Mutex<HashMap<String, Vec<String>>>>,
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
        })
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
        guard
            .execute(
                "UPDATE users SET suspended = ?2, disabled = ?3, suspended_until_ms = ?4 \
                 WHERE id = ?1",
                rusqlite::params![user_id, suspended as i64, disabled as i64, until_ms],
            )
            .map_err(wxlike_identity::IdError::Db)?;
        Ok(())
    }

    /// 进程内即时效果：平台真实够得着的是群成员缓存；其余落日志留痕
    /// （会话撤销在域内删表完成，WS/设备密钥接入点在 server 层）。
    fn dispatch(&self, events: &[GovernanceEvent]) {
        for event in events {
            match event {
                GovernanceEvent::CacheClear { scope, key } if scope == "group" => {
                    self.group_cache
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(key);
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
                    tracing::info!(%user_id, %count, "治理撤销会话（域内已删 sessions/刷新链/step-up）");
                }
                GovernanceEvent::DeviceRevoked { user_id, device_id } => {
                    // 设备密钥作废的接入点在 server/密钥面；这里留痕保证事件不丢。
                    tracing::info!(%user_id, %device_id, "设备被管理端撤销");
                }
            }
        }
    }
}

impl UserMetadataPort for GovernanceLinks {
    /// 参与的未解散群数（relations 域自有表，与平台同库）。
    fn group_count(&self, user_id: &str) -> wxlike_identity::Result<i64> {
        let Some(guard) = self.conn() else {
            return Err(wxlike_identity::IdError::Db(rusqlite::Error::InvalidQuery));
        };
        guard
            .query_row(
                "SELECT COUNT(DISTINCT m.group_id) FROM group_members m \
                 LEFT JOIN group_meta g ON g.group_id = m.group_id \
                 WHERE m.user_id = ?1 AND COALESCE(g.dissolved_at, 0) = 0",
                rusqlite::params![user_id],
                |r| r.get(0),
            )
            .map_err(wxlike_identity::IdError::Db)
    }

    /// 针对该用户的举报工单数（平台 `reports` 表，按 target_kind='user'）。
    fn report_count(&self, user_id: &str) -> wxlike_identity::Result<i64> {
        let Some(guard) = self.conn() else {
            return Err(wxlike_identity::IdError::Db(rusqlite::Error::InvalidQuery));
        };
        guard
            .query_row(
                "SELECT COUNT(*) FROM reports WHERE target_kind = 'user' AND target_id = ?1",
                rusqlite::params![user_id],
                |r| r.get(0),
            )
            .map_err(wxlike_identity::IdError::Db)
    }
}
