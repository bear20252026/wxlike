//! E2：直播 ingest 的**落盘面与授权面**。
//!
//! # 为什么这一层不碰 RTMP
//!
//! ADR-21 与 `docs/p5-report.md` 定的纪律是"不在进程内跑媒体服务器"。RTMP 的
//! 协议终止交给外部节点（Xiu，或一台跑 `ffmpeg` 的接入机），本模块只做它做完
//! 之后的三件事：
//!
//! 1. **凭据校验**：与 `crates/platform/src/live.rs` 同一套自包含凭据
//!    `<exp_ms>.<hmac_sha256(secret, stream_key|exp)>`，**不查库**。接入节点可能
//!    在另一台机器上，让它依赖平台数据库等于把直播绑死在单点（这条纪律是
//!    p5-report 里写着的，本模块是它的第一次落地）。
//! 2. **滚动窗口 HLS 打包**：分片进来 → 落盘 → 重写 `index.m3u8`
//!    （`#EXT-X-MEDIA-SEQUENCE` + 只保留最近 N 片）。
//! 3. **目录约定打通**：产物落在 `{video_root}/live/{stream_key}/`，播放地址是
//!    `/live/{stream_key}/index.m3u8`——与平台 `live_state().hls_url` 生成的
//!    字符串**逐字节相同**，所以平台侧一行都不用改就能把地址发给观众。
//!
//! # 单写者
//!
//! `window.json` 读—改—写由一把进程内互斥锁串行化。跨进程（两台接入机推同一个
//! key）不支持：直播推流本来就该是单源的，多个写者只会产出互相覆盖的清单。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, PoisonError};

use anyhow::Context as _;
use axum::body::Bytes;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hmac::{Hmac, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use wxlike_api::{ApiError, ApiResult};

use crate::{timing::constant_time_eq, VideoState, VideoStore};

type HmacSha256 = Hmac<Sha256>;

/// 分片目标时长（与 VOD 的 `HLS_SECONDS` 同一个值，两处不一致会让播放器
/// 在直播/点播之间切一下就没法对齐缓冲）。
pub const LIVE_TARGET_MS: u64 = 4_000;
/// 播放窗口保留多少个分片（默认 12 片 ≈ 48 秒可回看）。
pub const LIVE_DEFAULT_WINDOW: usize = 12;
/// 窗口上限：再多就是在本地堆一个 DVR，那是另一件事。
pub const LIVE_MAX_WINDOW: usize = 120;
/// 推流凭据环境变量（与平台共用同一个 secret）。
pub const LIVE_SECRET_ENV: &str = "WXLIKE_LIVE_INGEST_SECRET";
/// 窗口大小环境变量。
pub const LIVE_WINDOW_ENV: &str = "WXLIKE_LIVE_WINDOW_SEGMENTS";
/// 推流凭据所在请求头。
pub const LIVE_TOKEN_HEADER: &str = "x-wxlike-live-token";
/// 分片时长所在请求头（毫秒）。打包器不猜时长，猜错会让清单里的
/// `#EXT-X-TARGETDURATION` 与 `#EXTINF` 打架，播放器直接拒收。
pub const LIVE_SEGMENT_MS_HEADER: &str = "x-wxlike-segment-duration-ms";

/// 播放窗口里的一片。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LiveSegment {
    /// 相对 `live/{stream_key}/` 的文件名，如 `seg_000012.ts`
    pub name: String,
    /// 实测时长（来自接入节点的 ffprobe）
    pub duration_ms: u64,
    /// HLS 序号（`#EXT-X-MEDIA-SEQUENCE` 由窗口第一片给出）
    pub seq: u64,
}

/// 一路直播的窗口状态，落盘为 `window.json`。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LiveWindow {
    pub stream_key: String,
    /// 清单里最多留几片
    pub window: usize,
    /// 下一片的序号
    pub next_seq: u64,
    pub segments: Vec<LiveSegment>,
    /// 收到 stop 后为真：清单追加 `#EXT-X-ENDLIST`，不再接受新分片
    pub ended: bool,
    /// 最近一片的文件名。重推同一片要挡住：清单里出现两条相同 `#EXTINF`
    /// 会让播放器把时长算错（实测 12 秒的片子被读成 16.5 秒）。
    #[serde(default)]
    pub last_name: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl LiveWindow {
    /// 新建一路直播的窗口（`window` 会被夹到 1..=`LIVE_MAX_WINDOW`）。
    pub fn new(stream_key: &str, window: usize, now_ms: i64) -> Self {
        Self {
            stream_key: stream_key.to_string(),
            window: window.clamp(1, LIVE_MAX_WINDOW),
            next_seq: 0,
            segments: Vec::new(),
            ended: false,
            last_name: None,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        }
    }

    /// 追加一片并裁窗口。已结束的窗口拒绝追加（`Err`）——推流端还在发而平台
    /// 已停播，这是要人看的问题，不能默默丢掉。
    pub fn append(&mut self, name: &str, duration_ms: u64, now_ms: i64) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.ended,
            "live window for {} already ended",
            self.stream_key
        );
        anyhow::ensure!(duration_ms > 0, "segment duration must be positive");
        anyhow::ensure!(
            !self.contains_name(name),
            "segment {name} was already packed into this session"
        );
        self.segments.push(LiveSegment {
            name: name.to_string(),
            duration_ms,
            seq: self.next_seq,
        });
        self.next_seq += 1;
        while self.segments.len() > self.window {
            self.segments.remove(0);
        }
        self.updated_at_ms = now_ms;
        self.last_name = Some(name.to_string());
        Ok(())
    }

    /// 这个名字是否还在记忆里（窗口内、或紧邻窗口的上一片）。
    ///
    /// 记忆只有窗口这么大：比窗口更早的旧名字重推会被当成"新的一片"赋新序号。
    /// 要防接入节点重放历史，正确做法是停播后换新流密钥，而不是让打包器无界
    /// 地记住所有见过的文件名。
    pub fn contains_name(&self, name: &str) -> bool {
        self.last_name.as_deref() == Some(name) || self.segments.iter().any(|s| s.name == name)
    }

    /// 停播：清单收尾，保留窗口内最后几片（回放尾巴）。
    pub fn end(&mut self, now_ms: i64) {
        self.ended = true;
        self.updated_at_ms = now_ms;
    }
}

/// 生成滚动窗口清单（`#EXTM3U`）。
///
/// 纯函数，不需要 ffmpeg：`TARGETDURATION` 取窗口内最长片（RFC 8216 要求它
/// ≥ 所有 `#EXTINF`），`MEDIA-SEQUENCE` 取窗口第一片的序号——播放器靠这两个
/// 字段判断"老片可以扔了"。
pub fn live_playlist(segments: &[LiveSegment], ended: bool) -> String {
    let mut out = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    if segments.is_empty() {
        // 空窗口也要是一份**合法**清单：播放器轮询时会拿到它。
        // `#EXT-X-TARGETDURATION` 给个保守值，不能省略（省略=不合规）。
        out.push_str(&format!(
            "#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:0\n",
            LIVE_TARGET_MS.div_ceil(1000)
        ));
        if ended {
            out.push_str("#EXT-X-ENDLIST\n");
        }
        return out;
    }
    let target_ms = segments
        .iter()
        .map(|s| s.duration_ms)
        .max()
        .unwrap_or(LIVE_TARGET_MS)
        .max(1);
    out.push_str(&format!(
        "#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
        target_ms.div_ceil(1000),
        segments[0].seq
    ));
    for seg in segments {
        out.push_str(&format!(
            "#EXTINF:{:.3},\n{}\n",
            seg.duration_ms as f64 / 1000.0,
            seg.name
        ));
    }
    if ended {
        out.push_str("#EXT-X-ENDLIST\n");
    }
    out
}

/// 一份 `window.json` → 清单文本。
pub fn playlist_of(window: &LiveWindow) -> String {
    live_playlist(&window.segments, window.ended)
}

// ---------------------------------------------------------------------------
// 推流凭据
// ---------------------------------------------------------------------------

/// 与平台同形的签名：`hmac_sha256(secret, "{stream_key}|{exp_ms}")`。
pub fn sign_ingest(secret: &str, stream_key: &str, exp_ms: u64) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| HmacSha256::new_from_slice(b"wxlike-live").expect("static key"));
    mac.update(stream_key.as_bytes());
    mac.update(b"|");
    mac.update(exp_ms.to_string().as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// 自包含凭据 `<exp_ms>.<sig>`（接入节点自己签，用于测试与本地起流）。
pub fn ingest_token(secret: &str, stream_key: &str, exp_ms: u64) -> String {
    format!("{exp_ms}.{}", sign_ingest(secret, stream_key, exp_ms))
}

/// 校验凭据：签名 + 有效期。**不查库**（见模块头）。
///
/// 会话是否还在播由平台侧的 `verify_ingest` 管；接入节点只关心"这把 key 配这个
/// 过期时间是不是平台签发的"。
pub fn verify_ingest_token(
    secret: &str,
    stream_key: &str,
    token: &str,
    now_ms: u64,
) -> anyhow::Result<()> {
    let (exp_s, sig) = token
        .split_once('.')
        .context("ingest token must be `<exp_ms>.<hmac>`")?;
    let exp: u64 = exp_s
        .trim()
        .parse()
        .context("ingest token expiry is not a number")?;
    anyhow::ensure!(now_ms <= exp, "ingest token expired at {exp}");
    let expected = sign_ingest(secret, stream_key, exp);
    anyhow::ensure!(
        constant_time_eq(sig.as_bytes(), expected.as_bytes()),
        "ingest token signature mismatch"
    );
    Ok(())
}

/// 平台读同一个环境变量（`server/src/lib.rs` 把它塞进 `PlatformOptions`）。
/// 没配就是**拒绝一切推流**——不提供 dev 默认密钥，`docs/audit-report.md` 已经
/// 点过一次这个名。
pub fn ingest_secret_from_env() -> Option<String> {
    std::env::var(LIVE_SECRET_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 窗口大小配置（env，非法值报错而不是悄悄用默认值）。
fn window_from_env() -> anyhow::Result<usize> {
    match std::env::var(LIVE_WINDOW_ENV) {
        Ok(raw) => {
            let value: usize = raw
                .trim()
                .parse()
                .with_context(|| format!("{LIVE_WINDOW_ENV} is not a number: {raw:?}"))?;
            anyhow::ensure!(
                (1..=LIVE_MAX_WINDOW).contains(&value),
                "{LIVE_WINDOW_ENV} must be 1..={LIVE_MAX_WINDOW}"
            );
            Ok(value)
        }
        Err(_) => Ok(LIVE_DEFAULT_WINDOW),
    }
}

// ---------------------------------------------------------------------------
// store side
// ---------------------------------------------------------------------------

/// `window.json` 的读—改—写串行化。见模块头"单写者"。
fn live_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

impl VideoStore {
    /// `live/{stream_key}` 目录。key 已由 [`check_stream_key`] 校验过才走到这里。
    pub fn live_dir(&self, stream_key: &str) -> PathBuf {
        self.root().join("live").join(stream_key)
    }

    /// 当前窗口（`None` = 这路直播还没有分片进来过）。
    pub fn live_window(&self, stream_key: &str) -> anyhow::Result<Option<LiveWindow>> {
        let path = self.live_dir(stream_key).join("window.json");
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
        }
    }

    /// 收一片：落盘 + 进窗口 + 重写清单。返回新的窗口。
    pub fn live_append_segment(
        &self,
        stream_key: &str,
        name: &str,
        bytes: &[u8],
        duration_ms: u64,
    ) -> anyhow::Result<LiveWindow> {
        check_stream_key(stream_key).map_err(|err| anyhow::anyhow!("live segment: {err}"))?;
        check_live_segment_name(name).map_err(|err| anyhow::anyhow!("live segment: {err}"))?;
        let _guard = live_lock().lock().unwrap_or_else(PoisonError::into_inner);
        let dir = self.live_dir(stream_key);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create live dir {}", dir.display()))?;
        let now = wxlike_core::types::now_ms() as i64;
        let mut window = match self.live_window(stream_key)? {
            Some(window) => window,
            None => LiveWindow::new(stream_key, window_from_env()?, now),
        };
        // 先写文件（temp + rename），再动清单：顺序反了会有一份指向不存在分片的清单
        let tmp = dir.join(format!(".{name}.part"));
        std::fs::write(&tmp, bytes)
            .with_context(|| format!("write live segment {}", tmp.display()))?;
        std::fs::rename(&tmp, dir.join(name))
            .with_context(|| format!("stage live segment {name}"))?;
        window.append(name, duration_ms, now)?;
        std::fs::write(dir.join("window.json"), serde_json::to_vec_pretty(&window)?)?;
        std::fs::write(dir.join("index.m3u8"), playlist_of(&window).as_bytes())?;
        // 出窗口的分片**不删**：那正是回放/录制的原料，而直播时长的磁盘上限
        // 是运维策略（挂几块盘、留几天），不该由打包器靠猜文件名来代劳。
        // 需要回收时由部署侧按 `window.json` 里的 seq 删。
        Ok(window)
    }

    /// 停播：清单加 `#EXT-X-ENDLIST`，之后的推流一律拒。
    pub fn live_end(&self, stream_key: &str) -> anyhow::Result<LiveWindow> {
        check_stream_key(stream_key).map_err(|err| anyhow::anyhow!("live end: {err}"))?;
        let _guard = live_lock().lock().unwrap_or_else(PoisonError::into_inner);
        let dir = self.live_dir(stream_key);
        let mut window = self
            .live_window(stream_key)?
            .with_context(|| format!("no live session for {stream_key}"))?;
        window.end(wxlike_core::types::now_ms() as i64);
        std::fs::write(dir.join("window.json"), serde_json::to_vec_pretty(&window)?)?;
        std::fs::write(dir.join("index.m3u8"), playlist_of(&window).as_bytes())?;
        Ok(window)
    }

    /// 清单字节（无会话时 `None` → handler 回 404，不回空清单假装在播）。
    pub fn live_playlist_bytes(&self, stream_key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let path = self.live_dir(stream_key).join("index.m3u8");
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
        }
    }

    /// 直播分片路径（handler 再用 `crate::range` 处理 Range）。
    pub fn live_segment_path(&self, stream_key: &str, name: &str) -> PathBuf {
        self.live_dir(stream_key).join(name)
    }
}

/// 直播播放地址。**与平台 `live_state().hls_url` 的拼法逐字对齐**
/// （`crates/platform/src/live.rs` 里是 `format!("/live/{key}/index.m3u8")`），
/// 所以平台发出的地址能直接打到本模块的路由上；单测会断言这两处一致。
pub fn live_url(stream_key: &str) -> String {
    format!("/live/{stream_key}/index.m3u8")
}

/// 流密钥：平台发的是 24 位 hex（`auth::random_hex(12)`），这里放宽到
/// hex/alnum/-/_ 且 ≤ 64 字符——**先校验再拼路径**，与点播 id 同一条防线。
pub fn check_stream_key(stream_key: &str) -> Result<(), String> {
    if stream_key.is_empty() || stream_key.len() > 64 {
        return Err("stream key must be 1..=64 characters".into());
    }
    if stream_key.contains("..") {
        return Err("stream key must not contain `..`".into());
    }
    if !stream_key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("stream key must be alphanumeric (with - and _)".into());
    }
    Ok(())
}

/// 直播分片名：不带路径、`seg_*.ts` 或 `init.mp4`/`*.m4s`（fMP4 直播预留），
/// 拒绝 `..`。与 [`crate::check_segment_name`] 分开，因为直播文件名由接入节点
/// 决定、不受 ffmpeg 的 `seg_%03d.ts` 约束。
pub fn check_live_segment_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 96 {
        return Err("segment name must be 1..=96 characters".into());
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("segment name must be a bare file name".into());
    }
    let ok_ext = name.ends_with(".ts") || name.ends_with(".m4s") || name.ends_with(".mp4");
    if !ok_ext {
        return Err("segment name must end with .ts, .m4s or .mp4".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err("segment name has forbidden characters".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP surface
// ---------------------------------------------------------------------------

pub(crate) fn routes(state: VideoState) -> Router {
    Router::new()
        .route("/live/{stream_key}/index.m3u8", get(get_playlist))
        .route(
            "/live/{stream_key}/segment/{name}",
            get(get_segment).put(push_segment),
        )
        .route("/live/{stream_key}/stop", post(stop_session))
        .with_state(state)
}

/// 接入节点推上一片之后的回执。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct LiveAppendReply {
    /// 这一片的 HLS 序号
    pub seq: u64,
    /// 窗口里的片数
    pub in_window: usize,
    /// 清单当前的 `#EXT-X-MEDIA-SEQUENCE`
    pub media_sequence: u64,
    /// 观众该拉的地址（与平台 `live_state().hls_url` 同形）
    pub playlist_url: String,
}

/// 推流授权：凭据校验只在内存里做 HMAC，**不查库**（见模块头）。
///
/// 状态码区分是有意的：
/// * 503 = 服务端没配 `WXLIKE_LIVE_INGEST_SECRET`（部署问题，配上就好）
///
/// * 401 = 没带头 / 签名不对 / 已过期（接入节点的问题，重试也没用）
///
/// 把这两种混成一个 403，运维就只能靠猜。
fn authorize(stream_key: &str, headers: &HeaderMap) -> ApiResult<()> {
    let Some(secret) = ingest_secret_from_env() else {
        return Err(ApiError::unavailable(format!(
            "{LIVE_SECRET_ENV} is not set; refusing all ingest (no dev default secret)"
        )));
    };
    let Some(token) = headers
        .get(LIVE_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    else {
        return Err(ApiError::unauthorized(format!(
            "missing {LIVE_TOKEN_HEADER}"
        )));
    };
    verify_ingest_token(&secret, stream_key, &token, wxlike_core::types::now_ms())
        .map_err(|err| ApiError::unauthorized(err.to_string()))
}

/// 拉直播清单。
///
/// `Cache-Control: no-store` 是直播面的硬要求：清单是滚动窗口，被中间层缓存住
/// 就等于观众停在 48 秒以前。
async fn get_playlist(
    State(state): State<VideoState>,
    AxumPath(stream_key): AxumPath<String>,
) -> ApiResult<Response> {
    check_stream_key(&stream_key).map_err(not_found)?;
    let bytes = state
        .store
        .live_playlist_bytes(&stream_key)
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    Ok(match bytes {
        Some(body) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/vnd.apple.mpegurl"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response(),
        // 没有会话就是 404：回一份空清单会让播放器以为"在播但没内容"。
        // 以前这里手拼 `{"error":"no live session"}`——形状对、但是硬编码的字符串，
        // 多一个字段就得改字面量。现在走统一契约。
        None => ApiError::not_found("no live session").into_response(),
    })
}

/// 拉直播分片（支持 Range：观众掉线重传时用得上）。
async fn get_segment(
    State(state): State<VideoState>,
    AxumPath((stream_key, name)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    check_stream_key(&stream_key).map_err(not_found)?;
    check_live_segment_name(&name).map_err(not_found)?;
    let path = state.store.live_segment_path(&stream_key, &name);
    Ok(crate::serve_file(path, "video/mp2t", headers).await)
}

/// 接入节点推一片上来。
async fn push_segment(
    State(state): State<VideoState>,
    AxumPath((stream_key, name)): AxumPath<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<LiveAppendReply>)> {
    check_stream_key(&stream_key).map_err(bad_request)?;
    authorize(&stream_key, &headers)?;
    check_live_segment_name(&name).map_err(bad_request)?;
    let duration_ms = header_ms(&headers)?;
    if duration_ms == 0 {
        return Err(ApiError::bad_request(format!(
            "{LIVE_SEGMENT_MS_HEADER} must be positive"
        )));
    }
    // 已经结束的会话、以及重推的同一片，都在落盘之前拒掉：
    // 后者一旦落盘就会在清单里写出两条相同的 `#EXTINF`。
    let existing = state
        .store
        .live_window(&stream_key)
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    if let Some(window) = existing {
        if window.ended {
            return Err(ApiError::conflict(format!(
                "live session {stream_key} already ended"
            )));
        }
        if window.contains_name(&name) {
            return Err(ApiError::conflict(format!(
                "segment {name} was already packed into this session"
            )));
        }
    }
    let store = state.store.clone();
    let key = stream_key.clone();
    let window = tokio::task::spawn_blocking(move || {
        store.live_append_segment(&key, &name, &body, duration_ms)
    })
    .await
    .map_err(|e| ApiError::internal(format!("packager task: {e}")))?
    .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    let last_seq = window
        .segments
        .last()
        .map(|s| s.seq)
        .unwrap_or_else(|| window.next_seq.saturating_sub(1));
    let media_sequence = window.segments.first().map(|s| s.seq).unwrap_or(0);
    Ok((
        StatusCode::CREATED,
        Json(LiveAppendReply {
            seq: last_seq,
            in_window: window.segments.len(),
            media_sequence,
            playlist_url: live_url(&stream_key),
        }),
    ))
}

/// 停播：清单加 `#EXT-X-ENDLIST`，窗口冻结。
async fn stop_session(
    State(state): State<VideoState>,
    AxumPath(stream_key): AxumPath<String>,
    headers: HeaderMap,
) -> ApiResult<Json<LiveWindow>> {
    check_stream_key(&stream_key).map_err(bad_request)?;
    authorize(&stream_key, &headers)?;
    let store = state.store.clone();
    let key = stream_key.clone();
    let result = tokio::task::spawn_blocking(move || store.live_end(&key))
        .await
        .map_err(|e| ApiError::internal(format!("packager task: {e}")))?;
    match result {
        Ok(window) => Ok(Json(window)),
        // 没推过流、或已经停过：404，而不是"成功停了一个空会话"
        Err(err) if err.to_string().contains("no live session") => {
            Err(ApiError::not_found(err.to_string()))
        }
        Err(err) => Err(ApiError::internal(format!("{err:#}"))),
    }
}

/// 毫秒时长头：缺失/非数字都是 400，不猜时长（清单里的 `TARGETDURATION` 与
/// `#EXTINF` 一旦打架，播放器会整段拒收）。
fn header_ms(headers: &HeaderMap) -> ApiResult<u64> {
    let raw = headers
        .get(LIVE_SEGMENT_MS_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::bad_request(format!("missing {LIVE_SEGMENT_MS_HEADER}")))?;
    raw.parse::<u64>().map_err(|err| {
        ApiError::bad_request(format!("{LIVE_SEGMENT_MS_HEADER} is not a number: {err}"))
    })
}

fn not_found(msg: String) -> ApiError {
    ApiError::not_found(msg)
}

fn bad_request(msg: String) -> ApiError {
    ApiError::bad_request(msg)
}
