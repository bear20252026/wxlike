//! E1 之三：播放统计——"谁看了多久"。
//!
//! 形状照搬 `crates/ledger`：事件 append-only，聚合靠重放，不做原地累加列。
//! 为什么这样：观看上报是**增量**（心跳 delta），任何"读—改—写"的计数字段在
//! 多实例部署下都会丢更新；事件表 + `SUM` 天然幂等，重复上报只是多几行，不会
//! 把一个数加两遍再加一遍。
//!
//! 隐私边界（合同 §1.4 内部测试模式）：聚合数（多少人在看、看了多久）任何调用方
//! 可读；**按人拆开**的名单必须配 `WXLIKE_VIDEO_STATS_TOKEN` 并带匹配的
//! `x-wxlike-video-stats-token` 才给——没配就 503 明确拒绝，绝不返回空名单
//! 假装"没人看"。
//!
//! `viewer` 是**客户端自报**的标识：本服务的点播面不鉴权（`server/src/lib.rs`
//! 直接把 `/api/video/*` 挂在公开路由上，Zone E 不许改装配），所以这里能算出
//! "谁看了多久"，但不能当作可信的身份。要把 viewer 绑到 user_id，得由命令面
//! （`crates/cmd`）加一条带鉴权的入口，已写进 PR 交集成方。

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use wxlike_api::{ApiError, ApiJson, ApiResult};

use crate::{timing::constant_time_eq, VideoState, VideoStore};

/// 单条上报允许的观看时长上限（12 小时）。
///
/// 上限不是为了"防攻击"，是把客户端算错的数量级（把秒当毫秒、把整部剧集的总时长
/// 算进一条心跳）挡在门外——一条 40 小时的"观看"会把平均完成度压成 0，而 0 比
/// 报错更难查。
pub const MAX_WATCHED_MS: u64 = 12 * 60 * 60 * 1000;
/// `viewer` / `session_id` 的长度上限。
pub const MAX_WATCH_ID_CHARS: usize = 128;
/// 名单一页最多多少人。
pub const MAX_VIEWER_PAGE: u32 = 500;
/// 名单默认页大小。
pub const DEFAULT_VIEWER_PAGE: u32 = 50;
/// 名单鉴权所需的环境变量与请求头。
pub const STATS_TOKEN_ENV: &str = "WXLIKE_VIDEO_STATS_TOKEN";
/// 名单鉴权请求头。
pub const STATS_TOKEN_HEADER: &str = "x-wxlike-video-stats-token";

/// 一次观看上报（客户端心跳，或暂停/关闭时补发最后一段）。
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct WatchEvent {
    /// 观看者标识，客户端自报（见模块头最后一段）。
    pub viewer: String,
    /// 一次播放会话（同一个人打开两次 = 两个会话）。留空则服务端补一个。
    #[serde(default)]
    pub session_id: String,
    /// 本次**新增**的观看时长。必须为正：0 时长的"我还活着"不该进统计。
    pub watched_ms: u64,
    /// 播放器当前进度（用来回答"看到了第几分钟"）。
    #[serde(default)]
    pub position_ms: u64,
    /// 客户端自报看完。服务端不猜（进度到末尾 ≠ 看完）。
    #[serde(default)]
    pub completed: bool,
}

/// 一条聚合结果。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct WatchStats {
    pub video_id: String,
    /// 上报条数（一次会话通常有多条心跳）。
    pub events: u64,
    pub unique_viewers: u64,
    pub total_watched_ms: u64,
    /// 人均观看时长 = `total_watched_ms / unique_viewers`（没人看时是 0）。
    pub avg_watched_ms_per_viewer: u64,
    /// 平均完成度：每人累计时长 ÷ 片长（封顶 1.0）再取人均。
    /// 片长未知时是 `None`——不是 `Some(0.0)`，"不知道"和"没人看完"是两件事。
    pub avg_completion: Option<f64>,
    pub first_seen_ms: Option<i64>,
    pub last_seen_ms: Option<i64>,
}

/// 按人拆开的一行。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ViewerWatch {
    pub viewer: String,
    pub events: u64,
    pub sessions: u64,
    pub watched_ms: u64,
    /// 这个人到达过的最远进度。
    pub max_position_ms: u64,
    pub completed: bool,
    pub last_seen_ms: i64,
}

/// 上报被接受后回执给客户端的内容（后续心跳要沿用同一个 `session_id`）。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct WatchEventRecord {
    pub event_id: i64,
    pub viewer: String,
    pub session_id: String,
    pub recorded_at_ms: i64,
}

/// 记录失败的三种原因，handler 一一对到 404 / 422 / 500。
///
/// 分开是必须的：把"库坏了"和"参数不对"都回 422，运维会去查客户端而实际上
/// 该查磁盘。
#[derive(Debug)]
pub enum WatchError {
    /// 参数非法（长度、范围、空值）
    Invalid(String),
    /// 这个 video_id 没有对应对象
    UnknownVideo,
    /// 存储层出错
    Store(anyhow::Error),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(msg) => write!(f, "invalid watch report: {msg}"),
            Self::UnknownVideo => write!(f, "no such video"),
            Self::Store(err) => write!(f, "watch store: {err:#}"),
        }
    }
}

impl std::error::Error for WatchError {}

impl From<rusqlite::Error> for WatchError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Store(err.into())
    }
}

impl VideoStore {
    /// 记一条观看事件。
    pub fn record_watch(
        &self,
        video_id: &str,
        event: &WatchEvent,
    ) -> Result<WatchEventRecord, WatchError> {
        let viewer = event.viewer.trim();
        if viewer.is_empty() {
            return Err(WatchError::Invalid("viewer must not be empty".into()));
        }
        if viewer.chars().count() > MAX_WATCH_ID_CHARS {
            return Err(WatchError::Invalid(format!(
                "viewer longer than {MAX_WATCH_ID_CHARS} chars"
            )));
        }
        if event.watched_ms == 0 || event.watched_ms > MAX_WATCHED_MS {
            return Err(WatchError::Invalid(format!(
                "watched_ms must be 1..={MAX_WATCHED_MS}"
            )));
        }
        let raw_session = event.session_id.trim();
        if raw_session.chars().count() > MAX_WATCH_ID_CHARS {
            return Err(WatchError::Invalid(format!(
                "session_id longer than {MAX_WATCH_ID_CHARS} chars"
            )));
        }
        let session_id = if raw_session.is_empty() {
            format!("s-{}", random_hex(8))
        } else {
            raw_session.to_string()
        };
        if !self.contains(video_id) {
            return Err(WatchError::UnknownVideo);
        }
        let now = wxlike_core::types::now_ms() as i64;
        let db = match self.db() {
            Ok(db) => db,
            Err(err) => return Err(WatchError::Store(err)),
        };
        // 一次持锁完成 INSERT + last_insert_rowid：分两次取锁的话，中间别人插过
        // 一行，回执里的 event_id 就不是自己那条了。
        let event_id = {
            let c = db.c();
            let stored = c.execute(
                "INSERT INTO watch_events(video_id, viewer, session_id, watched_ms,
                                          position_ms, completed, created_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    video_id,
                    viewer,
                    session_id,
                    event.watched_ms as i64,
                    event.position_ms as i64,
                    i64::from(event.completed),
                    now
                ],
            )?;
            if stored != 1 {
                return Err(WatchError::Store(anyhow::anyhow!(
                    "watch insert affected {stored} rows"
                )));
            }
            c.last_insert_rowid()
        };
        Ok(WatchEventRecord {
            event_id,
            viewer: viewer.to_string(),
            session_id,
            recorded_at_ms: now,
        })
    }

    /// 聚合：多少事件、多少人、看了多久、平均完成度。
    pub fn watch_stats(&self, video_id: &str) -> anyhow::Result<WatchStats> {
        let db = self.db()?;
        let (events, unique_viewers, total_watched_ms, first_seen_ms, last_seen_ms) = {
            let c = db.c();
            c.query_row(
                "SELECT COUNT(*), COUNT(DISTINCT viewer), COALESCE(SUM(watched_ms), 0),
                        MIN(created_at), MAX(created_at)
                 FROM watch_events WHERE video_id = ?1",
                params![video_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)? as u64,
                        r.get::<_, i64>(1)? as u64,
                        r.get::<_, i64>(2)? as u64,
                        r.get::<_, Option<i64>>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                    ))
                },
            )?
        };
        // 没人看时是 0，不是除零：`checked_div` 把这件事写成一行
        let avg_watched_ms_per_viewer = total_watched_ms.checked_div(unique_viewers).unwrap_or(0);
        Ok(WatchStats {
            video_id: video_id.to_string(),
            events,
            unique_viewers,
            total_watched_ms,
            avg_watched_ms_per_viewer,
            avg_completion: self.avg_completion(&db, video_id)?,
            first_seen_ms,
            last_seen_ms,
        })
    }

    /// 按人拆开的名册（handler 侧还要过 stats token 这道门）。
    pub fn watch_viewers(&self, video_id: &str, limit: u32) -> anyhow::Result<Vec<ViewerWatch>> {
        let limit = limit.clamp(1, MAX_VIEWER_PAGE);
        let db = self.db()?;
        let c = db.c();
        let mut stmt = c.prepare(
            "SELECT viewer, COUNT(*), COUNT(DISTINCT session_id), SUM(watched_ms),
                    MAX(position_ms), MAX(completed), MAX(created_at)
             FROM watch_events WHERE video_id = ?1
             GROUP BY viewer
             ORDER BY SUM(watched_ms) DESC, MAX(created_at) DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![video_id, limit as i64], |r| {
            Ok(ViewerWatch {
                viewer: r.get(0)?,
                events: r.get::<_, i64>(1)? as u64,
                sessions: r.get::<_, i64>(2)? as u64,
                watched_ms: r.get::<_, i64>(3)? as u64,
                max_position_ms: r.get::<_, i64>(4)? as u64,
                completed: r.get::<_, i64>(5)? != 0,
                last_seen_ms: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 人均完成度：先算每人看了整片的几成（封顶 1.0），再取平均。
    ///
    /// 片长来自 `meta.json`。读不到时返回 `None` 而不是 0.0，并分两种情况记日志：
    /// 对象已经不在（被清理）是 `debug`，对象在但 `meta.json` 读不出来是 `warn`——
    /// 后者说明存储坏了，必须看得见。
    fn avg_completion(
        &self,
        db: &crate::db::VideoDb,
        video_id: &str,
    ) -> anyhow::Result<Option<f64>> {
        let duration_ms = match self.meta(video_id) {
            Ok(meta) => u64::from(meta.duration_ms),
            Err(err) => {
                if self.contains(video_id) {
                    tracing::warn!(video_id, error = %err, "对象在但 meta.json 读不出来：完成度记为未知");
                } else {
                    tracing::debug!(video_id, error = %err, "对象已不存在：完成度记为未知");
                }
                return Ok(None);
            }
        };
        if duration_ms == 0 {
            return Ok(None);
        }
        let per_viewer: Vec<i64> = {
            let c = db.c();
            let mut stmt = c.prepare(
                "SELECT SUM(watched_ms) FROM watch_events WHERE video_id = ?1 GROUP BY viewer",
            )?;
            let rows = stmt.query_map(params![video_id], |r| r.get::<_, i64>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        if per_viewer.is_empty() {
            return Ok(Some(0.0));
        }
        let sum: f64 = per_viewer
            .iter()
            .map(|w| ((*w as f64) / duration_ms as f64).min(1.0))
            .sum();
        Ok(Some(sum / per_viewer.len() as f64))
    }
}

/// 名单分页参数。
#[derive(Deserialize, Debug, Default)]
pub struct ViewerPage {
    /// 返回前 N 人（默认 50，上限 500）
    pub limit: Option<u32>,
}

pub(crate) fn routes(state: VideoState) -> Router {
    Router::new()
        .route("/api/video/{video_id}/view", post(record_view))
        .route("/api/video/{video_id}/stats", get(get_stats))
        .route("/api/video/{video_id}/stats/viewers", get(get_viewers))
        .with_state(state)
}

async fn record_view(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
    ApiJson(event): ApiJson<WatchEvent>,
) -> ApiResult<(StatusCode, Json<WatchEventRecord>)> {
    crate::check_id(&video_id)?;
    match state.store.record_watch(&video_id, &event) {
        Ok(record) => Ok((StatusCode::CREATED, Json(record))),
        Err(WatchError::Invalid(msg)) => Err(ApiError::unprocessable(msg)),
        Err(WatchError::UnknownVideo) => Err(ApiError::not_found("no such video")),
        Err(err @ WatchError::Store(_)) => Err(ApiError::internal(err.to_string())),
    }
}

async fn get_stats(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
) -> ApiResult<Json<WatchStats>> {
    crate::check_id(&video_id)?;
    state
        .store
        .watch_stats(&video_id)
        .map(Json)
        .map_err(|e| ApiError::internal(format!("{e:#}")))
}

async fn get_viewers(
    State(state): State<VideoState>,
    AxumPath(video_id): AxumPath<String>,
    headers: HeaderMap,
    Query(page): Query<ViewerPage>,
) -> ApiResult<Json<Vec<ViewerWatch>>> {
    crate::check_id(&video_id)?;
    require_stats_token(&headers)?;
    let limit = page.limit.unwrap_or(DEFAULT_VIEWER_PAGE);
    state
        .store
        .watch_viewers(&video_id, limit)
        .map(Json)
        .map_err(|e| ApiError::internal(format!("{e:#}")))
}

/// 按人名单的门：没配 `WXLIKE_VIDEO_STATS_TOKEN` 就直接拒绝。
///
/// 用 503 而不是 403：这是"服务没配这项能力"，不是"你没权限"。让运维看一眼
/// 就知道去配环境变量，而不是去查用户角色。
fn require_stats_token(headers: &HeaderMap) -> ApiResult<()> {
    let expected = std::env::var(STATS_TOKEN_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let Some(expected) = expected else {
        return Err(ApiError::unavailable(format!(
            "viewer identity breakdown needs {STATS_TOKEN_ENV} to be configured"
        )));
    };
    let given = headers
        .get(STATS_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // 常数时间比较：这是共享密钥，不该按字节提前返回
    if constant_time_eq(given.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(ApiError::forbidden(format!(
            "viewer identity breakdown needs a matching {STATS_TOKEN_HEADER}"
        )))
    }
}

/// 客户端没给 `session_id` 时补一个。形状照搬 `crates/platform/src/auth.rs` 的
/// `random_hex`（同仓库；`rand` + `hex` 都已是 workspace 依赖）。
fn random_hex(n: usize) -> String {
    use rand::RngCore as _;
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}
