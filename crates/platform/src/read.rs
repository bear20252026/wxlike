//! P3 读扩散接线：把**已存在**的信箱能力暴露成 HTTP。
//!
//! 重要：这里不新建表、不新建游标。序列号、已读游标、未读数、五层同步指令
//! 全部来自 [`wxlike_message::MessageStore`]（`crates/message`，ADR-16/17）。
//! 之前我在 platform 层另写了一套 `read_cursor`，与它重复，已回滚——见
//! `docs/p3-mailbox.md`。
//!
//! 本模块只解决一件事：**会话 id → 属主三元组**（owner_type, owner_id）。
//! 信箱按属主组织，而 HTTP 拿到的是 conv_id，所以需要一张映射表。direct 的
//! `d-a~b` 可以推出来，群与频道是随机 id，必须落表（`conv_owner`）。
//!
//! 当前覆盖：channel（`communities::send_channel_message` 已写入信箱）。
//! direct/group 仍走 `core::Store` 扇出，**未镜像进信箱**，因此它们的
//! `unread` 会是 0——这是接线未完成，不是算错。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::{Platform, PlatformState};

/// 信箱错误 → 平台错误（`Mail` 变体已有，别再 leak 字符串）。
fn mail<T>(r: wxlike_message::Result<T>) -> Result<T> {
    r.map_err(|e| PlatformError::Mail(e.to_string()))
}

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/conv/{conv}/unread", get(unread))
        .route("/api/conv/{conv}/read", post(mark_read))
        .route("/api/conv/{conv}/sync", get(sync))
}

/// 未读快照（全部来自信箱）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct UnreadView {
    pub conv: String,
    /// 属主类型：direct / group / channel。
    pub owner: String,
    /// 信箱最新序列号。
    pub head_seq: u64,
    /// 我的已读游标。
    pub read_seq: u64,
    /// `head_seq − read_seq`。
    pub unread: u64,
}

#[derive(Deserialize, Default)]
pub struct MarkRead {
    /// 读到第几条；缺省 = 读到最新。
    #[serde(default)]
    pub seq: Option<u64>,
}

/// 五层差分指令（原样透出 `MessageStore::sync_plan` 的判定）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "directive", rename_all = "snake_case")]
pub enum SyncView {
    UpToDate,
    Delta { since_version: u64 },
    BatchCollapse,
    FullResync,
}

impl Platform {
    /// 会话 → 属主类型。direct 由 id 推导，其余查 `conv_owner`。
    pub fn owner_kind_of(&self, conv: &str) -> Result<wxlike_message::OwnerType> {
        use wxlike_message::OwnerType;
        if conv.starts_with("d-") {
            return Ok(OwnerType::Direct);
        }
        let c = self.db().lock();
        let kind: Option<String> = c
            .query_row(
                "SELECT owner_type FROM conv_owner WHERE conv_id = ?1",
                params![conv],
                |r| r.get(0),
            )
            .ok();
        match kind.as_deref() {
            Some("group") => Ok(OwnerType::Group),
            Some("channel") => Ok(OwnerType::Channel),
            // 没有记录的老会话：默认按群处理（比 500 更有用，且可后续补录）
            _ => Ok(OwnerType::Group),
        }
    }

    /// 登记会话属主（建群 / 建频道时调用）。幂等。
    pub fn set_owner_kind(&self, conv: &str, kind: wxlike_message::OwnerType) -> Result<()> {
        let name = match kind {
            wxlike_message::OwnerType::Direct => "direct",
            wxlike_message::OwnerType::Group => "group",
            wxlike_message::OwnerType::Channel => "channel",
        };
        self.db().lock().execute(
            "INSERT INTO conv_owner(conv_id, owner_type) VALUES(?1, ?2)
             ON CONFLICT(conv_id) DO UPDATE SET owner_type = excluded.owner_type",
            params![conv, name],
        )?;
        Ok(())
    }

    pub fn conv_unread(&self, user_id: &str, conv: &str) -> Result<UnreadView> {
        let owner = self.owner_kind_of(conv)?;
        let store = self
            .message_store()
            .ok_or(PlatformError::Mail("mailbox unavailable".into()))?;
        let head_seq = mail(store.head_seq((owner, conv)))?;
        let unread = mail(store.unread((owner, conv), user_id))?;
        Ok(UnreadView {
            conv: conv.to_string(),
            owner: match owner {
                wxlike_message::OwnerType::Direct => "direct",
                wxlike_message::OwnerType::Group => "group",
                wxlike_message::OwnerType::Channel => "channel",
            }
            .to_string(),
            head_seq,
            read_seq: head_seq.saturating_sub(unread),
            unread,
        })
    }

    pub fn mark_conv_read(
        &self,
        user_id: &str,
        conv: &str,
        upto: Option<u64>,
    ) -> Result<UnreadView> {
        let owner = self.owner_kind_of(conv)?;
        let store = self
            .message_store()
            .ok_or(PlatformError::Mail("mailbox unavailable".into()))?;
        let target = match upto {
            Some(seq) => seq,
            None => mail(store.head_seq((owner, conv)))?,
        };
        mail(store.ack((owner, conv), user_id, target))?;
        self.conv_unread(user_id, conv)
    }

    pub fn conv_sync(&self, conv: &str, client_version: u64) -> Result<SyncView> {
        let owner = self.owner_kind_of(conv)?;
        let store = self
            .message_store()
            .ok_or(PlatformError::Mail("mailbox unavailable".into()))?;
        let plan = mail(store.sync_plan((owner, conv), client_version))?;
        Ok(match plan {
            wxlike_message::SyncDirective::UpToDate => SyncView::UpToDate,
            wxlike_message::SyncDirective::Delta { since_version } => {
                SyncView::Delta { since_version }
            }
            wxlike_message::SyncDirective::BatchCollapse => SyncView::BatchCollapse,
            wxlike_message::SyncDirective::FullResync => SyncView::FullResync,
        })
    }
}

async fn unread(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<Json<UnreadView>, ApiError> {
    Ok(Json(platform.conv_unread(&authed.user_id, &conv)?))
}

async fn mark_read(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    body: axum::body::Bytes,
) -> std::result::Result<(StatusCode, Json<UnreadView>), ApiError> {
    // 空 body 合法（读到最新）；`Option<Json<T>>` 接不住空 body。
    let upto = if body.is_empty() {
        None
    } else {
        serde_json::from_slice::<MarkRead>(&body)
            .map_err(|_| PlatformError::BadInput("bad read body"))?
            .seq
    };
    Ok((
        StatusCode::OK,
        Json(platform.mark_conv_read(&authed.user_id, &conv, upto)?),
    ))
}

#[derive(Deserialize)]
struct SyncQuery {
    #[serde(default)]
    version: u64,
}

async fn sync(
    State(platform): State<PlatformState>,
    _authed: Authed,
    Path(conv): Path<String>,
    Query(q): Query<SyncQuery>,
) -> std::result::Result<Json<SyncView>, ApiError> {
    Ok(Json(platform.conv_sync(&conv, q.version)?))
}
