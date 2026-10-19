//! P3 read-service 的最小切片：**已读游标 + 未读数**。
//!
//! 参照 open-im-server 的会话模型（`refs/open-im-server`
//! `pkg/common/storage/model/conversation.go`：每个 `(owner, conversation)`
//! 一行 `max_seq` / `has_read_seq`，未读是两者的差，**不为每条消息写已读标记**）。
//! 我们这版把 `max_seq` 直接查 `messages`（按 `(conv_id, seq)` 索引取 MAX），
//! 少一张需要同步维护的冗余表——等 P3 的 message-service 建好每会话序列号，
//! 再决定是否物化。
//!
//! 为什么不能用"最大 seq − 已读 seq"：我们的 `messages.seq` 是**全局**
//! AUTOINCREMENT，不同会话交错增长，差值会把别人的消息算进来。所以未读必须是
//! `COUNT(*) WHERE conv_id = ? AND seq > read_seq`——区间计数走索引，代价与
//! 未读数无关。
//!
//! 语义要点：
//! * 游标**单调**：两台设备一台读到 50、另一台报 30，结果仍是 50
//!   （`MAX(read_seq, ?)`），否则旧设备一上线未读数就会反弹。
//! * 上报值超过当前最大 seq 时按最大 seq 截断，不留下"未来游标"。
//! * 只读写自己的游标，接口不返回别人的任何数据。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::{Platform, PlatformState};

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/conv/{conv}/unread", get(unread))
        .route("/api/conv/{conv}/read", post(mark_read))
}

/// 一个会话的未读快照。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Unread {
    pub conv: String,
    /// 会话内最大的消息 seq（没有消息时 0）。
    pub max_seq: i64,
    /// 我的已读游标。
    pub read_seq: i64,
    /// `read_seq` 之后的消息条数。
    pub unread: i64,
}

#[derive(Deserialize, Default)]
pub struct MarkRead {
    /// 读到的 seq；缺省表示"读到最新"。
    #[serde(default)]
    pub seq: Option<i64>,
}

impl Platform {
    /// 会话当前最大 seq 与我的游标。
    pub fn conv_unread(&self, user_id: &str, conv: &str) -> Result<Unread> {
        self.ensure_conv_visible(user_id, conv)?;
        let c = self.db().lock();
        // `messages` 表归 core::store 所有。Platform 允许单独打开一个没有消息表
        // 的库（纯控制面的测试/演示），这时未读按 0 算，而不是 500。
        if !has_messages(&c) {
            return Ok(Unread {
                conv: conv.to_string(),
                max_seq: 0,
                read_seq: 0,
                unread: 0,
            });
        }
        let max_seq: i64 = c.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE conv_id = ?1",
            params![conv],
            |r| r.get(0),
        )?;
        let read_seq: i64 = c
            .query_row(
                "SELECT read_seq FROM read_cursor WHERE user_id = ?1 AND conv_id = ?2",
                params![user_id, conv],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        // 游标可能因为消息被清理而落后于 0，钳一下避免负数
        let read_seq = read_seq.clamp(0, max_seq);
        let unread: i64 = c.query_row(
            "SELECT COUNT(*) FROM messages WHERE conv_id = ?1 AND seq > ?2",
            params![conv, read_seq],
            |r| r.get(0),
        )?;
        Ok(Unread {
            conv: conv.to_string(),
            max_seq,
            read_seq,
            unread,
        })
    }

    /// 把游标推到 `upto`（缺省=最新），**只前进不后退**。
    pub fn mark_conv_read(&self, user_id: &str, conv: &str, upto: Option<i64>) -> Result<Unread> {
        self.ensure_conv_visible(user_id, conv)?;
        let now = wxlike_core::types::now_ms() as i64;
        {
            // 锁的作用域必须收紧：`self.db()` 是**非重入** Mutex，
            // 持锁时再调任何会加锁的 Platform 方法就是死锁。
            let c = self.db().lock();
            if !has_messages(&c) {
                drop(c);
                return self.conv_unread(user_id, conv);
            }
            let max_seq: i64 = c.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM messages WHERE conv_id = ?1",
                params![conv],
                |r| r.get(0),
            )?;
            let target = upto.unwrap_or(max_seq).clamp(0, max_seq);
            c.execute(
                "INSERT INTO read_cursor(user_id, conv_id, read_seq, updated_at)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(user_id, conv_id) DO UPDATE SET
                     read_seq = MAX(read_cursor.read_seq, excluded.read_seq),
                     updated_at = excluded.updated_at",
                params![user_id, conv, target, now],
            )?;
        }
        self.conv_unread(user_id, conv)
    }

    /// 这个用户能不能看这个会话（1:1 看参与者，群看成员）。
    ///
    /// 不查也能返回 0，但那样等于给了"这个 conv 存在"的探测口；宁可 404。
    fn ensure_conv_visible(&self, user_id: &str, conv: &str) -> Result<()> {
        let visible = if let Some(pair) = conv.strip_prefix("d-") {
            pair.split('~').any(|u| u == user_id)
        } else {
            self.is_member(conv, user_id).unwrap_or(false)
        };
        if visible {
            Ok(())
        } else {
            Err(PlatformError::NotFound)
        }
    }
}

/// 这个库里有没有消息表（Platform 可以脱离 core::store 单独使用）。
fn has_messages(c: &rusqlite::Connection) -> bool {
    c.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'messages'",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

async fn unread(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
) -> std::result::Result<Json<Unread>, ApiError> {
    Ok(Json(platform.conv_unread(&authed.user_id, &conv)?))
}

async fn mark_read(
    State(platform): State<PlatformState>,
    authed: Authed,
    Path(conv): Path<String>,
    body: axum::body::Bytes,
) -> std::result::Result<(StatusCode, Json<Unread>), ApiError> {
    // 空 body 是合法的（"读到最新"）。`Option<Json<T>>` 做不到这点：
    // 它只接受"没有 Content-Type"，空 body 仍会解析失败并回 400。
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
