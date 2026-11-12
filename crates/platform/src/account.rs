//! **handler 层**（`account`）：路由 + 提取器 + 响应组装。
//! 不碰 SQL——`crate::domain::repo` 对本模块不可见。

use crate::auth::Authed;
use crate::error::{ApiError, PlatformError};
use crate::extract::ApiJson;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use wxlike_identity::preferences::PURGE_DOMAINS;

use crate::domain::account::{
    JobReply, PreferencesPatch, GRANTABLE_PURPOSES, MAX_PREF_SYNC_PAGE, PURPOSE_DELETE,
    PURPOSE_EXPORT,
};

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
/// 验证本体在 `identity::step_up`，口令/TOTP 的判定在 `domain::security::confirm_identity`。
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
