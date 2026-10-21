//! 视频转码回调的落地（ADR-21）：video-service 转完一档，把"这个 video_id
//! 现在有清单 / 播放地址 / 封面 / 时长"告诉平台。
//!
//! **平台只存引用，不存字节。** 消息里带的是 `core::msgmeta::VideoPayload`
//! 的结构化引用；发送方在发消息之前需要拿到 `playlist_url` / `play_url` /
//! `duration_ms`，而这三个数只有转码完成时才知道。回调是唯一权威来源——
//! 没有它，客户端只能自己拼 URL 并猜时长，猜错的代价是消息里写死一个
//! 打不开的地址。
//!
//! 两个刻意选择：
//!
//! 1. **未配置令牌 = 接口关闭**（与 `stats` 的名单门同一个口径）。回调能改
//!    消息引用，敞开等于让人往你的消息里塞任意播放地址。令牌走
//!    [`crate::PlatformOptions::video_callback_token`]，装配层从
//!    `WXLIKE_VIDEO_CALLBACK_TOKEN` 读。
//! 2. **回调可重入**：同一条作业重试会再回调一次，`video_id` 是主键，
//!    后到的覆盖先到的。转码是幂等的（同一源文件 → 同一个 id），所以
//!    "覆盖"不会产生两条互相矛盾的历史。

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::{Platform, PlatformState};

/// 回调令牌的环境变量名（装配层读它，本模块不直接读 env——测试要能注入）。
pub const CALLBACK_TOKEN_ENV: &str = "WXLIKE_VIDEO_CALLBACK_TOKEN";

/// 令牌头。用自定义头而不是 `Authorization`：这条请求不是用户身份，
/// 是服务间凭据，混在一起会让审计分不清"谁在做事"。
pub const CALLBACK_TOKEN_HEADER: &str = "x-wxlike-video-token";

pub fn routes() -> Router<PlatformState> {
    Router::new()
        .route("/api/video/callback", post(callback))
        .route("/api/video/ref/{video_id}", get(get_ref))
}

/// video-service 推过来的回调体（字段与 `crates/video` 的 `CallbackBody` 对齐）。
#[derive(Debug, Deserialize)]
pub struct CallbackBody {
    pub job_id: String,
    pub state: String,
    #[serde(default)]
    pub result: Option<CallbackResult>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub updated_at: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CallbackResult {
    pub video_id: String,
    #[serde(default)]
    pub playlist_url: String,
    #[serde(default)]
    pub play_url: Option<String>,
    #[serde(default)]
    pub thumb_url: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub renditions: Vec<serde_json::Value>,
}

/// 读回来的形状。字段名与消息侧的 `VideoPayload` 保持可对照（不混用类型：
/// 消息侧是"要发出去的东西"，这里是"服务端记下来的事实"）。
#[derive(Debug, Serialize)]
pub struct VideoRefView {
    pub video_id: String,
    pub job_id: String,
    pub state: String,
    pub playlist_url: String,
    pub play_url: String,
    pub thumb_url: String,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub renditions: Vec<serde_json::Value>,
    pub attempts: u32,
    pub updated_at_ms: i64,
}

fn require_token(platform: &Platform, headers: &HeaderMap) -> Result<()> {
    let Some(expected) = platform.options().video_callback_token.as_deref() else {
        // 未配置 = 关闭。不是"忘了配就放行"。
        return Err(PlatformError::Forbidden);
    };
    let got = headers
        .get(CALLBACK_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !constant_time_eq(got.as_bytes(), expected.as_bytes()) {
        return Err(PlatformError::Unauthorized);
    }
    Ok(())
}

/// 长度不短路的比较：令牌比较不该因为"第几位不同"而多花时间。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `video_id` 与 URL 的规矩和消息侧一致（`VideoPayload::validate` 是权威），
/// 这里只做"能不能落库"的最小事前检查——不复制那套规则，只挡住明显的坏数据。
fn validate(result: &CallbackResult) -> Result<()> {
    let id_ok =
        result.video_id.len() == 64 && result.video_id.chars().all(|c| c.is_ascii_hexdigit());
    if !id_ok {
        return Err(PlatformError::BadInput("video_id must be a sha256 hex"));
    }
    if !result.playlist_url.starts_with("/api/video/")
        || !result.playlist_url.ends_with(".m3u8")
        || result.playlist_url.contains("..")
    {
        return Err(PlatformError::BadInput(
            "playlist_url must be a /api/video/{id}/playlist.m3u8 path",
        ));
    }
    if let Some(play) = result.play_url.as_deref() {
        if !play.is_empty()
            && (!play.starts_with("/api/video/") || !play.ends_with(".mp4") || play.contains(".."))
        {
            return Err(PlatformError::BadInput(
                "play_url must be a /api/video/{id}/play.mp4 path",
            ));
        }
    }
    if let Some(thumb) = result.thumb_url.as_deref() {
        if !thumb.is_empty()
            && (!thumb.starts_with("/api/video/")
                || !thumb.ends_with(".png")
                || thumb.contains(".."))
        {
            return Err(PlatformError::BadInput(
                "thumb_url must be a /api/video/{id}/thumb.png path",
            ));
        }
    }
    Ok(())
}

async fn callback(
    State(platform): State<PlatformState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<CallbackBody>,
) -> std::result::Result<Json<VideoRefView>, ApiError> {
    store_callback(&platform, &headers, body)
        .map_err(explain)
        .map(Json)
}

/// 真正的逻辑：同步、返回库层错误，HTTP 映射留给上面那层。
///
/// 分两层不是为了好看——`ApiError` 的 `From` 只认 `PlatformError`，
/// 混在一个函数里写 `?` 会同时遇到两种错误类型，编译期就会打架。
fn store_callback(
    platform: &Platform,
    headers: &HeaderMap,
    body: CallbackBody,
) -> Result<VideoRefView> {
    require_token(platform, headers)?;
    // 失败的回调也要收：它告诉我们"这条作业没成"，但**不写引用**——
    // 写一个没有清单的引用，客户端会拿它当可播放。
    let Some(result) = body.result.clone() else {
        return Err(PlatformError::BadInput(
            "callback without a result: nothing to reference",
        ));
    };
    validate(&result)?;
    let at = body
        .updated_at
        .unwrap_or_else(|| wxlike_core::types::now_ms() as i64);
    let renditions = serde_json::to_string(&result.renditions)?;
    {
        let c = platform.db().lock();
        c.execute(
            r#"
            INSERT INTO video_refs (
                video_id, job_id, state, playlist_url, play_url, thumb_url,
                duration_ms, width, height, renditions, attempts, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(video_id) DO UPDATE SET
                job_id       = excluded.job_id,
                state        = excluded.state,
                playlist_url = excluded.playlist_url,
                play_url     = excluded.play_url,
                thumb_url    = excluded.thumb_url,
                duration_ms  = excluded.duration_ms,
                width        = excluded.width,
                height       = excluded.height,
                renditions   = excluded.renditions,
                attempts     = excluded.attempts,
                updated_at_ms= excluded.updated_at_ms
            "#,
            params![
                result.video_id,
                body.job_id,
                body.state,
                result.playlist_url,
                result.play_url.unwrap_or_default(),
                result.thumb_url.unwrap_or_default(),
                result.duration_ms as i64,
                result.width as i64,
                result.height as i64,
                renditions,
                body.attempts as i64,
                at,
            ],
        )?;
    }
    platform
        .video_ref(&result.video_id)?
        .ok_or(PlatformError::NotFound)
}

async fn get_ref(
    State(platform): State<PlatformState>,
    Path(video_id): Path<String>,
) -> std::result::Result<Json<VideoRefView>, ApiError> {
    platform
        .video_ref(&video_id)?
        .map(Json)
        .ok_or_else(|| ApiError::from(PlatformError::NotFound))
}

/// `video_refs` 一行的读取形状。列多，别名避免 `clippy::type_complexity`
/// （`report.rs` 里同一个坑）。
type RefRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    String,
    i64,
    i64,
);

fn row_to_view(row: RefRow) -> VideoRefView {
    let (
        video_id,
        job_id,
        state,
        playlist_url,
        play_url,
        thumb_url,
        duration_ms,
        width,
        height,
        renditions,
        attempts,
        updated_at_ms,
    ) = row;
    VideoRefView {
        video_id,
        job_id,
        state,
        playlist_url,
        play_url,
        thumb_url,
        duration_ms: duration_ms.max(0) as u64,
        width: width.max(0) as u32,
        height: height.max(0) as u32,
        renditions: serde_json::from_str(&renditions).unwrap_or_default(),
        attempts: attempts.max(0) as u32,
        updated_at_ms,
    }
}

impl Platform {
    /// 读一条视频引用。`None` = 还没收到回调（作业没转完，或根本没提交过）。
    pub fn video_ref(&self, video_id: &str) -> Result<Option<VideoRefView>> {
        let c = self.db().lock();
        let row: Option<RefRow> = c
            .query_row(
                r#"
                SELECT video_id, job_id, state, playlist_url, play_url, thumb_url,
                       duration_ms, width, height, renditions, attempts, updated_at_ms
                FROM video_refs WHERE video_id = ?1
                "#,
                params![video_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                        r.get(10)?,
                        r.get(11)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.map(row_to_view))
    }
}

/// `ApiError` 的映射在这里显式写一遍：`Forbidden` 的默认文案是"not permitted"，
/// 但这条 403 的真实含义是"部署没配令牌"，运维要能从响应里看出来。
pub fn explain(err: PlatformError) -> ApiError {
    match err {
        PlatformError::Forbidden => ApiError::new(
            StatusCode::FORBIDDEN,
            format!("video callback disabled: set {CALLBACK_TOKEN_ENV} to enable it"),
        ),
        other => other.into(),
    }
}
