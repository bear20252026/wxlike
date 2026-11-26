//! G02-09 账户治理的管理端编排（从 `identity_admin` handler 收进来）。
//!
//! 这里只做三件事：拿一次连接、把 identity/relations 域函数跑完、把域错误
//! 映射成 HTTP。handler 从此不再接触 `Connection`，也不再把"鉴权 + 治理 +
//! 群缓存失效"拆在闭包里自己拼。
//!
//! 同步域 API 仍由 handler 包进 `spawn_blocking`（ADR-28）；本模块不引入
//! `await`，事务边界保持在一次连接守卫内。

use crate::domain::qr_login::id_err;
use crate::error::ApiError;
use crate::storage::sql_port::StorageSqlExec;
use crate::Platform;
use wxlike_identity::governance::{
    self, AdminUserView, GovernCommand, GovernEffect, PlatformGovernancePort,
};
use wxlike_relations::governance::{
    self as relations_governance, GroupGovernCommand, GroupGovernEffect, GroupGovernanceEvent,
};

impl Platform {
    /// 封禁：域内即时撤销会话，平台镜像/事件经 `GovernanceLinks` 端口。
    pub(crate) fn admin_suspend(
        &self,
        operator_token: &str,
        cmd: &GovernCommand,
    ) -> Result<GovernEffect, ApiError> {
        let now = wxlike_core::types::now_ms() as i64;
        let pending = self.db().with_tx(|c| {
            Ok(governance::suspend_tx(
                &StorageSqlExec(c),
                operator_token,
                cmd,
                now,
            )?)
        })?;
        pending.finish(&self.governance).map_err(id_err)
    }

    /// 解封（可逆，留痕）。
    pub(crate) fn admin_unsuspend(
        &self,
        operator_token: &str,
        cmd: &GovernCommand,
    ) -> Result<GovernEffect, ApiError> {
        let now = wxlike_core::types::now_ms() as i64;
        let pending = self.db().with_tx(|c| {
            Ok(governance::unsuspend_tx(
                &StorageSqlExec(c),
                operator_token,
                cmd,
                now,
            )?)
        })?;
        pending.finish(&self.governance).map_err(id_err)
    }

    /// 撤销设备：同事务作废该设备的会话/刷新链/step-up。
    pub(crate) fn admin_revoke_device(
        &self,
        operator_token: &str,
        cmd: &GovernCommand,
    ) -> Result<GovernEffect, ApiError> {
        let now = wxlike_core::types::now_ms() as i64;
        let effect = self.db().with_tx(|c| {
            Ok(governance::revoke_device_tx(
                &StorageSqlExec(c),
                operator_token,
                cmd,
                now,
            )?)
        })?;
        // 事件必须在事务提交后派发：提交失败时不能先告诉平台"设备已下线"。
        self.governance.dispatch(&effect.events);
        Ok(effect)
    }

    /// 管理员元数据视图（白名单投影）；群数/举报数经 `UserMetadataPort`。
    pub(crate) fn admin_user_view(
        &self,
        operator_token: &str,
        user_id: &str,
    ) -> Result<AdminUserView, ApiError> {
        let c = self.db().acquire();
        governance::user_view_with(
            &StorageSqlExec(&c),
            operator_token,
            user_id,
            &self.governance,
            wxlike_core::types::now_ms() as i64,
        )
        .map_err(id_err)
    }

    /// 群治理：先做 Admin audience 鉴权（relations 域函数不鉴权），再执行
    /// freeze/unfreeze/transfer/dissolve，并把群成员缓存就地失效。
    pub(crate) fn admin_govern_group(
        &self,
        operator_token: &str,
        cmd: &GroupGovernCommand,
    ) -> Result<GroupGovernEffect, ApiError> {
        let now = wxlike_core::types::now_ms() as i64;
        // 鉴权与群治理共用同一个事务：令牌在事务里校验，治理写入失败时一并回滚。
        let effect = self.db().with_tx(|c| {
            let exec = StorageSqlExec(c);
            wxlike_identity::session::authenticate_with(
                &exec,
                operator_token,
                wxlike_identity::Audience::Admin,
                now,
            )?;
            Ok(relations_governance::govern_with(&exec, cmd, now)?)
        })?;
        for event in &effect.events {
            if let GroupGovernanceEvent::CacheClear { scope, key } = event {
                if scope == "group" {
                    self.invalidate_group(key);
                }
            }
        }
        Ok(effect)
    }
}
