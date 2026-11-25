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
//! `Platform::run_account_jobs` 推进——与红包清扫 [`crate::spawn_redpacket_sweeper`]
//! 同一个模式：服务端起周期任务，测试直接调函数，状态推进因此是确定的。
//! 取件走 `export_link`/`redeem_link`（**短时 + 单次**）；产物按自身内容摘要
//! 存在媒体仓里，`artifact` 就是那个摘要，不是路径，所以没有穿越面。
//! 媒体仓写不进去（断网/供应商故障）时作业是 `failed` + 原因，
//! **不会**留下 `done` 或产物地址。
//!
//! # 注销的顺序
//!
//! `Platform::schedule_deletion` 先按既有规则撤销**全部会话 + 刷新链 + step-up**
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
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::account`]。

use crate::auth::Authed;
use crate::error::{PlatformError, Result};
use crate::Platform;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use wxlike_identity::preferences::{
    self, Job, JobKind, JobStatus, ServerPrefs, EXPORT_SCOPES, SERVER_KEYS,
};
use wxlike_identity::throttle;
use wxlike_persistence::{SessionsRepo, UsersRepo};
use wxlike_security::privacy::{
    self as privacy_rule, MapPrefs, NotifyKind, PrefSource, PrivacyPrefs, SearchKey, Viewer,
    Visibility,
};

use super::repo::account::{
    anonymize_user, deleted_at, export_contacts, export_ledger, export_media, export_messages,
    export_profile, export_user_settings, mask_rows, open_delete_job, purge_rows, wallet_balance,
};
use super::repo::account_jobs;
use super::repo::preferences as pref_repo;
use crate::domain::repo::sessions::SqliteSessionsRepo;
use crate::domain::repo::users::SqliteUsersRepo;

/// 一次偏好同步最多返回多少条增量（游标翻页）。
pub(crate) const MAX_PREF_SYNC_PAGE: i64 = 200;

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

/// 本卡两个敏感动作的 step-up 用途。字符串与 `identity::step_up::PURPOSES`
/// 以及契约 fixture `jobRequest.step_up_purpose` 的 enum 完全一致——
/// 用途名一旦分叉，"要验证"就变成两张互不相认的通行证。
pub const PURPOSE_EXPORT: &str = "account.export";
pub const PURPOSE_DELETE: &str = "account.delete";

/// 本卡可授予的 step-up 用途（封闭集合；`device.revoke` 归 #103）。
pub(crate) const GRANTABLE_PURPOSES: [&str; 2] = [PURPOSE_EXPORT, PURPOSE_DELETE];

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
    pub(crate) fn of(job: &Job) -> Self {
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

impl PreferencesPatch {
    /// 取补丁本体。少了键、两个键都给、给成空补丁，一律算客户端漏了字段——
    /// 当"什么都不改"处理会把一个 bug 变成一次静默的成功。
    pub(crate) fn patch(&self) -> Result<&BTreeMap<String, String>> {
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

impl Platform {
    /// 偏好文档：原始值 + 同步游标 + 由策略层算出来的生效判定。
    ///
    /// `quiet_hours` 是 `Option`：`None` = 客户端没说，此时响铃判定是**在"不在
    /// 免打扰时段"这个假设下**算出来的，文档里用 `quiet_hours_known: false`
    /// 明说，不把它伪装成确定答案。
    pub(crate) fn preferences_document(
        &self,
        user_id: &str,
        quiet_hours: Option<bool>,
    ) -> Result<Value> {
        let c = self.db().acquire();
        let prefs = pref_repo::get(&c, user_id)?;
        let head = pref_repo::head_seq(&c, user_id)?;
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
    pub(crate) fn patch_preferences(
        &self,
        user_id: &str,
        patch: &BTreeMap<String, String>,
        device_id: &str,
    ) -> Result<()> {
        let now = wxlike_core::types::now_ms() as i64;
        self.db()
            .with_tx(|c| pref_repo::patch(c, user_id, patch, device_id, now).map(|_| ()))?;
        Ok(())
    }

    /// 增量：`since_seq` 之后的变更 + 当前游标（下次从这里接着拉）。
    pub(crate) fn preferences_since(
        &self,
        user_id: &str,
        since_seq: i64,
        limit: i64,
    ) -> Result<(Vec<preferences::PrefRow>, i64)> {
        let c = self.db().acquire();
        let rows = pref_repo::since(&c, user_id, since_seq, limit)?;
        Ok((rows, pref_repo::head_seq(&c, user_id)?))
    }

    /// 补丁归属的设备：body 给了就用 body 的，否则用当前会话里记的那台。
    ///
    /// **会话行找不到就报错**（401），不 `unwrap_or_default()` 造一台"空设备"：
    /// 那条路径意味着令牌刚被撤销或库里的归属信息没了，把变更记到一台不存在的
    /// 设备上，跨端同步就永远查不出是谁改的。
    pub(crate) fn patch_device(&self, authed: &Authed, provided: &str) -> Result<String> {
        if !provided.is_empty() {
            check_device_id(provided)?;
            return Ok(provided.to_string());
        }
        let c = self.db().lock()?;
        let stored = SqliteSessionsRepo::new(&c)
            .device_id_by_token(&crate::auth::token_digest(&authed.token))?;
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
    pub(crate) fn account_sid(&self, token: &str) -> Result<String> {
        self.session_sid(token)?.ok_or(PlatformError::Unauthorized)
    }

    /// step-up 的口令闸门（固定窗口 + 封禁，规则本体在 `identity::throttle`）。
    ///
    /// 桶键按账号，不按 IP：共用出口地址（公司/校园 NAT）下按 IP 封会把一整层
    /// 用户一起关在门外，而撞库的目标始终是**这个账号**。
    pub(crate) fn step_up_gate(&self, user_id: &str) -> Result<()> {
        let limit = throttle::Limit {
            max_attempts: STEP_UP_MAX_ATTEMPTS,
            window_ms: STEP_UP_WINDOW_MS,
            block_ms: STEP_UP_BLOCK_MS,
        };
        let now = wxlike_core::types::now_ms() as i64;
        let c = self.db().lock()?;
        throttle::check(&c, &step_up_bucket(user_id), limit, now)?;
        Ok(())
    }

    /// 记一次验证失败（达到阈值即封禁；封禁这件事要留在日志里）。
    pub(crate) fn step_up_denied(&self, user_id: &str) {
        let limit = throttle::Limit {
            max_attempts: STEP_UP_MAX_ATTEMPTS,
            window_ms: STEP_UP_WINDOW_MS,
            block_ms: STEP_UP_BLOCK_MS,
        };
        let now = wxlike_core::types::now_ms() as i64;
        let c = match self.db().lock() {
            Ok(c) => c,
            Err(err) => {
                tracing::error!(error = %err, "step-up 闸门拿不到连接");
                return;
            }
        };
        match throttle::record_failure(&c, &step_up_bucket(user_id), limit, now) {
            Ok(status) if status.blocked_until > now => {
                tracing::warn!(user = %user_id, until = status.blocked_until, "step-up 口令闸门封禁");
            }
            Err(err) => tracing::error!(error = %err, "step-up 闸门记不上账"),
            _ => {}
        }
    }

    /// 验证通过即清零，不惩罚正常用户。
    pub(crate) fn step_up_passed(&self, user_id: &str) {
        let c = match self.db().lock() {
            Ok(c) => c,
            Err(err) => {
                tracing::error!(error = %err, "step-up 闸门拿不到连接");
                return;
            }
        };
        if let Err(err) = throttle::reset(&c, &step_up_bucket(user_id)) {
            tracing::error!(error = %err, "step-up 闸门清不掉");
        }
    }

    /// 校验用途串并消费一次通行证（单次）。
    ///
    /// 先比用途、后消费：一个写错的 `step_up_purpose` 不该把另一张**有效**的
    /// 通行证吃掉（那会让用户以为"我验过了，怎么还要验"）。
    pub(crate) fn take_step_up(
        &self,
        authed: &Authed,
        want: &'static str,
        provided: &str,
    ) -> Result<()> {
        if provided != want {
            return Err(PlatformError::BadInput("step_up_purpose"));
        }
        let sid = self.account_sid(&authed.token)?;
        self.consume_step_up(&authed.user_id, &sid, want)
    }

    /// 申请导出。已有排队/执行中的导出时返回同一条作业（不重复打包）。
    pub fn request_export_job(&self, user_id: &str) -> Result<Job> {
        let now = wxlike_core::types::now_ms() as i64;
        self.db().with_tx(|c| {
            if deleted_at(c, user_id)?.is_some() {
                return Err(PlatformError::Blocked("account scheduled for deletion"));
            }
            account_jobs::request_export(c, user_id, now)
        })
    }

    /// 撤回一条排队中的作业。判定全在 `preferences::cancel`，这里只做
    /// 错误形状的翻译（409 而不是 409 "already exists: …"）。
    pub fn cancel_account_job(&self, user_id: &str, job_id: &str) -> Result<Job> {
        validate_id(job_id)?;
        let now = wxlike_core::types::now_ms() as i64;
        self.db()
            .with_tx(|c| account_jobs::cancel(c, job_id, user_id, now))
            .map_err(|err| match err {
                PlatformError::Conflict(what) => {
                    let reason = match what.as_str() {
                        "deletion cannot be cancelled" => "deletion cannot be cancelled",
                        "job already left the queue" => "job already left the queue",
                        "job already claimed" => "job already claimed",
                        _ => "job conflict",
                    };
                    PlatformError::Blocked(reason)
                }
                other => other,
            })
    }

    /// 本人的一条作业。**别人的作业一律 404**：作业 id 不是存在性探测面
    /// （与 `/api/sessions/{sid}/revoke` 同一口径）。
    pub fn account_job(&self, user_id: &str, job_id: &str) -> Result<Job> {
        validate_id(job_id)?;
        let c = self.db().acquire();
        let job = account_jobs::job(&c, job_id)?.ok_or(PlatformError::NotFound)?;
        if job.user_id != user_id {
            return Err(PlatformError::NotFound);
        }
        Ok(job)
    }

    pub fn account_jobs(&self, user_id: &str) -> Result<Vec<Job>> {
        let c = self.db().acquire();
        account_jobs::list(&c, user_id)
    }

    /// 为已完成的导出签一个短时单次下载令牌。未完成 → 403（`export not ready`）。
    pub fn create_export_link(&self, user_id: &str, job_id: &str) -> Result<(String, i64)> {
        let now = wxlike_core::types::now_ms() as i64;
        self.db().with_tx(|c| {
            let job = account_jobs::job(c, job_id)?.ok_or(PlatformError::NotFound)?;
            if job.user_id != user_id {
                return Err(PlatformError::NotFound);
            }
            let raw = account_jobs::export_link(c, job_id, user_id, now)?;
            Ok((raw, preferences::EXPORT_LINK_TTL_MS))
        })
    }

    /// 兑换下载令牌并读出产物。第二次兑换 401（单次）。
    pub async fn take_export_download(&self, raw: &str) -> Result<Vec<u8>> {
        validate_id(raw)?;
        let now = wxlike_core::types::now_ms() as i64;
        let raw = raw.to_string();
        let artifact = self
            .db()
            .blocking(move |c| Ok(preferences::redeem_link(c, &raw, now)?))
            .await?;
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
        let c = self.db().lock()?;
        let port = crate::storage::StorageConnection::sqlite_borrow(&c);
        if deleted_at(&port, user_id)?.is_some() {
            if let Some(job) = open_delete_job(&port, user_id)? {
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
        let revoked = SqliteSessionsRepo::new(&c).count_for_user(user_id)? as usize;
        // 规则本体在 identity：先撤凭证，再排清理作业。
        let job = preferences::request_delete(&c, user_id, now)?;
        let n = SqliteUsersRepo::new(&c).mark_deleted(user_id, now)?;
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
            let now = wxlike_core::types::now_ms() as i64;
            let claimed = self.db().with_tx(move |c| account_jobs::claim(c, now))?;
            let Some(job) = claimed else {
                break;
            };
            let outcome = match job.kind {
                JobKind::Export => self.run_export_job(&job).await,
                JobKind::Delete => self.run_delete_job(&job).await,
            };
            if let Err(err) = outcome {
                self.mark_job_failed(&job.id, &err.to_string()).await;
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
        let user_id = job.user_id.clone();
        let bytes = self
            .db()
            .with_tx(move |c| export_document_for(c, &user_id))?;
        let sha = wxlike_core::msgmeta::sha256_hex(&bytes);
        if let Err(err) = self.media_store().put(&sha, &bytes).await {
            // 底层 io/sqlite 文本只进服务端日志（它可能带路径），跨请求边界的那句
            // 用固定措辞——用户在作业列表里看得到"为什么没导出来"，
            // 但拿不到服务端文件系统形状。
            tracing::error!(job = %job.id, error = %err, "export: 媒体仓写入失败");
            // 媒体仓连不上是**依赖暂时不可用**（503、可重试），不是我们的代码
            // 出错（500、不重试）。以前借用 `Internal` 表达，客户端于是不重试，
            // 用户得手动再点一次——而存储恢复后重试本来就能成。
            return Err(PlatformError::Unavailable("export storage unreachable"));
        }
        let now = wxlike_core::types::now_ms() as i64;
        let job_id = job.id.clone();
        let size = bytes.len() as i64;
        self.db()
            .with_tx(move |c| account_jobs::finish(c, &job_id, &sha, size, now))?;
        Ok(())
    }

    /// 执行注销清理。整个清理 + 脱敏 + 匿名化在**一个事务**里：任何一条语句
    /// 失败（含外键冲突）就整体回滚，作业记 `failed`，绝不留"删了一半"。
    async fn run_delete_job(&self, job: &Job) -> Result<()> {
        let job = job.clone();
        let now = wxlike_core::types::now_ms() as i64;
        self.db()
            .blocking(move |conn| {
                let c = &*conn;
                let port = crate::storage::StorageConnection::sqlite_borrow(c);
                // 没有标记绝不清理：一条凭空出现的 delete 作业不属于注销流程。
                if deleted_at(&port, &job.user_id)?.is_none() {
                    return Err(PlatformError::Blocked("deletion was never scheduled"));
                }
                // 余额没清零就不清钱包：静默销毁余额是不可接受的"清理成功"。
                if wallet_balance(&port, &job.user_id)? != 0 {
                    return Err(PlatformError::Blocked("wallet not empty"));
                }
                let tx = c.unchecked_transaction()?;
                let counts = {
                    let tx_port = crate::storage::StorageConnection::sqlite_borrow(&tx);
                    let counts = purge_rows(&tx_port, &job.user_id)?;
                    mask_rows(&tx_port, &job.user_id)?;
                    anonymize_user(&tx_port, &job.user_id)?;
                    counts
                };
                tx.commit()?;
                let total: i64 = counts.iter().map(|(_, n)| *n as i64).sum();
                // 注销作业的 artifact 存的是"每个域清掉多少行"的摘要，不是下载地址
                // （`export_link` 只认 kind = export，所以这里不会被误当成产物路径）。
                let summary = serde_json::to_string(&counts)?;
                preferences::finish(c, &job.id, &summary, total, now)?;
                Ok(())
            })
            .await
    }

    async fn mark_job_failed(&self, job_id: &str, reason: &str) {
        let trimmed: String = reason.chars().take(MAX_FAIL_REASON_CHARS).collect();
        let job_id = job_id.to_string();
        let fail_id = job_id.clone();
        let now = wxlike_core::types::now_ms() as i64;
        let ran = self
            .db()
            .with_tx(move |c| account_jobs::fail(c, &job_id, &trimmed, now));
        if let Err(err) = ran {
            tracing::error!(job = %fail_id, error = %err, "account job: 连失败原因都没写进去");
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
        let c = self.db().acquire();
        export_document_for(&c, user_id)
    }
}

/// `Platform::export_document` 的取数部分：调用方已经持有连接，整段查询
/// 因此可以作为一个阻塞任务搬进 `Db::blocking`，而不用在 await 上下文里
/// 再取一次锁。
fn export_document_for(
    port: &crate::storage::StorageConnection<'_>,
    user_id: &str,
) -> Result<Vec<u8>> {
    let profile = export_profile(port, user_id)?;
    let Some(owner) = profile.first().cloned() else {
        // 本人行读不到 = 这份导出没有内容可言，必须失败，不能交空包。
        return Err(PlatformError::NotFound);
    };
    let prefs = pref_repo::get(port, user_id)?;
    let settings = json!({
        "preferences": prefs.values,
        "legacy_user_settings": export_user_settings(port, user_id)?,
        "privacy": privacy_view(&prefs),
    });
    let doc = json!({
        "user_id": user_id,
        "exported_at_ms": wxlike_core::types::now_ms(),
        "scopes": EXPORT_SCOPES,
        "profile": owner,
        "contacts": export_contacts(port, user_id)?,
        "messages": export_messages(port, user_id)?,
        "media_manifest": export_media(port, user_id)?,
        "ledger": export_ledger(port, user_id)?,
        "settings": settings,
    });
    Ok(serde_json::to_vec_pretty(&doc)?)
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
/// 服务端起循环，测试直接调 `Platform::run_account_jobs`，推进因此是确定的）。
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

#[derive(Deserialize, Default)]
pub(crate) struct PreferencesPatch {
    /// 字段级补丁：只改列出的键，其余不动（两台设备各改各的不互相覆盖）。
    /// 契约 fixture 里的名字是 `values`，两个都收，但**必须且只能给一个**。
    #[serde(default)]
    pub(crate) patch: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub(crate) values: Option<BTreeMap<String, String>>,
    /// 这次变更归属哪台设备；留空则用当前会话记录的设备。
    #[serde(default)]
    pub(crate) device_id: String,
}
