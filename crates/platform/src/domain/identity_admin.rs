//! G02-09 账户治理的管理端编排（从 `identity_admin` handler 收进来）。
//!
//! 这里只做三件事：拿一次连接、把 identity/relations 域函数跑完、把域错误
//! 映射成 HTTP。handler 从此不再接触 `Connection`，也不再把"鉴权 + 治理 +
//! 群缓存失效"拆在闭包里自己拼。
//!
//! 同步域 API 仍由 handler 包进 `spawn_blocking`（ADR-28）；本模块不引入
//! `await`，事务边界保持在一次连接守卫内。

use axum::http::StatusCode;

use crate::domain::qr_login::id_err;
use crate::error::ApiError;
use crate::Platform;
use wxlike_identity::governance::{self, AdminUserView, GovernCommand, GovernEffect};
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
        let c = self.db().lock();
        governance::suspend(
            &c,
            operator_token,
            cmd,
            &self.governance,
            wxlike_core::types::now_ms() as i64,
        )
        .map_err(id_err)
    }

    /// 解封（可逆，留痕）。
    pub(crate) fn admin_unsuspend(
        &self,
        operator_token: &str,
        cmd: &GovernCommand,
    ) -> Result<GovernEffect, ApiError> {
        let c = self.db().lock();
        governance::unsuspend(
            &c,
            operator_token,
            cmd,
            &self.governance,
            wxlike_core::types::now_ms() as i64,
        )
        .map_err(id_err)
    }

    /// 撤销设备：同事务作废该设备的会话/刷新链/step-up。
    pub(crate) fn admin_revoke_device(
        &self,
        operator_token: &str,
        cmd: &GovernCommand,
    ) -> Result<GovernEffect, ApiError> {
        let c = self.db().lock();
        governance::revoke_device(
            &c,
            operator_token,
            cmd,
            &self.governance,
            wxlike_core::types::now_ms() as i64,
        )
        .map_err(id_err)
    }

    /// 管理员元数据视图（白名单投影）；群数/举报数经 `UserMetadataPort`。
    pub(crate) fn admin_user_view(
        &self,
        operator_token: &str,
        user_id: &str,
    ) -> Result<AdminUserView, ApiError> {
        let c = self.db().lock();
        governance::user_view(
            &c,
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
        let c = self.db().lock();
        wxlike_identity::session::authenticate(
            &c,
            operator_token,
            wxlike_identity::Audience::Admin,
            wxlike_core::types::now_ms() as i64,
        )
        .map_err(id_err)?;
        let effect = relations_governance::govern(&c, cmd, wxlike_core::types::now_ms() as i64)
            .map_err(rel_err)?;
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

/// relations 的 `RelError` → HTTP。与 identity 的 `id_err` 同口径：
/// 损坏/存储/解码一律 500（服务端问题），不伪装成客户端错误。
fn rel_err(err: wxlike_relations::RelError) -> ApiError {
    use wxlike_relations::RelError;
    match err {
        RelError::BadInput(what) => ApiError::unprocessable(format!("bad input: {what}")),
        RelError::Forbidden(what) => ApiError::forbidden(format!("forbidden: {what}")),
        RelError::NotFound(what) => ApiError::not_found(format!("no such {what}")),
        RelError::Conflict(what) => ApiError::conflict(what),
        RelError::Corrupt(what) => {
            tracing::error!(%what, "relations 治理读到损坏行");
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "corrupt stored row")
        }
        RelError::Db(_) | RelError::Json(_) => {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}
