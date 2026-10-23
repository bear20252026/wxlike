//! E3：转码流水线——提交即返回、后台跑、失败可重试、完成后回调。
//!
//! 为什么要异步作业：`POST /api/video` 是同步转码，客户端要举着连接等整个
//! ffmpeg 跑完（50 MB 的片子内网尚可，公网就是"上传进度条永远不满"），中途
//! 断网或重启只能从头再来。作业表把这件事拆成
//! **提交（幂等）→ 转码（可重试）→ 回调（可重试）** 三段，各自有状态，
//! 进程重启后接着跑。
//!
//! 状态机形状照搬 `crates/core/src/outbox.rs`（同仓库参照重写，不是照搬代码）：
//! * `state` 是 TEXT 列；未知值直接报错，不当成 `pending` 重跑一遍；
//! * 退避 `1s→2s→4s→8s→16s→30s`、上限 5 次，与 outbox 一致（两套退避 = 用户
//!   在一处等 30 秒、另一处等 5 分钟）；
//! * **幂等键就是内容哈希**：`job_id == video_id`，同一份字节重复提交
//!   `ON CONFLICT DO NOTHING`，不会转两遍；
//! * 失败语义由知道错误含义的那层给（[`JobFailure`]），队列只执行；
//!   `throttled` 不烧尝试次数。
//!
//! 两处偏离 outbox，都是有意的：
//! 1. **转码成功与回调成功是两条状态**。回调挂了不能把作业退回 `pending`——
//!    那会白白重跑一遍几十分钟的 ffmpeg。
//! 2. `running` 卡超过 [`STALE_RUNNING_MS`] 的作业会被重新领取。worker 被杀
//!    不该让作业永远停在 running；重跑安全是因为产物是改名到位的（见
//!    `VideoStore::transcode`），半途而废的产物根本不会出现在对象目录里。

use std::time::Duration;

use anyhow::Context as _;
use axum::extract::multipart::Multipart;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, Error as SqliteError, OptionalExtension as _, Row};
use serde::{Deserialize, Serialize};
use wxlike_api::{ApiError, ApiResult};

use crate::{Ingested, VideoState, VideoStore};

/// 作业最多尝试几次（含首次）。超过进 `failed`，等运营点重试。
///
/// 与客户端发送队列共用同一套退避策略（`wxlike_core::retry::Backoff::DEFAULT`）。
/// 以前这里写着"与 outbox 的 `BASE_DELAY_MS` 一致"——**注释就是重复的证据**，
/// 现在改成同一个常量源，漂移不再可能。
pub const MAX_JOB_ATTEMPTS: u32 = wxlike_core::retry::Backoff::DEFAULT.max_attempts;
/// 首次重试延迟（见 `wxlike_core::retry::Backoff`）。
pub const BASE_DELAY_MS: u64 = wxlike_core::retry::Backoff::DEFAULT.base_ms;
/// 退避上限（见 `wxlike_core::retry::Backoff`）。
pub const MAX_DELAY_MS: u64 = wxlike_core::retry::Backoff::DEFAULT.max_ms;
/// 一轮 worker 只取这么几个作业：**串行转码**。阶梯转码是 CPU 密集型
/// （libx264 每档一遍），并行 4 个只会让 4 个都变慢并拖住整台机器。
/// 要真并发是给 video-service 单独开进程（Q7），不是在这里堆线程。
pub const JOBS_PER_ROUND: u32 = 1;
/// `running` 超过这个时长还没结束 = worker 中途死了，可以重新领取。
pub const STALE_RUNNING_MS: i64 = 30 * 60 * 1000;
/// 出站回调超时（对端挂住时不能把 worker 拖死）。
pub const CALLBACK_TIMEOUT_MS: u64 = 5_000;
/// worker 轮询间隔。
pub const WORKER_POLL: Duration = Duration::from_millis(500);
/// 作业源字节的暂存目录名（相对 `VideoStore::root()`）。
pub const PENDING_DIR: &str = "pending";
/// 错误文本入库前的截断长度：ffmpeg 一次能吐几 KB，作业表不是日志文件。
pub const MAX_ERROR_CHARS: usize = 2_000;

/// 作业状态（DB 里是 TEXT）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// 等着转（`next_retry_at` 到点才被领取）
    Pending,
    /// 正在转
    Running,
    /// 可播（清单、分片、`play.mp4` 都已落盘）
    Succeeded,
    /// 不再自动重试（错误不可重试，或次数用尽）
    Failed,
}

impl JobState {
    /// 入库用的字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    /// 解析状态列。未知值 = 库被别的版本写过，报错而不是猜。
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        Ok(match value {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            other => anyhow::bail!("unknown job state {other:?}"),
        })
    }
}

/// 回调状态（与作业状态分开，见模块头第 1 条偏离）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallbackState {
    /// 这个作业不需要回调（提交时没给地址，也没配全局地址）
    None,
    /// 服务没配 `WXLIKE_VIDEO_CALLBACK_URL`：部署时补上即可，与 `None` 不同——
    /// `None` 是"这个作业不需要"，`NotConfigured` 是"整个服务没配"
    NotConfigured,
    /// 待发/待重试
    Pending,
    /// 已送达
    Delivered,
    /// 不再重试
    Failed,
}

impl CallbackState {
    /// 入库用的字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::NotConfigured => "not_configured",
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
        }
    }

    /// 解析回调状态列。
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        Ok(match value {
            "none" => Self::None,
            "not_configured" => Self::NotConfigured,
            "pending" => Self::Pending,
            "delivered" => Self::Delivered,
            "failed" => Self::Failed,
            other => anyhow::bail!("unknown callback state {other:?}"),
        })
    }
}

/// 一个作业，即 `GET /api/video/jobs/{job_id}` 的响应体。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Job {
    /// == `video_id`（源字节的 SHA-256）
    pub job_id: String,
    /// 提交者（issue #7）：查作业/重试只对自己的人开放
    pub owner: String,
    pub state: JobState,
    pub attempts: u32,
    pub next_retry_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_error: Option<String>,
    pub source_size: u64,
    /// 转码成功后才有：播放地址、档位、时长。
    pub result: Option<Ingested>,
    pub callback_state: CallbackState,
    /// 本作业的回调地址（没给则用 `WXLIKE_VIDEO_CALLBACK_URL`）
    pub callback_url: Option<String>,
    pub callback_attempts: u32,
    pub callback_next_retry_at: i64,
    pub callback_error: Option<String>,
}

/// 提交结果。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct JobSubmit {
    pub job: Job,
    /// `false` = 这份内容早就转好了（或同一份字节已在队列里），没有新作业入队。
    pub newly_queued: bool,
}

/// 转码失败的原因（形状照搬 outbox 的 `SendFailure`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobFailure {
    pub message: String,
    pub retryable: bool,
    /// 限流/资源不足：**不烧尝试次数**，下一轮再来。
    pub throttled: bool,
}

impl JobFailure {
    /// 流水线报错的默认归类：**全部可重试**。
    ///
    /// 这里不猜"哪些错误是永久的"。ffmpeg 的错误文本千奇百怪，一次偶发的磁盘满
    /// 或 OOM 被归成永久错误，用户看到的就是"这个视频永远转不了"。
    /// 真正不该重试的错误（源字节长度对不上、参数非法）由调用方显式构造
    /// [`Self::permanent`]。
    pub fn from_err(err: &anyhow::Error) -> Self {
        Self {
            message: format!("{err:#}"),
            retryable: true,
            throttled: false,
        }
    }

    /// 可重试的失败。
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            throttled: false,
        }
    }

    /// 不可重试：直接终态。
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            throttled: false,
        }
    }

    /// 被限流：等一等再来，**不烧尝试次数**。
    pub fn throttled(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            throttled: true,
        }
    }
}

/// 第 `attempts` 次失败之后等多久（1/2/4/8/16/30s 封顶）。
///
/// 与发送队列共用 `wxlike_core::retry::Backoff`，不在这里维护第二份公式。
pub fn backoff_ms(attempts: u32) -> u64 {
    wxlike_core::retry::Backoff::DEFAULT.delay_ms(attempts)
}

/// 一轮 worker 的结论（进日志，也是验收断言点）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WorkerReport {
    /// 本轮转完并可播
    pub succeeded: u32,
    /// 本轮转失败但还会重试
    pub retrying: u32,
    /// 本轮转失败且不再自动重试
    pub failed: u32,
    /// 回调送达
    pub callbacks_delivered: u32,
    /// 回调还会重试
    pub callbacks_retrying: u32,
    /// 回调不再重试
    pub callbacks_failed: u32,
}

/// `owner` 在 [`JOB_SELECT_COLUMNS`] 里的下标（追加在最后，既有下标不位移）。
const OWNER_COLUMN: usize = 14;

/// 作业行的全部列（三条查询共用一份，少写一遍就会错一位）。
const JOB_SELECT_COLUMNS: &str = "job_id, state, attempts, next_retry_at, created_at, updated_at,
            last_error, source_size, result, callback_state, callback_url,
            callback_attempts, callback_next_retry_at, callback_error, owner";

/// 到点该转的作业（含卡住的 `running`）。
///
/// 拼出来而不是写死：判据里的 `STALE_RUNNING_MS` 必须跟常量是同一个数，
/// 两处各写一遍 1800000 是等着它漂移的典型写法。
static DUE_JOBS_SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "SELECT {JOB_SELECT_COLUMNS}
         FROM video_jobs
         WHERE (state = 'pending' AND next_retry_at <= ?1)
            OR (state = 'running' AND updated_at <= ?1 - {STALE_RUNNING_MS})
         ORDER BY next_retry_at, created_at
         LIMIT ?2"
    )
});

/// 到点该发的回调（同一份列清单，见 [`JOB_SELECT_COLUMNS`]）。
static DUE_CALLBACKS_SQL: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "SELECT {JOB_SELECT_COLUMNS}
         FROM video_jobs
         WHERE callback_state = 'pending' AND callback_next_retry_at <= ?1
         ORDER BY callback_next_retry_at, created_at
         LIMIT ?2"
    )
});

/// 提交被**策略**拒绝（不是服务端故障）。
///
/// 分成两类是为了让 HTTP 层给出正确的状态码：配额是"等一下再来"（429），
/// 回调地址是"你这个参数不对"（400）。混成一个 `anyhow` 字符串的话，
/// 调用方只能靠 grep 消息文本猜。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub kind: RejectKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectKind {
    /// 超出配额/并发预算
    Quota,
    /// 回调地址不合法或不在允许列表
    BadCallback,
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Rejected {}

impl VideoStore {
    /// 提交一个转码作业：入库 + 落盘源字节就返回，转码在后台。
    ///
    /// 已经可播的内容直接回现成结果（`newly_queued = false`），同时保证作业行
    /// 存在——否则"提交返回 200 但查作业 404"这种不一致会让客户端没法轮询。
    ///
    /// `owner` 是提交者（issue #7）：作业归属于他，别人查不到也重试不了。
    /// 配额与回调地址在**这里**就判掉，别等 worker 转完才发现不该收。
    pub fn submit_job(
        &self,
        bytes: &[u8],
        owner: &str,
        callback_url: Option<&str>,
    ) -> anyhow::Result<JobSubmit> {
        anyhow::ensure!(!bytes.is_empty(), "empty upload");
        let job_id = self.content_id(bytes);
        let policy = self.policy();
        let url = match callback_url
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_string)
            .or_else(|| {
                std::env::var("WXLIKE_VIDEO_CALLBACK_URL")
                    .ok()
                    .map(|u| u.trim().to_string())
                    .filter(|u| !u.is_empty())
            }) {
            // 客户端给的地址要过白名单（issue #8）；服务端默认地址是运维自己
            // 配的，不再套一遍白名单，否则环境变量与 allowlist 要写两遍。
            Some(url) if callback_url.is_some() => {
                let checked = policy.callbacks.check(&url).map_err(|message| Rejected {
                    kind: RejectKind::BadCallback,
                    message,
                })?;
                Some(checked)
            }
            other => other,
        };
        self.check_quota(owner, bytes.len() as u64)?;
        let now = wxlike_core::types::now_ms() as i64;
        let db = self.db()?;

        if self.contains(&job_id) {
            let result = self.ingested_from_meta(&job_id)?;
            db.c().execute(
                "INSERT INTO video_jobs(job_id, owner, state, attempts, next_retry_at, created_at,
                                        updated_at, source_size, result, callback_state,
                                        callback_url, callback_next_retry_at)
                 VALUES(?1, ?7, 'succeeded', 0, ?2, ?2, ?2, ?3, ?4, ?5, ?6, ?2)
                 ON CONFLICT(job_id) DO UPDATE SET
                    state = 'succeeded',
                    result = excluded.result,
                    updated_at = excluded.updated_at,
                    callback_state = CASE WHEN video_jobs.callback_state = 'delivered'
                                          THEN 'delivered' ELSE excluded.callback_state END,
                    callback_next_retry_at = excluded.callback_next_retry_at",
                params![
                    job_id,
                    now,
                    bytes.len() as i64,
                    serde_json::to_string(&result)?,
                    CallbackState::for_url(&url).as_str(),
                    url,
                    owner
                ],
            )?;
            return Ok(JobSubmit {
                job: self.job(&job_id)?.context("job row vanished")?,
                newly_queued: false,
            });
        }

        // 源字节先落盘：排队期间进程被杀，重启后作业还能接着跑
        let pending = self.pending_dir(&job_id);
        std::fs::create_dir_all(&pending)?;
        let tmp = pending.join("source.part");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, pending.join("source"))?;

        let inserted = db.c().execute(
            "INSERT INTO video_jobs(job_id, owner, state, attempts, next_retry_at, created_at,
                                    updated_at, source_size, callback_state, callback_url)
             VALUES(?1, ?6, 'pending', 0, ?2, ?2, ?2, ?3, ?4, ?5)
             ON CONFLICT(job_id) DO NOTHING",
            params![
                job_id,
                now,
                bytes.len() as i64,
                CallbackState::for_url(&url).as_str(),
                url,
                owner
            ],
        )?;
        Ok(JobSubmit {
            job: self.job(&job_id)?.context("job row vanished")?,
            newly_queued: inserted == 1,
        })
    }

    // ---- 归属与配额（issue #7）------------------------------------------

    /// 记一条同步上传的归属（`POST /api/video` 不建作业，但同样要知道是谁传的）。
    pub fn set_owner(&self, video_id: &str, owner: &str, size: u64) -> anyhow::Result<()> {
        let db = self.db()?;
        db.c().execute(
            "INSERT INTO video_owners(video_id, owner, size, created_at) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(video_id, owner) DO NOTHING",
            params![
                video_id,
                owner,
                size as i64,
                wxlike_core::types::now_ms() as i64
            ],
        )?;
        Ok(())
    }

    /// 这个人已经上传占用的源字节总量。
    pub fn user_upload_bytes(&self, owner: &str) -> anyhow::Result<u64> {
        let db = self.db()?;
        let bytes: i64 = db.c().query_row(
            "SELECT COALESCE(SUM(size), 0) FROM video_owners WHERE owner = ?1",
            params![owner],
            |row| row.get(0),
        )?;
        Ok(bytes.max(0) as u64)
    }

    /// 同步上传的磁盘配额（`POST /api/video` 不建作业，但同样落盘）。
    pub fn check_upload_quota(&self, owner: &str, incoming: u64) -> anyhow::Result<()> {
        let policy = self.policy();
        let used = self.user_upload_bytes(owner)?;
        if used.saturating_add(incoming) > policy.max_user_bytes {
            return Err(Rejected {
                kind: RejectKind::Quota,
                message: format!(
                    "上传总量超出个人额度（{} MiB）",
                    policy.max_user_bytes / (1024 * 1024)
                ),
            }
            .into());
        }
        Ok(())
    }

    /// 这条内容是不是这个人传的（含"同一份字节的另一次上传"）。
    pub fn owns_video(&self, video_id: &str, owner: &str) -> anyhow::Result<bool> {
        let db = self.db()?;
        let found: Option<i64> = db
            .c()
            .query_row(
                "SELECT 1 FROM video_owners WHERE video_id = ?1 AND owner = ?2",
                params![video_id, owner],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// 作业归属：**不泄露存在性**——不存在与"是别人的"都是 `false`。
    pub fn job_owned_by(&self, job_id: &str, owner: &str) -> anyhow::Result<bool> {
        let db = self.db()?;
        let found: Option<i64> = db
            .c()
            .query_row(
                "SELECT 1 FROM video_jobs WHERE job_id = ?1 AND owner = ?2",
                params![job_id, owner],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// 这个用户当前排队/在飞的作业数与被占字节数。
    pub fn user_usage(&self, owner: &str) -> anyhow::Result<(u32, u64)> {
        let db = self.db()?;
        let (count, bytes): (i64, i64) = db.c().query_row(
            "SELECT COUNT(*), COALESCE(SUM(source_size), 0) FROM video_jobs
             WHERE owner = ?1 AND state IN ('pending', 'running')",
            params![owner],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((count.max(0) as u32, bytes.max(0) as u64))
    }

    /// 全站排队中的源字节（磁盘预算）。
    pub fn total_pending_bytes(&self) -> anyhow::Result<u64> {
        let db = self.db()?;
        let bytes: i64 = db.c().query_row(
            "SELECT COALESCE(SUM(source_size), 0) FROM video_jobs
             WHERE state IN ('pending', 'running')",
            [],
            |row| row.get(0),
        )?;
        Ok(bytes.max(0) as u64)
    }

    /// 配额闸门。超了就返回 [`Rejected`]，让 HTTP 层回 429 而不是 500。
    fn check_quota(&self, owner: &str, incoming: u64) -> anyhow::Result<()> {
        let policy = self.policy();
        let (jobs, bytes) = self.user_usage(owner)?;
        if jobs >= policy.max_user_jobs {
            return Err(Rejected {
                kind: RejectKind::Quota,
                message: format!(
                    "排队中的转码作业过多（{jobs}/{}），等前面的转完再传",
                    policy.max_user_jobs
                ),
            }
            .into());
        }
        if bytes.saturating_add(incoming) > policy.max_user_bytes {
            return Err(Rejected {
                kind: RejectKind::Quota,
                message: format!(
                    "排队中的源文件超出个人额度（{} MiB），等前面的转完再传",
                    policy.max_user_bytes / (1024 * 1024)
                ),
            }
            .into());
        }
        let total = self.total_pending_bytes()?;
        if total.saturating_add(incoming) > policy.max_total_bytes {
            return Err(Rejected {
                kind: RejectKind::Quota,
                message: "转码队列已满，稍后再试".to_string(),
            }
            .into());
        }
        Ok(())
    }

    /// 查作业。`None` = 没提交过。
    pub fn job(&self, job_id: &str) -> anyhow::Result<Option<Job>> {
        let db = self.db()?;
        let sql = format!("SELECT {JOB_SELECT_COLUMNS} FROM video_jobs WHERE job_id = ?1");
        let c = db.c();
        let mut stmt = c.prepare(&sql)?;
        let mut rows = stmt.query_map(params![job_id], row_to_job)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// 到点该转的作业。
    pub fn due_jobs(&self, now_ms: i64, limit: u32) -> anyhow::Result<Vec<Job>> {
        let db = self.db()?;
        let c = db.c();
        let mut stmt = c.prepare(DUE_JOBS_SQL.as_str())?;
        let rows = stmt.query_map(params![now_ms, limit as i64], row_to_job)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 领取作业（CAS：只有还是 `pending` 才领得到，两个 worker 不会转同一份）。
    pub fn claim_job(&self, job_id: &str) -> anyhow::Result<bool> {
        let now = wxlike_core::types::now_ms() as i64;
        let db = self.db()?;
        let changed = db.c().execute(
            "UPDATE video_jobs SET state = 'running', updated_at = ?2
             WHERE job_id = ?1 AND state = 'pending'",
            params![job_id, now],
        )?;
        Ok(changed == 1)
    }

    /// 转码成功：写结果、按需武装回调、删掉暂存源字节。
    pub fn succeed_job(&self, job_id: &str, result: &Ingested) -> anyhow::Result<Job> {
        let now = wxlike_core::types::now_ms() as i64;
        let db = self.db()?;
        let job = self.job(job_id)?.context("no such job")?;
        let callback_state = match job.callback_state {
            CallbackState::Delivered => CallbackState::Delivered,
            _ if job.callback_url.is_some() => CallbackState::Pending,
            _ => CallbackState::NotConfigured,
        };
        db.c().execute(
            "UPDATE video_jobs SET state = 'succeeded', result = ?2, last_error = NULL,
                    updated_at = ?3, callback_state = ?4, callback_next_retry_at = ?3
             WHERE job_id = ?1",
            params![
                job_id,
                serde_json::to_string(result)?,
                now,
                callback_state.as_str()
            ],
        )?;
        // 源字节只在"还要重试"时才需要留
        std::fs::remove_dir_all(self.pending_dir(job_id)).ok();
        self.job(job_id)?.context("job vanished after success")
    }

    /// 转码失败：按 [`JobFailure`] 决定退避重试还是落终态。
    pub fn fail_job(&self, job_id: &str, failure: &JobFailure) -> anyhow::Result<Job> {
        let now = wxlike_core::types::now_ms() as i64;
        let job = self.job(job_id)?.context("no such job")?;
        let attempts = if failure.throttled {
            job.attempts
        } else {
            job.attempts.saturating_add(1)
        };
        let terminal = !failure.retryable || attempts >= MAX_JOB_ATTEMPTS;
        let delay = if terminal {
            0
        } else {
            backoff_ms(attempts.max(1))
        };
        let db = self.db()?;
        db.c().execute(
            "UPDATE video_jobs SET state = ?2, attempts = ?3, next_retry_at = ?4,
                    last_error = ?5, updated_at = ?4
             WHERE job_id = ?1",
            params![
                job_id,
                if terminal { "failed" } else { "pending" },
                attempts,
                now + delay as i64,
                truncate_err(&failure.message)
            ],
        )?;
        if terminal {
            // 终态保留源字节：运营点"重试"时不该要求客户端重新上传一遍
            tracing::warn!(job_id, error = %failure.message, "转码作业彻底失败");
        }
        self.job(job_id)?.context("job vanished after failure")
    }

    /// 人工重试：只有终态作业能重置，清零尝试次数并退回 `pending`。
    pub fn requeue_job(&self, job_id: &str) -> anyhow::Result<Option<Job>> {
        let now = wxlike_core::types::now_ms() as i64;
        let db = self.db()?;
        let changed = db.c().execute(
            "UPDATE video_jobs SET state = 'pending', attempts = 0, next_retry_at = ?2,
                    last_error = NULL, updated_at = ?2,
                    callback_state = CASE WHEN callback_url IS NOT NULL THEN 'pending'
                                          ELSE callback_state END,
                    callback_attempts = 0, callback_error = NULL,
                    callback_next_retry_at = ?2
             WHERE job_id = ?1 AND state = 'failed'",
            params![job_id, now],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.job(job_id)
    }

    /// 跑一个作业：读暂存字节 → 走现有阶梯流水线 → 写回结果。
    ///
    /// 复用 [`VideoStore::ingest`]，所以"同步上传"和"作业"是同一条转码路径，
    /// 不存在两套画质。返回的 `Err` 只可能是记账失败（库坏了），业务失败都在
    /// 作业行里。
    pub async fn run_job(&self, job_id: &str) -> anyhow::Result<Job> {
        if !self.claim_job(job_id)? {
            anyhow::bail!("job {job_id} is not claimable (not pending)");
        }
        let job = self.job(job_id)?.context("no such job")?;
        let staged = self.pending_dir(job_id).join("source");
        let bytes = match std::fs::read(&staged) {
            Ok(bytes) => bytes,
            Err(err) => {
                return self.fail_job(
                    job_id,
                    &JobFailure::retryable(format!("read staged source: {err}")),
                );
            }
        };
        if bytes.len() as u64 != job.source_size {
            return self.fail_job(
                job_id,
                &JobFailure::permanent(format!(
                    "staged source is {} bytes, job recorded {}",
                    bytes.len(),
                    job.source_size
                )),
            );
        }
        match self.ingest(&bytes).await {
            Ok(ingested) => self.succeed_job(job_id, &ingested),
            Err(err) => self.fail_job(job_id, &JobFailure::from_err(&err)),
        }
    }

    /// 作业源字节的暂存目录。
    pub fn pending_dir(&self, job_id: &str) -> std::path::PathBuf {
        self.root().join(PENDING_DIR).join(job_id)
    }

    /// 一轮：领作业转码，再把到点的回调发一遍。
    ///
    /// 顺序是有意的——先转码后回调，回调是调用方等的那个通知。
    pub async fn run_worker_round(&self) -> anyhow::Result<WorkerReport> {
        let mut report = WorkerReport::default();
        let now = wxlike_core::types::now_ms() as i64;
        for job in self.due_jobs(now, JOBS_PER_ROUND)? {
            let updated = self.run_job(&job.job_id).await?;
            match updated.state {
                JobState::Succeeded => report.succeeded += 1,
                JobState::Failed => report.failed += 1,
                JobState::Pending => report.retrying += 1,
                JobState::Running => report.retrying += 1,
            }
        }
        for job in self.due_callbacks(now, JOBS_PER_ROUND)? {
            let Some(url) = job.callback_url.clone() else {
                self.record_callback(
                    &job.job_id,
                    &Delivery::Permanent("no callback url".to_string()),
                )?;
                report.callbacks_failed += 1;
                continue;
            };
            let body = CallbackBody::from_job(&job);
            // 提交时查过一次，发之前**再查一次**：只查一次的话，攻击者让域名
            // 先解析到公网、回调时再指向 169.254.169.254 就绕过去了（issue #8）。
            let delivery = match self.policy().callbacks.resolve(&url).await {
                Err(message) => Delivery::Permanent(message),
                Ok(()) => {
                    deliver_callback_signed(&url, &body, self.policy().callbacks.token()).await
                }
            };
            self.record_callback(&job.job_id, &delivery)?;
            match delivery {
                Delivery::Delivered => report.callbacks_delivered += 1,
                Delivery::Permanent(_) => report.callbacks_failed += 1,
                Delivery::Throttled | Delivery::Retryable(_) => report.callbacks_retrying += 1,
            }
        }
        Ok(report)
    }

    /// 到点该发的回调。
    pub fn due_callbacks(&self, now_ms: i64, limit: u32) -> anyhow::Result<Vec<Job>> {
        let db = self.db()?;
        let c = db.c();
        let mut stmt = c.prepare(DUE_CALLBACKS_SQL.as_str())?;
        let rows = stmt.query_map(params![now_ms, limit as i64], row_to_job)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 记一次回调结论。
    pub fn record_callback(&self, job_id: &str, delivery: &Delivery) -> anyhow::Result<()> {
        let now = wxlike_core::types::now_ms() as i64;
        let job = self.job(job_id)?.context("no such job")?;
        let db = self.db()?;
        match delivery {
            Delivery::Delivered => db.c().execute(
                "UPDATE video_jobs SET callback_state = 'delivered', callback_error = NULL,
                        updated_at = ?2 WHERE job_id = ?1",
                params![job_id, now],
            ),
            // 限流：不烧次数，只把下次时间推后
            Delivery::Throttled => db.c().execute(
                "UPDATE video_jobs SET callback_next_retry_at = ?2
                 WHERE job_id = ?1",
                params![
                    job_id,
                    now + backoff_ms(job.callback_attempts.max(1)) as i64
                ],
            ),
            Delivery::Retryable(message) => {
                let attempts = job.callback_attempts.saturating_add(1);
                let terminal = attempts >= MAX_JOB_ATTEMPTS;
                db.c().execute(
                    "UPDATE video_jobs SET callback_state = ?2, callback_attempts = ?3,
                            callback_next_retry_at = ?4, callback_error = ?5, updated_at = ?4
                     WHERE job_id = ?1",
                    params![
                        job_id,
                        if terminal { "failed" } else { "pending" },
                        attempts,
                        now + backoff_ms(attempts) as i64,
                        truncate_err(message)
                    ],
                )
            }
            Delivery::Permanent(message) => db.c().execute(
                "UPDATE video_jobs SET callback_state = 'failed', callback_error = ?2,
                        updated_at = ?3 WHERE job_id = ?1",
                params![job_id, truncate_err(message), now],
            ),
        }?;
        Ok(())
    }

    /// 幂等地起一个 worker（同一个存储根目录只起一个）。
    ///
    /// 谁调：`POST /api/video/jobs` 第一次提交时自起——`server/src/lib.rs` 是
    /// 热点文件，Zone E 不许改。装配分离开独立进程（Q7）后应改成二进制显式
    /// 调用 [`spawn_worker`]。
    pub fn start_worker(&self) {
        use std::collections::HashSet;
        use std::path::PathBuf;
        use std::sync::{Mutex, OnceLock, PoisonError};
        static RUNNING: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
        let set = RUNNING.get_or_init(|| Mutex::new(HashSet::new()));
        let root = self.root().to_path_buf();
        {
            let mut guard = set.lock().unwrap_or_else(PoisonError::into_inner);
            if !guard.insert(root.clone()) {
                return;
            }
        }
        if tokio::runtime::Handle::try_current().is_err() {
            // 不在 tokio 运行时里（单元测试直接调 store）：不该起后台任务
            let mut guard = set.lock().unwrap_or_else(PoisonError::into_inner);
            guard.remove(&root);
            return;
        }
        let store = self.clone();
        let _handle = spawn_worker(store, WORKER_POLL);
    }
}

impl CallbackState {
    fn for_url(url: &Option<String>) -> Self {
        if url.is_some() {
            Self::Pending
        } else {
            Self::None
        }
    }
}

/// 出站回调的共享令牌头（与 `wxlike_platform::video_refs::CALLBACK_TOKEN_HEADER` 同值）。
pub const CALLBACK_TOKEN_HEADER: &str = "x-wxlike-video-token";

/// 回调体：作业号 + 结果。平台侧拿到这一份就能把消息里的视频引用补齐。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct CallbackBody {
    /// 作业号（== video_id）
    pub job_id: String,
    pub state: JobState,
    pub result: Option<Ingested>,
    pub attempts: u32,
    pub updated_at: i64,
}

impl CallbackBody {
    /// 从作业构造回调体。
    pub fn from_job(job: &Job) -> Self {
        Self {
            job_id: job.job_id.clone(),
            state: job.state,
            result: job.result.clone(),
            attempts: job.attempts,
            updated_at: job.updated_at,
        }
    }
}

/// 一次出站回调的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// 2xx，别再发了
    Delivered,
    /// 对端限流：**不烧尝试次数**
    Throttled,
    /// 值得再试（网络/超时/5xx）
    Retryable(String),
    /// 别再试了（4xx：对端明确拒绝）
    Permanent(String),
}

/// 阻塞 IO 的分类结果（小枚举：把 `ureq::Error` 直接带回 `Result` 的
/// `Err` 变体是 272 字节，clippy 会拦，而且调用方本来也不需要整个错误结构）。
enum Sent {
    /// 对端回的状态码（含 ureq 当作错误返回的 4xx/5xx）
    Status(u16),
    /// 地址本身不可能对（协议不认识 / URL 看不懂）
    Unusable(String),
    /// 这次没连通（DNS、连接被拒、超时）
    Transport(String),
}

/// 发一次回调（不带令牌；测试与内部调用用）。
pub async fn deliver_callback(url: &str, body: &CallbackBody) -> Delivery {
    deliver_callback_signed(url, body, None).await
}

/// 发一次回调，带上服务间共享令牌。
///
/// 平台侧的 `/api/video/callback` 是"未配置令牌 = 接口关闭"，所以视频侧不带
/// 这个头的话，配了令牌的部署里回调永远是 401——两边必须成对配置
/// （`WXLIKE_VIDEO_CALLBACK_TOKEN`）。
pub async fn deliver_callback_signed(
    url: &str,
    body: &CallbackBody,
    token: Option<&str>,
) -> Delivery {
    let json = match serde_json::to_string(body) {
        Ok(json) => json,
        Err(err) => return Delivery::Permanent(format!("serialize callback: {err}")),
    };
    let url = url.to_string();
    let token = token.map(str::to_string);
    let sent = tokio::task::spawn_blocking(move || -> Result<Sent, String> {
        // ureq 3：超时是 agent 级配置（顺带白拿连接池），body 用 `send()`。
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_millis(CALLBACK_TIMEOUT_MS)))
            // 4xx/5xx 不当错误：状态码要交给 `classify_status` 分流
            // （429 是限流、5xx 才重试），当成错误就分不出来了。
            .http_status_as_error(false)
            // **不跟随重定向**（issue #8）：跟随等于把"目标白名单"交给对端决定。
            // ureq 2 对 POST 本来就不跟；3 默认会跟（还把 POST 改成 GET），
            // 所以这里显式关掉，顺便让 3xx 落回 `classify_status` 的永久档。
            .max_redirects(0)
            .build()
            .into();
        let mut request = agent.post(&url).header("content-type", "application/json");
        if let Some(token) = token.as_deref() {
            request = request.header(CALLBACK_TOKEN_HEADER, token);
        }
        match request.send(json.as_str()) {
            Ok(response) => Ok(Sent::Status(response.status().as_u16())),
            Err(ureq::Error::BadUri(_)) => {
                // 配置错误重试一万次也是错的
                Ok(Sent::Unusable("callback url is unusable".to_string()))
            }
            Err(err) => Ok(Sent::Transport(format!("callback transport: {err}"))),
        }
    })
    .await;
    match sent {
        Err(err) => Delivery::Retryable(format!("callback task: {err}")),
        Ok(Err(err)) => Delivery::Retryable(err),
        Ok(Ok(Sent::Unusable(message))) => Delivery::Permanent(message),
        Ok(Ok(Sent::Transport(message))) => Delivery::Retryable(message),
        Ok(Ok(Sent::Status(status))) => classify_status(status),
    }
}

/// 状态码 → 结论。
///
/// 429 单独一档是有原因的：限流不是"这条回调发不出去"，是"现在别发"。算成失败
/// 会把额度烧光，最后显示成"回调永久失败"，而实际上再等一秒就到了。
/// 3xx 判永久：ureq 不跟 POST 的重定向，对端"搬家"不是重试能解决的。
fn classify_status(status: u16) -> Delivery {
    if (200..300).contains(&status) {
        Delivery::Delivered
    } else if status == 429 {
        Delivery::Throttled
    } else if (500..600).contains(&status) {
        Delivery::Retryable(format!("callback returned {status}"))
    } else {
        Delivery::Permanent(format!("callback rejected with {status}"))
    }
}

/// 起一个作业 worker（每 `poll` 一轮，直到进程退出）。
pub fn spawn_worker(store: VideoStore, poll: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(poll);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            match store.run_worker_round().await {
                Ok(report) if report == WorkerReport::default() => {}
                Ok(report) => tracing::info!(?report, "video worker 一轮"),
                Err(err) => tracing::error!(error = %err, "video worker 轮次失败"),
            }
        }
    })
}

fn row_to_job(row: &Row<'_>) -> Result<Job, SqliteError> {
    // 状态列是 TEXT：未知值说明库被别的版本写过，必须报错而不是当成 pending
    let state = parse_state_column(row.get::<_, String>(1)?)?;
    let callback_state = parse_callback_state_column(row.get::<_, String>(9)?)?;
    Ok(Job {
        job_id: row.get(0)?,
        owner: row.get(OWNER_COLUMN)?,
        state,
        attempts: row.get::<_, i64>(2)?.max(0) as u32,
        next_retry_at: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        last_error: row.get(6)?,
        source_size: row.get::<_, i64>(7)?.max(0) as u64,
        result: row
            .get::<_, Option<String>>(8)?
            .map(|text| serde_json::from_str(&text))
            .transpose()
            .map_err(|err| {
                SqliteError::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(err))
            })?,
        callback_state,
        callback_url: row.get(10)?,
        callback_attempts: row.get::<_, i64>(11)?.max(0) as u32,
        callback_next_retry_at: row.get(12)?,
        callback_error: row.get(13)?,
    })
}

fn parse_state_column(text: String) -> Result<JobState, SqliteError> {
    JobState::parse(&text).map_err(|err| {
        tracing::error!(column = "state", error = %err, "作业状态列不认识");
        SqliteError::InvalidQuery
    })
}

fn parse_callback_state_column(text: String) -> Result<CallbackState, SqliteError> {
    CallbackState::parse(&text).map_err(|err| {
        tracing::error!(column = "callback_state", error = %err, "回调状态列不认识");
        SqliteError::InvalidQuery
    })
}

/// 错误文本截断（见 [`MAX_ERROR_CHARS`]）。
pub fn truncate_err(message: &str) -> String {
    if message.chars().count() <= MAX_ERROR_CHARS {
        return message.to_string();
    }
    let mut out: String = message.chars().take(MAX_ERROR_CHARS).collect();
    out.push_str("…(truncated)");
    out
}

// ---------------------------------------------------------------------------
// HTTP surface（异步作业面，与同步的 `POST /api/video` 并存）
// ---------------------------------------------------------------------------

pub(crate) fn routes(state: VideoState) -> Router {
    Router::new()
        .route("/api/video/jobs", post(create_job))
        .route("/api/video/jobs/{job_id}", get(get_job))
        .route("/api/video/jobs/{job_id}/retry", post(retry_job))
        .with_state(state)
}

/// 提交一个转码作业：`bytes` 是视频本体，`callback_url` 可选（不填则用服务端的
/// `WXLIKE_VIDEO_CALLBACK_URL`，两者都没有就不回调，作业状态里写明原因）。
///
/// 新入队回 202（后台才转），命中已有对象回 200（已经可播，`newly_queued=false`）。
async fn create_job(
    State(state): State<VideoState>,
    headers: HeaderMap,
    mut form: Multipart,
) -> ApiResult<(StatusCode, Json<JobSubmit>)> {
    let caller = crate::policy::require_caller(state.store.policy(), &headers)?;
    let (bytes, callback_url) = read_job_fields(&mut form)
        .await
        .map_err(ApiError::unprocessable)?;
    if bytes.is_empty() {
        return Err(ApiError::unprocessable("empty upload"));
    }
    let submit = state
        .store
        .submit_job(&bytes, &caller, callback_url.as_deref())
        .map_err(reject_or_internal)?;
    // 后台执行器随第一次提交起来（见 `VideoStore::start_worker`）
    state.store.start_worker();
    let status = if submit.newly_queued {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(submit)))
}

async fn get_job(
    State(state): State<VideoState>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
) -> ApiResult<Json<Job>> {
    let caller = crate::policy::require_caller(state.store.policy(), &headers)?;
    crate::check_id(&job_id)?;
    let job = state
        .store
        .job(&job_id)
        .map_err(|e| ApiError::internal(format!("{e:#}")))?
        .ok_or_else(|| ApiError::not_found("no such job"))?;
    // 别人的作业一律 404：不确认它存在，也不泄露它的状态
    let also_uploaded = state
        .store
        .owns_video(&job_id, &caller)
        .map_err(|e| ApiError::internal(format!("{e:#}")))?;
    if job.owner != caller && !also_uploaded {
        return Err(ApiError::not_found("no such job"));
    }
    Ok(Json(job))
}

/// `submit_job` 的拒绝映射：配额 → 429，回调地址 → 400，其余是服务端故障。
fn reject_or_internal(err: anyhow::Error) -> ApiError {
    if let Some(rejected) = err.downcast_ref::<Rejected>() {
        return match rejected.kind {
            RejectKind::Quota => {
                ApiError::new(StatusCode::TOO_MANY_REQUESTS, rejected.message.clone())
            }
            RejectKind::BadCallback => ApiError::bad_request(rejected.message.clone()),
        };
    }
    ApiError::internal(format!("submit: {err:#}"))
}

/// 人工重试一个终态作业。
///
/// 404 = 没这个作业；409 = 作业还在跑/已成功（重试它会白烧一遍 ffmpeg）；
/// 200 = 已重新入队。
async fn retry_job(
    State(state): State<VideoState>,
    headers: HeaderMap,
    AxumPath(job_id): AxumPath<String>,
) -> ApiResult<Json<Job>> {
    let caller = crate::policy::require_caller(state.store.policy(), &headers)?;
    crate::check_id(&job_id)?;
    let current = state
        .store
        .job(&job_id)
        .map_err(|e| ApiError::internal(format!("{e:#}")))?
        .ok_or_else(|| ApiError::not_found("no such job"))?;
    // 重试会改共享状态：只有提交者本人能按，别人连"它存在"都不该知道
    if current.owner != caller {
        return Err(ApiError::not_found("no such job"));
    }
    if current.state != JobState::Failed {
        return Err(ApiError::conflict(format!(
            "job is {:?}, only a failed job can be retried",
            current.state
        )));
    }
    state
        .store
        .requeue_job(&job_id)
        .map_err(|e| ApiError::internal(format!("{e:#}")))?
        .map(Json)
        .ok_or_else(|| ApiError::conflict("job left the failed state mid-retry"))
}

/// 一次遍历把 `bytes` 与可选的 `callback_url` 都读出来。
///
/// 不能像同步上传那样"读到 bytes 就 return"——作业面允许多带一个字段，
/// 提前返回会把后面的字段留在流里，axum 会把它当成半个 multipart 报错。
async fn read_job_fields(form: &mut Multipart) -> Result<(Vec<u8>, Option<String>), String> {
    let mut bytes: Option<Vec<u8>> = None;
    let mut callback_url: Option<String> = None;
    while let Some(field) = form
        .next_field()
        .await
        .map_err(|e| format!("multipart: {e}"))?
    {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "bytes" => {
                if bytes.is_some() {
                    return Err("duplicate `bytes` field".into());
                }
                bytes = Some(
                    field
                        .bytes()
                        .await
                        .map(|b| b.to_vec())
                        .map_err(|e| format!("read bytes: {e}"))?,
                );
            }
            "callback_url" => {
                let text = String::from_utf8_lossy(
                    &field
                        .bytes()
                        .await
                        .map_err(|e| format!("read callback_url: {e}"))?,
                )
                .trim()
                .to_string();
                if !text.is_empty() {
                    if !(text.starts_with("http://") || text.starts_with("https://")) {
                        return Err("callback_url must be an http(s) URL".into());
                    }
                    callback_url = Some(text);
                }
            }
            other => return Err(format!("unexpected field `{other}`")),
        }
    }
    let bytes = bytes.ok_or_else(|| "missing `bytes` field".to_string())?;
    Ok((bytes, callback_url))
}
