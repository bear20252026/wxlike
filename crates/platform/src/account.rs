//! G02-05 账户四面（#114）：资料、偏好、数据导出、注销。
//!
//! 规则**不在这个文件里**——它们在 `wxlike-identity::preferences`（偏好与作业）和
//! `wxlike-security::privacy`（可见性与通知判定）。审计卡 #114 的原文是"规则有、
//! 面没有"，所以本模块的第一责任是**不在生产端另起一套口径**：
//!
//! * 偏好只认 `SERVER_KEYS` 白名单，不认识的键 422，不静默存；
//! * "别人能不能搜到我、通知投不投"一律问 `privacy::*`，不在这里重读一遍键；
//! * 作业状态一律从库里读原文判定；读不出来是 500，**不折成 `export`/`queued`**
//!   （旧代码的 `unwrap_or(JobKind::Export)` 能把一条注销作业读成可下载的导出）；
//! * 导出与注销只认 step-up 通行证（契约 fixture `jobRequest` 的
//!   `step_up_purpose`），不另开"顺手带个口令"的第二条验证通道。
//!
//! # 导出为什么不在请求路径里产出
//!
//! `POST /api/account/export` 只回执作业 handle（202）。打包由
//! [`Platform::run_account_jobs`] 推进——与红包清扫 [`crate::spawn_redpacket_sweeper`]
//! 同一个模式：服务端起周期任务，测试直接调函数，状态推进因此是确定的。
//! 取件走 `export_link`/`redeem_link`（**短时 + 单次**）；产物按自身内容摘要
//! 存在媒体仓里，`artifact` 就是那个摘要，不是路径，所以没有穿越面。
//! 媒体仓写不进去（断网/供应商故障）时作业是 `failed` + 原因，
//! **不会**留下 `done` 或产物地址。
//!
//! # 注销的顺序
//!
//! [`Platform::schedule_deletion`] 先按既有规则撤销**全部会话 + 刷新链 + step-up**
//! （`preferences::request_delete`），再落 `users.deleted_at`，最后排清理作业。
//! 标记一落，口令校验与会话签发同时 fail-closed，所以**不等 worker 跑完，数据
//! 也已经取不到**。用户行本身留到清理作业完成：`groups.owner_uid`、
//! `communities.owner_uid`、`threads.created_by` 这几条外键没有 CASCADE，硬删
//! 用户行会把无关的域顶坏。重复申请 → 409，不做第二次动作。
//!
//! # 取消的边界
//!
//! 排队中的导出可以撤回（`POST /api/account/jobs/{id}/cancel`）。注销作业按种类
//! 拒绝撤回，理由与上面一致：撤销凭证没有回滚路径，"取消注销"会造出一个既没
//! 登录能力又显示为正常的账号。

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wxlike_identity::preferences::{
    self, Job, JobKind, JobStatus, ServerPrefs, EXPORT_SCOPES, PURGE_DOMAINS, SERVER_KEYS,
};
use wxlike_identity::throttle;
use wxlike_security::privacy::{
    self as privacy_rule, MapPrefs, NotifyKind, PrefSource, PrivacyPrefs, SearchKey, Viewer,
    Visibility,
};

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError, Result};
use crate::extract::ApiJson;
use crate::Platform;

/// 一次偏好同步最多返回多少条增量（游标翻页）。
const MAX_PREF_SYNC_PAGE: i64 = 200;
/// 一轮 worker 最多推进几条作业（不让一次调用长期占住连接）。
const MAX_JOBS_PER_RUN: usize = 32;
/// 失败原因入库前的长度上限（`identity::check_label` 只收 64 个字符以内）。
const MAX_FAIL_REASON_CHARS: usize = 60;
/// 设备 id 的长度上限（与 `sessions` 模块同口径）。
const MAX_DEVICE_ID_CHARS: usize = 64;
/// step-up 口令闸门：同账号窗口内允许的失败次数。
const STEP_UP_MAX_ATTEMPTS: i64 = 5;
/// step-up 口令闸门的固定窗口长度。
const STEP_UP_WINDOW_MS: i64 = 5 * 60 * 1000;
/// 达到上限后的封禁时长。
const STEP_UP_BLOCK_MS: i64 = 15 * 60 * 1000;
/// 注销后 display name 的替换前缀（真名是个人数据，不能留）。
const DELETED_NAME_PREFIX: &str = "deleted-";

pub fn routes() -> Router<crate::PlatformState> {
    Router::new()
        .route(
            "/api/preferences",
            get(get_preferences).put(put_preferences),
        )
        .route("/api/preferences/sync", get(sync_preferences))
        // profile.update 的**路由**在 users.rs（G02 自有文件，WP-01 账号端点），
        // 这里不再注册第二遍——两个 /api/profile 就是两套口径。
        // 本卡补的是它的 G02-05 语义：审计、已注销账号拒写、空补丁报错。
        .route("/api/account/step-up", post(step_up_grant))
        .route("/api/account/export", post(request_export))
        .route("/api/account/export/{job_id}", get(export_status))
        .route("/api/account/export/{job_id}/link", post(create_link))
        .route("/api/account/export/download/{token}", get(download))
        .route("/api/account/jobs", get(list_jobs))
        .route("/api/account/jobs/{job_id}/cancel", post(cancel_job))
        .route("/api/account/delete", post(delete_account))
}

/// 本卡两个敏感动作的 step-up 用途。字符串与 `identity::step_up::PURPOSES`
/// 以及契约 fixture `jobRequest.step_up_purpose` 的 enum 完全一致——
/// 用途名一旦分叉，"要验证"就变成两张互不相认的通行证。
pub const PURPOSE_EXPORT: &str = "account.export";
pub const PURPOSE_DELETE: &str = "account.delete";
/// 本卡可授予的 step-up 用途（封闭集合；`device.revoke` 归 #103）。
const GRANTABLE_PURPOSES: [&str; 2] = [PURPOSE_EXPORT, PURPOSE_DELETE];

/// 作业对外的形状。`kind`/`status` 只有在库里写得出来时才存在——
/// 一条坏行进不到这里（`preferences::job` 就已经报错了）。
#[derive(Serialize, Debug)]
pub struct JobReply {
    pub job_id: String,
    pub kind: String,
    pub status: String,
    pub requested_at: i64,
    pub bytes: i64,
    /// 产物已就绪、可以签下载链接（只有导出可能为真）。
    pub download_ready: bool,
    pub error: String,
}

impl JobReply {
    fn of(job: &Job) -> Self {
        Self {
            job_id: job.id.clone(),
            kind: job.kind.as_str().to_string(),
            status: job.status.as_str().to_string(),
            requested_at: job.requested_at,
            bytes: job.bytes,
            download_ready: job.kind == JobKind::Export && job.status == JobStatus::Done,
            error: job.error.clone(),
        }
    }
}

#[derive(Deserialize, Default)]
struct PreferencesQuery {
    /// 客户端本地时区此刻是否在免打扰时段（服务端不知道用户的本地时间，
    /// 所以窗口判定归客户端，**投递/响铃/预览的判定仍归策略层**）。
    ///
    /// 这里保留 `Option`：缺参数与 `=false` **不是**同一个答案。旧写法
    /// `unwrap_or(false)` 会把"客户端没告诉服务端"折成"现在不在免打扰时段"，
    /// 于是响应里响铃判定看起来是确定的，实际是猜的。
    #[serde(default)]
    quiet_hours: Option<bool>,
}

#[derive(Deserialize, Default)]
struct PreferencesPatch {
    /// 字段级补丁：只改列出的键，其余不动（两台设备各改各的不互相覆盖）。
    /// 契约 fixture 里的名字是 `values`，两个都收，但**必须且只能给一个**。
    #[serde(default)]
    patch: Option<BTreeMap<String, String>>,
    #[serde(default)]
    values: Option<BTreeMap<String, String>>,
    /// 这次变更归属哪台设备；留空则用当前会话记录的设备。
    #[serde(default)]
    device_id: String,
}

impl PreferencesPatch {
    /// 取补丁本体。少了键、两个键都给、给成空补丁，一律算客户端漏了字段——
    /// 当"什么都不改"处理会把一个 bug 变成一次静默的成功。
    fn patch(&self) -> Result<&BTreeMap<String, String>> {
        match (&self.patch, &self.values) {
            (Some(_), Some(_)) => Err(PlatformError::BadInput("patch or values, not both")),
            (None, None) => Err(PlatformError::BadInput("patch or values required")),
            (Some(p), None) | (None, Some(p)) if p.is_empty() => {
                Err(PlatformError::BadInput("empty patch"))
            }
            (Some(p), None) | (None, Some(p)) => Ok(p),
        }
    }
}

#[derive(Deserialize, Default)]
struct SyncQuery {
    #[serde(default)]
    since_seq: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
}

/// 契约 fixture `jobRequest`：敏感动作只认 step-up 通行证，不给"顺手带个口令"
/// 留第二条通道——两条验证路径迟早分叉成两套口径。
#[derive(Deserialize, Default)]
struct JobRequest {
    #[serde(default)]
    step_up_purpose: String,
}

#[derive(Deserialize, Default)]
struct DeleteRequest {
    #[serde(default)]
    step_up_purpose: String,
    /// 显式确认位：缺省 false。少这一位，一个带 Bearer 的脚本就能把账号抹掉。
    #[serde(default)]
    confirm: bool,
}

#[derive(Deserialize, Default)]
struct StepUpGrantBody {
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    totp_code: String,
}

async fn get_preferences(
    State(platform): State<crate::PlatformState>,
    authed: Authed,
    Query(q): Query<PreferencesQuery>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(
        platform.preferences_document(&authed.user_id, q.quiet_hours)?,
    ))
}

async fn put_preferences(
    State(platform): State<crate::PlatformState>,
    authed: Authed,
    Json(body): Json<PreferencesPatch>,
) -> std::result::Result<Json<Value>, ApiError> {
    let patch = body.patch()?;
    let device = platform.patch_device(&authed, &body.device_id)?;
    let keys = patch.keys().cloned().collect::<Vec<_>>();
    platform.patch_preferences(&authed.user_id, patch, &device)?;
    // 审计只记改了哪些键——白名单内的键名不是个人数据，**值**才是。
    platform.audit(
        &authed.user_id,
        "preferences.patch",
        "preferences",
        &keys.join(","),
    );
    Ok(Json(platform.preferences_document(&authed.user_id, None)?))
}

async fn sync_preferences(
    State(platform): State<crate::PlatformState>,
    authed: Authed,
    Query(q): Query<SyncQuery>,
) -> std::result::Result<Json<Value>, ApiError> {
    let since = q.since_seq.unwrap_or(0);
    let limit = q
        .limit
        .unwrap_or(MAX_PREF_SYNC_PAGE)
        .clamp(1, MAX_PREF_SYNC_PAGE);
    let (rows, head) = platform.preferences_since(&authed.user_id, since, limit)?;
    let changes = rows
        .iter()
        .map(|r| {
            json!({
                "seq": r.seq, "key": r.key, "value": r.value,
                "updated_at": r.updated_at, "device_id": r.device_id,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "changes": changes, "head_seq": head })))
}

/// `POST /api/account/step-up`：为导出 / 注销换一张短时、单次的通行证。
///
/// 验证本体在 `identity::step_up`，口令/TOTP 的判定在 `security::confirm_identity`。
/// 两个凭证都没给 → 422，不把"忘了带凭据"当成验证通过；sid 取不到 → 401，
/// 不 `unwrap_or_default()` 造一个空 sid 的授权（那张证谁都能领）。
async fn step_up_grant(
    State(platform): State<crate::PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<StepUpGrantBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    if !GRANTABLE_PURPOSES.contains(&body.purpose.as_str()) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown step-up purpose",
        ));
    }
    if body.password.is_empty() && body.totp_code.is_empty() {
        platform.audit(
            &authed.user_id,
            "account.step_up",
            &body.purpose,
            "missing_credentials",
        );
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "password or totp_code required",
        ));
    }
    // 先闸后验：这个端点是一张**口令预言机**（持有会话的人可以离线重试到
    // 猜中），不闸就等于免费送第二条撞库通道。
    platform.step_up_gate(&authed.user_id)?;
    if !platform.confirm_identity(&authed.user_id, &body.totp_code, &body.password)? {
        platform.step_up_denied(&authed.user_id);
        platform.audit(&authed.user_id, "account.step_up", &body.purpose, "denied");
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid credentials",
        ));
    }
    platform.step_up_passed(&authed.user_id);
    let sid = platform.account_sid(&authed.token)?;
    let expires_at = platform.grant_step_up(&authed.user_id, &sid, &body.purpose)?;
    // 审计只记"给哪张用途发了证"。口令一个字节都不进审计，也不进日志。
    platform.audit(&authed.user_id, "account.step_up", &body.purpose, "granted");
    Ok(Json(
        json!({ "purpose": body.purpose, "expires_at": expires_at }),
    ))
}

async fn request_export(
    State(platform): State<crate::PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<JobRequest>,
) -> std::result::Result<(StatusCode, Json<JobReply>), ApiError> {
    platform.take_step_up(&authed, PURPOSE_EXPORT, &body.step_up_purpose)?;
    let job = platform.request_export_job(&authed.user_id)?;
    platform.audit(&authed.user_id, "account.export.requested", &job.id, "");
    Ok((StatusCode::ACCEPTED, Json(JobReply::of(&job))))
}

async fn export_status(
    State(platform): State<crate::PlatformState>,
    authed: Authed,
    Path(job_id): Path<String>,
) -> std::result::Result<Json<JobReply>, ApiError> {
    Ok(Json(JobReply::of(
        &platform.account_job(&authed.user_id, &job_id)?,
    )))
}

async fn create_link(
    State(platform): State<crate::PlatformState>,
    authed: Authed,
    Path(job_id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let (raw, expires_in_ms) = platform.create_export_link(&authed.user_id, &job_id)?;
    platform.audit(&authed.user_id, "account.export.link", &job_id, "");
    Ok(Json(json!({
        "download_token": raw,
        "url": format!("/api/account/export/download/{raw}"),
        "expires_in_ms": expires_in_ms,
        "single_use": true,
    })))
}

/// 取件。**故意不要求 Bearer**：注销之后取自己的导出是正当需求，而那时令牌
/// 已经被撤销。防外泄靠 `redeem_link` 的单次 + 10 分钟。
async fn download(
    State(platform): State<crate::PlatformState>,
    Path(token): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let bytes = platform.take_export_download(&token).await?;
    Ok(Json(
        serde_json::from_slice(&bytes).map_err(PlatformError::Codec)?,
    ))
}

async fn list_jobs(
    State(platform): State<crate::PlatformState>,
    authed: Authed,
) -> std::result::Result<Json<Value>, ApiError> {
    let jobs = platform.account_jobs(&authed.user_id)?;
    let list = jobs.iter().map(JobReply::of).collect::<Vec<_>>();
    Ok(Json(
        json!({ "jobs": list, "purge_domains": PURGE_DOMAINS }),
    ))
}

/// `POST /api/account/jobs/{id}/cancel`：撤回一条**还在排队**的作业。
///
/// 判定全在 `preferences::cancel`（只有 queued 可撤、注销作业按种类拒绝、
/// 别人的作业与不存在同形）。这里只搬运，不重新决定。
async fn cancel_job(
    State(platform): State<crate::PlatformState>,
    authed: Authed,
    Path(job_id): Path<String>,
) -> std::result::Result<Json<JobReply>, ApiError> {
    let job = platform.cancel_account_job(&authed.user_id, &job_id)?;
    platform.audit(
        &authed.user_id,
        "account.job.cancel",
        &job.id,
        job.kind.as_str(),
    );
    Ok(Json(JobReply::of(&job)))
}

/// `POST /api/account/delete`：step-up 通行证 + `confirm` 双确认，然后落标记、
/// 撤销全部凭证。**这个响应就是这个令牌最后一次能认证成功。**
async fn delete_account(
    State(platform): State<crate::PlatformState>,
    authed: Authed,
    ApiJson(body): ApiJson<DeleteRequest>,
) -> std::result::Result<(StatusCode, Json<Value>), ApiError> {
    if !body.confirm {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "confirm must be true",
        ));
    }
    platform.take_step_up(&authed, PURPOSE_DELETE, &body.step_up_purpose)?;
    let (job, revoked) = platform.schedule_deletion(&authed.user_id)?;
    platform.audit(
        &authed.user_id,
        "account.delete",
        &job.id,
        &format!("revoked_sessions={revoked}"),
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "job": JobReply::of(&job),
            "revoked_sessions": revoked,
            "purge_domains": PURGE_DOMAINS,
            "login_revoked": true,
        })),
    ))
}

impl Platform {
    /// 偏好文档：原始值 + 同步游标 + 由策略层算出来的生效判定。
    ///
    /// `quiet_hours` 是 `Option`：`None` = 客户端没说，此时响铃判定是**在"不在
    /// 免打扰时段"这个假设下**算出来的，文档里用 `quiet_hours_known: false`
    /// 明说，不把它伪装成确定答案。
    pub fn preferences_document(&self, user_id: &str, quiet_hours: Option<bool>) -> Result<Value> {
        let c = self.db().lock();
        let prefs = preferences::get(&c, user_id)?;
        let head = preferences::head_seq(&c, user_id)?;
        let quiet = quiet_hours.unwrap_or(false);
        Ok(json!({
            "values": prefs.values,
            "head_seq": head,
            "server_keys": SERVER_KEYS,
            "export_scopes": EXPORT_SCOPES,
            "quiet_hours_known": quiet_hours.is_some(),
            "privacy": privacy_view(&prefs),
            "notify": notify_view(&prefs, quiet),
        }))
    }

    /// 写一次字段级补丁（键不在白名单 → 422，值畸形 → 422）。
    pub fn patch_preferences(
        &self,
        user_id: &str,
        patch: &BTreeMap<String, String>,
        device_id: &str,
    ) -> Result<()> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        preferences::patch(&c, user_id, patch, device_id, now)?;
        Ok(())
    }

    /// 增量：`since_seq` 之后的变更 + 当前游标（下次从这里接着拉）。
    pub fn preferences_since(
        &self,
        user_id: &str,
        since_seq: i64,
        limit: i64,
    ) -> Result<(Vec<preferences::PrefRow>, i64)> {
        let c = self.db().lock();
        let rows = preferences::since(&c, user_id, since_seq, limit)?;
        Ok((rows, preferences::head_seq(&c, user_id)?))
    }

    /// 补丁归属的设备：body 给了就用 body 的，否则用当前会话里记的那台。
    ///
    /// **会话行找不到就报错**（401），不 `unwrap_or_default()` 造一台"空设备"：
    /// 那条路径意味着令牌刚被撤销或库里的归属信息没了，把变更记到一台不存在的
    /// 设备上，跨端同步就永远查不出是谁改的。
    fn patch_device(&self, authed: &Authed, provided: &str) -> Result<String> {
        if !provided.is_empty() {
            check_device_id(provided)?;
            return Ok(provided.to_string());
        }
        let c = self.db().lock();
        let stored: Option<Option<String>> = c
            .query_row(
                "SELECT device_id FROM sessions WHERE token = ?1",
                params![crate::auth::token_digest(&authed.token)],
                |r| r.get(0),
            )
            .optional()?;
        match stored {
            // 行在、`device_id` 是 NULL/空 = 历史会话确实没有这个字段，如实记空。
            Some(value) => Ok(value.unwrap_or_default()),
            None => Err(PlatformError::Unauthorized),
        }
    }

    /// 当前令牌对应的会话 sid。**取不到就 401**。
    ///
    /// `sessions.rs` 里用 `unwrap_or_default()` 兜出一个空 sid；step-up 的主键是
    /// `(user_id, sid, purpose)`，空 sid 的授权一旦签发，任何"同样取不到 sid"的
    /// 请求都能消费它——通行证就此与设备脱钩。本卡不复用那个兜底。
    fn account_sid(&self, token: &str) -> Result<String> {
        self.session_sid(token)?.ok_or(PlatformError::Unauthorized)
    }

    /// step-up 的口令闸门（固定窗口 + 封禁，规则本体在 `identity::throttle`）。
    ///
    /// 桶键按账号，不按 IP：共用出口地址（公司/校园 NAT）下按 IP 封会把一整层
    /// 用户一起关在门外，而撞库的目标始终是**这个账号**。
    fn step_up_gate(&self, user_id: &str) -> Result<()> {
        let limit = throttle::Limit {
            max_attempts: STEP_UP_MAX_ATTEMPTS,
            window_ms: STEP_UP_WINDOW_MS,
            block_ms: STEP_UP_BLOCK_MS,
        };
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        throttle::check(&c, &step_up_bucket(user_id), limit, now)?;
        Ok(())
    }

    /// 记一次验证失败（达到阈值即封禁；封禁这件事要留在日志里）。
    fn step_up_denied(&self, user_id: &str) {
        let limit = throttle::Limit {
            max_attempts: STEP_UP_MAX_ATTEMPTS,
            window_ms: STEP_UP_WINDOW_MS,
            block_ms: STEP_UP_BLOCK_MS,
        };
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        match throttle::record_failure(&c, &step_up_bucket(user_id), limit, now) {
            Ok(status) if status.blocked_until > now => {
                tracing::warn!(user = %user_id, until = status.blocked_until, "step-up 口令闸门封禁");
            }
            Err(err) => tracing::error!(error = %err, "step-up 闸门记不上账"),
            _ => {}
        }
    }

    /// 验证通过即清零，不惩罚正常用户。
    fn step_up_passed(&self, user_id: &str) {
        let c = self.db().lock();
        if let Err(err) = throttle::reset(&c, &step_up_bucket(user_id)) {
            tracing::error!(error = %err, "step-up 闸门清不掉");
        }
    }

    /// 校验用途串并消费一次通行证（单次）。
    ///
    /// 先比用途、后消费：一个写错的 `step_up_purpose` 不该把另一张**有效**的
    /// 通行证吃掉（那会让用户以为"我验过了，怎么还要验"）。
    fn take_step_up(&self, authed: &Authed, want: &'static str, provided: &str) -> Result<()> {
        if provided != want {
            return Err(PlatformError::BadInput("step_up_purpose"));
        }
        let sid = self.account_sid(&authed.token)?;
        self.consume_step_up(&authed.user_id, &sid, want)
    }

    /// 申请导出。已有排队/执行中的导出时返回同一条作业（不重复打包）。
    pub fn request_export_job(&self, user_id: &str) -> Result<Job> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        if deleted_at(&c, user_id)?.is_some() {
            return Err(PlatformError::Blocked("account scheduled for deletion"));
        }
        Ok(preferences::request_export(&c, user_id, now)?)
    }

    /// 撤回一条排队中的作业。判定全在 `preferences::cancel`，这里只做
    /// 错误形状的翻译（409 而不是 409 "already exists: …"）。
    pub fn cancel_account_job(&self, user_id: &str, job_id: &str) -> Result<Job> {
        validate_id(job_id)?;
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        preferences::cancel(&c, job_id, user_id, now).map_err(|err| match err {
            wxlike_identity::IdError::Conflict(what) => PlatformError::Blocked(what),
            other => other.into(),
        })
    }

    /// 本人的一条作业。**别人的作业一律 404**：作业 id 不是存在性探测面
    /// （与 `/api/sessions/{sid}/revoke` 同一口径）。
    pub fn account_job(&self, user_id: &str, job_id: &str) -> Result<Job> {
        validate_id(job_id)?;
        let c = self.db().lock();
        let job = preferences::job(&c, job_id)?.ok_or(PlatformError::NotFound)?;
        if job.user_id != user_id {
            return Err(PlatformError::NotFound);
        }
        Ok(job)
    }

    pub fn account_jobs(&self, user_id: &str) -> Result<Vec<Job>> {
        let c = self.db().lock();
        Ok(preferences::list(&c, user_id)?)
    }

    /// 为已完成的导出签一个短时单次下载令牌。未完成 → 403（`export not ready`）。
    pub fn create_export_link(&self, user_id: &str, job_id: &str) -> Result<(String, i64)> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        let job = preferences::job(&c, job_id)?.ok_or(PlatformError::NotFound)?;
        if job.user_id != user_id {
            return Err(PlatformError::NotFound);
        }
        let raw = preferences::export_link(&c, job_id, user_id, now)?;
        Ok((raw, preferences::EXPORT_LINK_TTL_MS))
    }

    /// 兑换下载令牌并读出产物。第二次兑换 401（单次）。
    pub async fn take_export_download(&self, raw: &str) -> Result<Vec<u8>> {
        validate_id(raw)?;
        let now = wxlike_core::types::now_ms() as i64;
        let artifact = {
            let c = self.db().lock();
            preferences::redeem_link(&c, raw, now)?
        };
        self.read_artifact(&artifact).await
    }

    /// 按内容摘要从媒体仓取产物。取不到就是错误——**不返回空文档假装导出成功**。
    async fn read_artifact(&self, artifact: &str) -> Result<Vec<u8>> {
        let store = self.media_store();
        let size = store.size_of(artifact).await?;
        let Some(size) = size else {
            return Err(PlatformError::NotFound);
        };
        // 0 字节的产物不可能是合法导出：交出去等于一次成功的空导出。
        if size == 0 {
            return Err(PlatformError::Internal(
                "export artifact is empty".to_string(),
            ));
        }
        let bytes = store.get(artifact, 0, size - 1).await?;
        bytes.ok_or(PlatformError::NotFound)
    }

    /// 落注销标记并撤销全部会话/刷新链/step-up。返回作业与撤销的会话数。
    ///
    /// 三种状态分得很清楚，**没有"静默再做一遍"**：
    ///
    /// 1. 标记已落 + 还有排队/执行中的注销作业 → 409 `Blocked`，什么都不做。
    ///    重做意味着再撤销一遍会话，那会把"注销之后又用恢复码登录的另一台设备"
    ///    踢掉——看起来像随机掉线，谁也查不出来。
    /// 2. 标记已落 + 没有在排队的作业（上一次清理失败了，比如钱包还有余额）
    ///    → 重新排一条作业。这是**重试通道**，不再撤销任何会话。
    /// 3. 没标记 → 正常流程：先撤凭证，再落标记。
    pub fn schedule_deletion(&self, user_id: &str) -> Result<(Job, usize)> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        if deleted_at(&c, user_id)?.is_some() {
            if let Some(job) = open_delete_job(&c, user_id)? {
                tracing::warn!(
                    job = %job.id,
                    user = %user_id,
                    "重复的注销申请：作业已在排，一步都不重做"
                );
                return Err(PlatformError::Blocked("account already scheduled"));
            }
            // 重试：`request_delete` 里撤销的都是已经撤销过的行，会话数按 0 报。
            let job = preferences::request_delete(&c, user_id, now)?;
            return Ok((job, 0));
        }
        let revoked = c.query_row(
            "SELECT COUNT(*) FROM sessions WHERE user_id = ?1",
            params![user_id],
            |r| r.get::<_, i64>(0),
        )? as usize;
        // 规则本体在 identity：先撤凭证，再排清理作业。
        let job = preferences::request_delete(&c, user_id, now)?;
        let n = c.execute(
            "UPDATE users SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
            params![user_id, now],
        )?;
        if n != 1 {
            // 并发：另一个请求已经落了标记（作业是同一条，`find_open` 去重）。
            // 状态是一致的，但这次调用确实没有做成任何事——如实报错。
            return Err(PlatformError::Blocked("account already scheduled"));
        }
        Ok((job, revoked))
    }

    /// 推进作业队列：导出打包、注销清理。返回本轮完成的条数。
    ///
    /// 与 [`crate::spawn_redpacket_sweeper`] 同一模式：服务端起周期任务，测试
    /// 直接调用本函数，于是"排队 → 执行中 → 完成/失败"的推进在测试里是确定的。
    /// 第一条失败即停止，并把那条作业标成 `failed`（用户能看到为什么）。
    pub async fn run_account_jobs(&self) -> Result<usize> {
        let mut done = 0;
        while done < MAX_JOBS_PER_RUN {
            let claimed = {
                let c = self.db().lock();
                preferences::claim(&c, wxlike_core::types::now_ms() as i64)?
            };
            let Some(job) = claimed else {
                break;
            };
            let outcome = match job.kind {
                JobKind::Export => self.run_export_job(&job).await,
                JobKind::Delete => self.run_delete_job(&job),
            };
            if let Err(err) = outcome {
                self.mark_job_failed(&job.id, &err.to_string());
                tracing::error!(job = %job.id, error = %err, "account job failed");
                return Err(err);
            }
            done += 1;
        }
        Ok(done)
    }

    /// 打包本人可见数据 → 按内容摘要入仓 → 作业完成。
    ///
    /// 顺序很讲究：先算文档（要拿数据库锁），再 await 写媒体仓，最后重新取锁
    /// 收尾——锁不能跨 `.await`（见 db.rs 模块头）。
    async fn run_export_job(&self, job: &Job) -> Result<()> {
        let bytes = self.export_document(&job.user_id)?;
        let sha = wxlike_core::msgmeta::sha256_hex(&bytes);
        if let Err(err) = self.media_store().put(&sha, &bytes).await {
            // 底层 io/sqlite 文本只进服务端日志（它可能带路径），跨请求边界的那句
            // 用固定措辞——用户在作业列表里看得到"为什么没导出来"，
            // 但拿不到服务端文件系统形状。
            tracing::error!(job = %job.id, error = %err, "export: 媒体仓写入失败");
            return Err(PlatformError::Internal("export storage unreachable".into()));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        preferences::finish(&c, &job.id, &sha, bytes.len() as i64, now)?;
        Ok(())
    }

    /// 执行注销清理。整个清理 + 脱敏 + 匿名化在**一个事务**里：任何一条语句
    /// 失败（含外键冲突）就整体回滚，作业记 `failed`，绝不留"删了一半"。
    fn run_delete_job(&self, job: &Job) -> Result<()> {
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        // 没有标记绝不清理：一条凭空出现的 delete 作业不属于注销流程。
        if deleted_at(&c, &job.user_id)?.is_none() {
            return Err(PlatformError::Blocked("deletion was never scheduled"));
        }
        // 余额没清零就不清钱包：静默销毁余额是不可接受的"清理成功"。
        if wallet_balance(&c, &job.user_id)? != 0 {
            return Err(PlatformError::Blocked("wallet not empty"));
        }
        let tx = c.unchecked_transaction()?;
        let counts = purge_rows(&tx, &job.user_id)?;
        mask_rows(&tx, &job.user_id)?;
        anonymize_user(&tx, &job.user_id)?;
        tx.commit()?;
        let total: i64 = counts.iter().map(|(_, n)| *n as i64).sum();
        // 注销作业的 artifact 存的是"每个域清掉多少行"的摘要，不是下载地址
        // （`export_link` 只认 kind = export，所以这里不会被误当成产物路径）。
        let summary = serde_json::to_string(&counts)?;
        preferences::finish(&c, &job.id, &summary, total, now)?;
        Ok(())
    }

    fn mark_job_failed(&self, job_id: &str, reason: &str) {
        let trimmed: String = reason.chars().take(MAX_FAIL_REASON_CHARS).collect();
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock();
        if let Err(err) = preferences::fail(&c, job_id, &trimmed, now) {
            tracing::error!(job = %job_id, error = %err, "account job: 连失败原因都没写进去");
        }
    }

    /// 导出文档：`EXPORT_SCOPES` 六个域逐个从真实表取，一个都不编。
    ///
    /// 不设行数上限——"你的数据"少一行就是一份假的成功导出；体积失控由
    /// 媒体仓与下载链接的 10 分钟窗口承担，不是由裁剪内容承担。
    ///
    /// `pub`：worker 用它，验收测试也要直接读一遍产物（"导出只含本人可见数据、
    /// 不含凭据列"这条只能在内容上断言，看状态字段是不够的）。
    pub fn export_document(&self, user_id: &str) -> Result<Vec<u8>> {
        let c = self.db().lock();
        let profile = rows_json(&c, SQL_PROFILE, user_id, PROFILE_COLS)?;
        let Some(owner) = profile.first().cloned() else {
            // 本人行读不到 = 这份导出没有内容可言，必须失败，不能交空包。
            return Err(PlatformError::NotFound);
        };
        let prefs = preferences::get(&c, user_id)?;
        let settings = json!({
            "preferences": prefs.values,
            "legacy_user_settings": rows_json(&c, SQL_USER_SETTINGS, user_id, USER_SETTINGS_COLS)?,
            "privacy": privacy_view(&prefs),
        });
        let doc = json!({
            "user_id": user_id,
            "exported_at_ms": wxlike_core::types::now_ms(),
            "scopes": EXPORT_SCOPES,
            "profile": owner,
            "contacts": rows_json(&c, SQL_CONTACTS, user_id, CONTACTS_COLS)?,
            "messages": rows_json(&c, SQL_MESSAGES, user_id, MESSAGES_COLS)?,
            "media_manifest": rows_json(&c, SQL_MEDIA, user_id, MEDIA_COLS)?,
            "ledger": rows_json(&c, SQL_LEDGER, user_id, LEDGER_COLS)?,
            "settings": settings,
        });
        Ok(serde_json::to_vec_pretty(&doc)?)
    }
}

/// 已注销标记。`None` = 没注销（或没有这个用户——调用方都得当成"不许清理"）。
fn deleted_at(conn: &Connection, user_id: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT deleted_at FROM users WHERE id = ?1",
            params![user_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

/// 有没有还在排队/执行中的注销作业。用的就是 `preferences::list` 这条
/// 既有查询，不另开一条 SQL——判"是否已在注销"的口径只能有一处。
fn open_delete_job(conn: &Connection, user_id: &str) -> Result<Option<Job>> {
    Ok(preferences::list(conn, user_id)?.into_iter().find(|j| {
        j.kind == JobKind::Delete && matches!(j.status, JobStatus::Queued | JobStatus::Running)
    }))
}

/// 钱包余额。用 SQL 的 COALESCE 表达"没有钱包行就是 0"，不在 Rust 里
/// `unwrap_or(0)`——那种写法会把"读库失败"也读成 0（然后真的去删钱包）。
fn wallet_balance(conn: &Connection, user_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE((SELECT balance FROM wallets WHERE user_id = ?1), 0)",
        params![user_id],
        |r| r.get(0),
    )?)
}

/// step-up 口令桶：账号维度，前缀与登录桶隔开（`identity::throttle` 的键约定）。
fn step_up_bucket(user_id: &str) -> String {
    format!("step-up:{}", throttle::key_user(user_id))
}

fn check_device_id(s: &str) -> Result<()> {
    if s.is_empty()
        || s.chars().count() > MAX_DEVICE_ID_CHARS
        || s.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(PlatformError::BadInput("device_id"));
    }
    Ok(())
}

/// 作业 id / 下载令牌都是随机 hex，形状检查按生成式标识符的口径来。
fn validate_id(s: &str) -> Result<()> {
    crate::validate::id(s).map(|_| ())
}

/// 一条只带一个用户 id 参数的查询 → JSON 数组，列名由调用方给。
fn rows_json(conn: &Connection, sql: &str, user_id: &str, cols: &[&str]) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![user_id], |r| {
        let mut obj = serde_json::Map::new();
        for (i, name) in cols.iter().enumerate() {
            let v: rusqlite::types::Value = r.get(i)?;
            obj.insert((*name).to_string(), json_of(v));
        }
        Ok(Value::Object(obj))
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn json_of(v: rusqlite::types::Value) -> Value {
    match v {
        rusqlite::types::Value::Null => Value::Null,
        rusqlite::types::Value::Integer(i) => json!(i),
        rusqlite::types::Value::Real(f) => json!(f),
        rusqlite::types::Value::Text(s) => json!(s),
        // 导出是给人读的 JSON：二进制只报存在与长度，不内联。
        rusqlite::types::Value::Blob(b) => json!({ "binary_bytes": b.len() }),
    }
}

/// 可见性 → 对外字符串（策略层的枚举形状，不让调用方去猜 Debug 输出）。
fn visibility_name(v: Visibility) -> &'static str {
    match v {
        Visibility::Visible => "visible",
        Visibility::Limited => "limited",
        Visibility::Hidden => "hidden",
    }
}

/// 生效隐私判定。**全部问 `wxlike-security::privacy`**：这里只负责把偏好值喂进去。
///
/// `profile_to_*` 两行是给本人看的"别人眼中的我"，不是把策略抄一遍——
/// 判定函数改了，这里跟着变，因为口径只有一处。
fn privacy_view(prefs: &ServerPrefs) -> Value {
    let src = MapPrefs(&prefs.values);
    let d = PrivacyPrefs::default();
    let p = PrivacyPrefs {
        discover_by_phone: src.flag("privacy.discover_by_phone", d.discover_by_phone),
        discover_by_id: src.flag("privacy.discover_by_id", d.discover_by_id),
        allow_stranger_msg: src.flag("privacy.allow_stranger_msg", d.allow_stranger_msg),
        read_receipts: src.flag("privacy.read_receipts", d.read_receipts),
    };
    let stranger = Viewer {
        is_self: false,
        is_friend: false,
        blocked: false,
    };
    let friend = Viewer {
        is_self: false,
        is_friend: true,
        blocked: false,
    };
    let blocked = Viewer {
        is_self: false,
        is_friend: false,
        blocked: true,
    };
    json!({
        "searchable_by_phone": privacy_rule::searchable_by(p, SearchKey::Phone, false),
        "searchable_by_id": privacy_rule::searchable_by(p, SearchKey::AccountId, false),
        "stranger_can_message": privacy_rule::may_send_stranger(stranger, p),
        "friend_read_receipts": privacy_rule::may_read_receipts(friend, p),
        "profile_to_stranger": visibility_name(privacy_rule::profile_visibility(stranger, p)),
        "profile_to_blocked": visibility_name(privacy_rule::profile_visibility(blocked, p)),
    })
}

/// 通知判定：四个类别各跑一次策略函数（硬关 DND、类别开关、免打扰、预览）。
fn notify_view(prefs: &ServerPrefs, quiet_hours: bool) -> Value {
    let src = MapPrefs(&prefs.values);
    let kinds: [(NotifyKind, &str); 4] = [
        (NotifyKind::DirectMessage, "dm"),
        (NotifyKind::GroupMessage, "group"),
        (NotifyKind::Mention, "mention"),
        (NotifyKind::Call, "call"),
    ];
    let mut out = serde_json::Map::new();
    for (kind, name) in kinds {
        let d = privacy_rule::notification(&src, kind, quiet_hours);
        out.insert(
            (*name).to_string(),
            json!({ "deliver": d.deliver, "sound": d.sound, "preview": d.preview }),
        );
    }
    out.insert(
        "hard_off".to_string(),
        json!(src.flag("general.dnd", false)),
    );
    Value::Object(out)
}

/// 注销清理的**周期任务**入口（与 [`crate::spawn_redpacket_sweeper`] 同一模式：
/// 服务端起循环，测试直接调 [`Platform::run_account_jobs`]，推进因此是确定的）。
///
/// 一轮失败只记日志不退出：一条卡住的作业（比如钱包还有余额）不该把整个
/// 账户作业队列停掉——但那一条必须是 `failed` 状态、带原因，用户看得见。
pub fn spawn_account_job_worker(
    state: crate::PlatformState,
    every: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            drain_account_jobs(&state).await;
        }
    })
}

/// 一轮作业的结果只写日志。
///
/// 不退出循环：一条卡住的作业（比如钱包还有余额）不该把整个账户作业队列
/// 停掉——但那一条必须是 `failed` 状态、带原因，用户看得见（#114）。
async fn drain_account_jobs(state: &crate::PlatformState) {
    match state.run_account_jobs().await {
        Ok(0) => {}
        Ok(n) => tracing::info!(jobs = n, "account jobs finished"),
        Err(err) => tracing::error!(error = %err, "account job worker: 一条作业失败"),
    }
}

/// 导出六个域的取数 SQL。每条只有一个 `?1`（本人 user id）参数，
/// 取不到的列一律不取——口令摘要、TOTP 种子、pre-key 材料都不在导出里。
const SQL_PROFILE: &str = "SELECT id, name, avatar, signature, created_at FROM users \
     WHERE id = ?1 AND deleted_at IS NULL";
const PROFILE_COLS: &[&str] = &["id", "name", "avatar", "signature", "created_at"];

const SQL_CONTACTS: &str = "SELECT f.friend_id, u.name, f.remark, f.source, f.friend_group, \
                            f.created_at FROM friends f JOIN users u ON u.id = f.friend_id \
                            WHERE f.user_id = ?1 ORDER BY f.created_at";
const CONTACTS_COLS: &[&str] = &[
    "friend_id",
    "name",
    "remark",
    "source",
    "friend_group",
    "created_at",
];

/// `messages` 域：本人的收藏快照（原消息被撤回也仍在）。会话信箱本体在
/// `wxlike-message` 的读扩散库里、按会话而非按人归属，不在这里越界删。
const SQL_MESSAGES: &str = "SELECT kind, text, media_id, sender, sent_at_ms, source_kind, \
                            source_id, snapshot_label, created_at FROM favorites \
                            WHERE user_id = ?1 AND revoked = 0 ORDER BY created_at";
const MESSAGES_COLS: &[&str] = &[
    "kind",
    "text",
    "media_id",
    "sender",
    "sent_at_ms",
    "source_kind",
    "source_id",
    "snapshot_label",
    "created_at",
];

const SQL_MEDIA: &str = "SELECT id, size, content_type, conv_id, public, has_thumb, created_at \
                         FROM media WHERE uploader = ?1 ORDER BY created_at";
const MEDIA_COLS: &[&str] = &[
    "id",
    "size",
    "content_type",
    "conv_id",
    "public",
    "has_thumb",
    "created_at",
];

const SQL_LEDGER: &str = "SELECT kind, amount, counterparty, ref_id, memo, created_at FROM ledger \
                          WHERE user_id = ?1 ORDER BY id";
const LEDGER_COLS: &[&str] = &[
    "kind",
    "amount",
    "counterparty",
    "ref_id",
    "memo",
    "created_at",
];

const SQL_USER_SETTINGS: &str = "SELECT allow_strangers FROM user_settings WHERE user_id = ?1";
const USER_SETTINGS_COLS: &[&str] = &["allow_strangers"];

/// 注销清理：`PURGE_DOMAINS` 八个域落到具体表的 DELETE。
///
/// 口径与 `preferences.rs` 模块头一致——**本人名下的行删掉，法定/业务必需的
/// 记录由对应域自己留着**（见 [`PURGE_MASKS`]）。域名一律取自 `PURGE_DOMAINS`，
/// 只多一档 `preferences`：偏好本体在 `identity_pref`，没有外键，不删就永久留着。
///
/// 顺序有意义：子行先于父行（外键不是 DEFERRABLE）。
const PURGE_DELETES: &[(&str, &str)] = &[
    // 会话与一次性凭证。会话在 `request_delete` 里已经撤过，这里是兜底：
    // 任何"清账之后又冒出来的会话行"都必须在这一步消失。
    ("sessions", "DELETE FROM sessions WHERE user_id = ?1"),
    (
        "sessions",
        "DELETE FROM identity_step_up WHERE user_id = ?1",
    ),
    // 导出下载令牌是**凭据**：注销后还能兑换，等于把数据留在门外面。
    (
        "sessions",
        "DELETE FROM identity_export_link WHERE user_id = ?1",
    ),
    (
        "sessions",
        "DELETE FROM identity_reset_token WHERE user_id = ?1",
    ),
    ("sessions", "DELETE FROM recovery_codes WHERE user_id = ?1"),
    (
        "device_keys",
        "DELETE FROM prekey_bundles WHERE user_id = ?1",
    ),
    ("device_keys", "DELETE FROM devices WHERE user_id = ?1"),
    (
        "device_keys",
        "DELETE FROM identity_device WHERE user_id = ?1",
    ),
    ("messages", "DELETE FROM favorites WHERE user_id = ?1"),
    ("messages", "DELETE FROM contact_log WHERE user_id = ?1"),
    (
        "messages",
        "DELETE FROM notification_settings WHERE user_id = ?1",
    ),
    ("messages", "DELETE FROM notifications WHERE user_id = ?1"),
    (
        "messages",
        "DELETE FROM message_requests WHERE from_uid = ?1",
    ),
    ("messages", "DELETE FROM message_requests WHERE to_uid = ?1"),
    ("messages", "DELETE FROM thread_subs WHERE user_id = ?1"),
    (
        "messages",
        "DELETE FROM secret_verifications WHERE user_id = ?1",
    ),
    ("messages", "DELETE FROM live_viewers WHERE user_id = ?1"),
    ("media", "DELETE FROM content_media_refs WHERE owner = ?1"),
    // 表里的媒体行删掉就再也取不到；字节本体由媒体域自己的 GC 回收——
    // 平台的 MediaStore 接口没有 delete，这里不越界造一个。
    ("media", "DELETE FROM media WHERE uploader = ?1"),
    ("relations", "DELETE FROM friends WHERE user_id = ?1"),
    ("relations", "DELETE FROM friends WHERE friend_id = ?1"),
    ("relations", "DELETE FROM blocks WHERE user_id = ?1"),
    ("relations", "DELETE FROM blocks WHERE blocked_id = ?1"),
    (
        "relations",
        "DELETE FROM friend_requests WHERE from_uid = ?1",
    ),
    ("relations", "DELETE FROM friend_requests WHERE to_uid = ?1"),
    ("relations", "DELETE FROM group_members WHERE user_id = ?1"),
    ("relations", "DELETE FROM group_mutes WHERE user_id = ?1"),
    (
        "relations",
        "DELETE FROM group_join_requests WHERE user_id = ?1",
    ),
    (
        "relations",
        "DELETE FROM group_invites WHERE created_by = ?1",
    ),
    (
        "relations",
        "DELETE FROM community_members WHERE user_id = ?1",
    ),
    ("relations", "DELETE FROM channel_role WHERE user_id = ?1"),
    (
        "relations",
        "DELETE FROM publisher_follows WHERE user_id = ?1",
    ),
    ("relations", "DELETE FROM article_reads WHERE user_id = ?1"),
    (
        "relations",
        "DELETE FROM article_reactions WHERE user_id = ?1",
    ),
    ("relations", "DELETE FROM video_follows WHERE user_id = ?1"),
    ("relations", "DELETE FROM video_follows WHERE creator = ?1"),
    ("relations", "DELETE FROM video_likes WHERE user_id = ?1"),
    ("relations", "DELETE FROM moment_likes WHERE user_id = ?1"),
    (
        "relations",
        "DELETE FROM status_reactions WHERE user_id = ?1",
    ),
    ("relations", "DELETE FROM user_stickers WHERE user_id = ?1"),
    ("commerce", "DELETE FROM wallet_idem WHERE user_id = ?1"),
    // 余额非零在 `run_delete_job` 里就被拒绝了，走到这里必须是 0。
    ("commerce", "DELETE FROM wallets WHERE user_id = ?1"),
    ("content", "DELETE FROM moment_comments WHERE author = ?1"),
    ("content", "DELETE FROM moments WHERE author = ?1"),
    ("content", "DELETE FROM moment_drafts WHERE author = ?1"),
    ("content", "DELETE FROM video_comments WHERE author = ?1"),
    ("content", "DELETE FROM video_posts WHERE author = ?1"),
    ("content", "DELETE FROM articles WHERE author = ?1"),
    ("content", "DELETE FROM notes WHERE user_id = ?1"),
    ("content", "DELETE FROM sticker_packs WHERE owner = ?1"),
    (
        "content",
        "DELETE FROM status_background_drafts WHERE owner = ?1",
    ),
    ("content", "DELETE FROM statuses WHERE owner = ?1"),
    ("content", "DELETE FROM publishers WHERE owner = ?1"),
    ("search_index", "DELETE FROM content_index WHERE owner = ?1"),
    (
        "preferences",
        "DELETE FROM identity_pref WHERE user_id = ?1",
    ),
    (
        "preferences",
        "DELETE FROM user_settings WHERE user_id = ?1",
    ),
];

/// 留下的记录做**脱敏**而不是删：账目是资金守恒的证据（审计要能对上），
/// 红包的领取关系是**别人**账目的凭证——删掉会让另一个用户的账说不清。
const PURGE_MASKS: &[(&str, &str)] = &[
    (
        "commerce",
        "UPDATE ledger SET memo = '', counterparty = '', ref_id = '' WHERE user_id = ?1",
    ),
    (
        "commerce",
        "UPDATE redpackets SET memo = '', claimers = '[]' WHERE from_uid = ?1",
    ),
];

/// 逐域执行 DELETE，返回每个域清掉的行数（写进作业的摘要）。
fn purge_rows(conn: &Connection, user_id: &str) -> Result<Vec<(&'static str, usize)>> {
    let mut by_domain: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (domain, sql) in PURGE_DELETES {
        let n = conn.execute(sql, params![user_id])?;
        *by_domain.entry(domain).or_insert(0) += n;
    }
    Ok(by_domain.into_iter().collect())
}

fn mask_rows(conn: &Connection, user_id: &str) -> Result<()> {
    for (_, sql) in PURGE_MASKS {
        conn.execute(sql, params![user_id])?;
    }
    Ok(())
}

/// 匿名化用户行本身：display name、头像、签名都不是"已注销账号"该留的东西，
/// 口令材料全部清零（连摘要都不留，库副本泄露出去也换不回任何凭证）。
///
/// 行留着而不是 DELETE：`groups.owner_uid` / `communities.owner_uid` /
/// `threads.created_by` 三条外键没有 CASCADE，硬删会把无关的域顶坏。
fn anonymize_user(conn: &Connection, user_id: &str) -> Result<()> {
    let anon_name = format!("{DELETED_NAME_PREFIX}{user_id}");
    let n = conn.execute(
        "UPDATE users SET name = ?2, avatar = '', signature = '', pw_salt = '', \
         pw_hash = '', pw_rounds = 0, pw_algo = 'deleted', totp_secret = NULL, \
         totp_enabled = 0 WHERE id = ?1",
        params![user_id, anon_name],
    )?;
    if n != 1 {
        // 用户行都不在，这次"清理成功"就是假的。
        return Err(PlatformError::NotFound);
    }
    Ok(())
}
