//! G02-05 设置、账户数据生命周期与通知偏好。
//!
//! # 两类偏好，两条路径
//!
//! * **本地偏好**（[`LocalPrefs`]）：主题、字号、密度……只属于这台机器。它们
//!   **不写库、不上云**，否则"我在手机上调深色模式"会污染桌面端；
//! * **服务端偏好**（[`ServerPrefs`]）：通知开关、隐私可见性、黑名单策略……
//!   逐字段存 `identity_pref`，带自增 `seq` 作为跨端同步游标。补丁是字段级的，
//!   两台设备同时改不同字段不会互相覆盖。
//!
//! # 导出与注销
//!
//! 两者都是**有审计的后台作业**，不在请求路径里跑。导出只覆盖本人可见数据
//! （[`EXPORT_SCOPES`]），下载链接短时且单次（[`redeem_link`]）。注销先撤销
//! 全部会话与刷新链，再交给各域清理（[`PURGE_DOMAINS`]）；法定/业务必需记录
//! 由对应域自行分域处置，不在这里一刀切删。
//!
//! 排队中的导出可以被申请人撤回（[`cancel`]）；注销**不行**——凭证撤销没有
//! 回滚路径，把作业标成取消只会得到一个"已恢复"却没恢复任何登录能力的账号。

use std::collections::BTreeMap;

use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::{random_hex, IdError, Result};

/// 服务端偏好的白名单键。**封闭集合**：写错的键不会静默存进去。
pub const SERVER_KEYS: [&str; 10] = [
    "notify.dm",
    "notify.group",
    "notify.preview",
    "notify.quiet_hours",
    "privacy.discover_by_phone",
    "privacy.discover_by_id",
    "privacy.allow_stranger_msg",
    "privacy.read_receipts",
    "general.language",
    "general.dnd",
];
/// 单个偏好值的长度上限。
pub const MAX_VALUE_CHARS: usize = 128;
/// 作业 id 的随机字节数。
pub const JOB_ID_BYTES: usize = 12;
/// 导出下载链接有效期：10 分钟。
pub const EXPORT_LINK_TTL_MS: i64 = 10 * 60 * 1000;
/// 导出覆盖的数据域（只含本人可见数据）。
pub const EXPORT_SCOPES: [&str; 6] = [
    "profile",
    "contacts",
    "messages",
    "media_manifest",
    "ledger",
    "settings",
];
/// 注销时要清理的域。
pub const PURGE_DOMAINS: [&str; 8] = [
    "sessions",
    "device_keys",
    "messages",
    "media",
    "relations",
    "commerce",
    "content",
    "search_index",
];

/// 本地偏好：只在这台设备上，序列化到本地存储即可。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LocalPrefs {
    #[serde(default)]
    pub theme: String,
    #[serde(default)]
    pub font_scale: f32,
    #[serde(default)]
    pub density: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub last_tab: String,
}

impl LocalPrefs {
    /// 应用一次本地补丁；未知字段忽略（前向兼容：新版本写的字段旧版本不炸）。
    pub fn apply(&mut self, patch: &serde_json::Value) {
        if let Some(v) = patch.get("theme").and_then(|v| v.as_str()) {
            self.theme = v.to_string();
        }
        if let Some(v) = patch.get("density").and_then(|v| v.as_str()) {
            self.density = v.to_string();
        }
        if let Some(v) = patch.get("language").and_then(|v| v.as_str()) {
            self.language = v.to_string();
        }
        if let Some(v) = patch.get("last_tab").and_then(|v| v.as_str()) {
            self.last_tab = v.to_string();
        }
        if let Some(v) = patch.get("font_scale").and_then(|v| v.as_f64()) {
            self.font_scale = v as f32;
        }
    }
}

/// 服务端偏好：键值对 + 类型化读取。缺省值集中在 [`ServerPrefs::get_bool`]。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerPrefs {
    pub values: BTreeMap<String, String>,
}

impl ServerPrefs {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(|s| s.as_str())
    }

    /// 布尔偏好：未设置时用 `default`（默认值就是产品默认行为）。
    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        match self.get(key) {
            Some("on") | Some("true") => true,
            Some("off") | Some("false") => false,
            _ => default,
        }
    }
}

/// 一条偏好变更（跨端同步的增量单位）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefRow {
    pub seq: i64,
    pub key: String,
    pub value: String,
    pub updated_at: i64,
    pub device_id: String,
}

/// 读取全部服务端偏好（缺的键不补，让客户端用自己的默认值）。
pub fn get(conn: &Connection, user_id: &str) -> Result<ServerPrefs> {
    let mut stmt = conn.prepare("SELECT key, value FROM identity_pref WHERE user_id = ?1")?;
    let rows = stmt.query_map(params![user_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut values = BTreeMap::new();
    for row in rows {
        let (k, v) = row?;
        values.insert(k, v);
    }
    Ok(ServerPrefs { values })
}

/// 字段级补丁。幂等：同样的补丁重复提交结果一致。
pub fn patch(
    conn: &Connection,
    user_id: &str,
    patch: &BTreeMap<String, String>,
    device_id: &str,
    now: i64,
) -> Result<ServerPrefs> {
    for (key, value) in patch {
        if !SERVER_KEYS.contains(&key.as_str()) {
            return Err(IdError::BadInput("pref key"));
        }
        if value.chars().count() > MAX_VALUE_CHARS || value.chars().any(|c| c.is_control()) {
            return Err(IdError::BadInput("pref value"));
        }
    }
    let tx = conn.unchecked_transaction()?;
    for (key, value) in patch {
        tx.execute(
            "INSERT INTO identity_pref(user_id, key, value, updated_at, device_id) \
             VALUES(?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(user_id, key) DO UPDATE SET \
                 value = ?3, updated_at = ?4, device_id = ?5",
            params![user_id, key, value, now, device_id],
        )?;
    }
    tx.commit()?;
    get(conn, user_id)
}

/// 增量同步：`since_seq` 之后的变更（另一台设备拉取用）。
pub fn since(conn: &Connection, user_id: &str, since_seq: i64, limit: i64) -> Result<Vec<PrefRow>> {
    let mut stmt = conn.prepare(
        "SELECT seq, key, value, updated_at, device_id FROM identity_pref \
         WHERE user_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![user_id, since_seq, limit], |r| {
        Ok(PrefRow {
            seq: r.get(0)?,
            key: r.get(1)?,
            value: r.get(2)?,
            updated_at: r.get(3)?,
            device_id: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 当前游标上界（客户端下次从这里接着拉）。
///
/// 空账号返回 0 是因为 SQL 里 `COALESCE(MAX(seq), 0)` 真的没有行；
/// **查询本身失败绝不折成 0**——那会把"库读不动"伪装成"这个账号还没写过偏好"，
/// 客户端于是把游标重置到 0，同步从下一次开始静默漏掉全部增量。
pub fn head_seq(conn: &Connection, user_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM identity_pref WHERE user_id = ?1",
        params![user_id],
        |r| r.get(0),
    )?)
}

/// 作业种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Export,
    Delete,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Export => "export",
            Self::Delete => "delete",
        }
    }

    /// 认不出来的种类同样算损坏。旧的 `unwrap_or(JobKind::Export)` 会把一条
    /// `kind='deleet'` 的注销作业读成"导出"：调用方于是拿着它去签发下载链接，
    /// 注销作业伪装成一份能下载的东西。
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "export" => Some(Self::Export),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
    /// 申请人自己在排队阶段撤回的。
    ///
    /// 它**不等于** `failed`：失败是"我们没做成"，取消是"用户不要了"。折成一个
    /// 状态会让作业列表把一次主动撤回报成一次故障（#114 的取消用例）。
    Cancelled,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// 认不出来的状态是**损坏的行**，不是"还没开始跑"。
    ///
    /// 旧实现把未知值折成 `Queued`：一条被写成 `don`（少个 e）的作业会一直
    /// 显示"排队中"，worker 也永远不会再取它（取的是 `status = 'queued'`
    /// 的字面量）——用户看到的是一个永不推进的假进度条。
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub user_id: String,
    pub kind: JobKind,
    pub status: JobStatus,
    pub requested_at: i64,
    pub artifact: String,
    pub bytes: i64,
    pub error: String,
}

/// 一行的**原文**读法：文本列先按字符串取，状态判定留到 [`decode_job`]。
///
/// 分两步是为了让"读不出来"必须经过一个可失败的地方。行闭包里只能返回
/// `rusqlite::Result`，折默认值太顺手（旧代码就在这里折出 Export/Queued），
/// 坏数据因此在类型层面假装成功。
struct RawJob {
    id: String,
    user_id: String,
    kind: String,
    status: String,
    requested_at: i64,
    artifact: String,
    bytes: i64,
    error: String,
}

fn raw_job(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawJob> {
    Ok(RawJob {
        id: r.get(0)?,
        user_id: r.get(1)?,
        kind: r.get(2)?,
        status: r.get(3)?,
        requested_at: r.get(4)?,
        artifact: r.get(5)?,
        bytes: r.get(6)?,
        error: r.get(7)?,
    })
}

/// 原文 → 领域对象。任何一种"看不懂"都是一条 [`IdError::Corrupt`]。
fn decode_job(raw: RawJob) -> Result<Job> {
    Ok(Job {
        id: raw.id,
        user_id: raw.user_id,
        kind: JobKind::parse(&raw.kind).ok_or(IdError::Corrupt("identity_job.kind"))?,
        status: JobStatus::parse(&raw.status).ok_or(IdError::Corrupt("identity_job.status"))?,
        requested_at: raw.requested_at,
        artifact: raw.artifact,
        bytes: raw.bytes,
        error: raw.error,
    })
}

const JOB_COLS: &str = "id, user_id, kind, status, requested_at, artifact, bytes, error";

fn find_open(conn: &Connection, user_id: &str, kind: JobKind) -> Result<Option<Job>> {
    let sql = format!(
        "SELECT {JOB_COLS} FROM identity_job \
         WHERE user_id = ?1 AND kind = ?2 AND status IN ('queued','running') \
         ORDER BY requested_at DESC LIMIT 1"
    );
    conn.query_row(&sql, params![user_id, kind.as_str()], raw_job)
        .optional()?
        .map(decode_job)
        .transpose()
}

/// 申请导出。已有排队/执行中的导出时返回同一条（幂等，不重复打包）。
pub fn request_export(conn: &Connection, user_id: &str, now: i64) -> Result<Job> {
    if let Some(job) = find_open(conn, user_id, JobKind::Export)? {
        return Ok(job);
    }
    enqueue(conn, user_id, JobKind::Export, now)
}

/// 申请注销：**先撤销全部会话与刷新链**，再排队清理各域。
pub fn request_delete(conn: &Connection, user_id: &str, now: i64) -> Result<Job> {
    if let Some(job) = find_open(conn, user_id, JobKind::Delete)? {
        return Ok(job);
    }
    crate::session::logout_all(conn, user_id, now)?;
    crate::step_up::revoke_all(conn, user_id)?;
    enqueue(conn, user_id, JobKind::Delete, now)
}

fn enqueue(conn: &Connection, user_id: &str, kind: JobKind, now: i64) -> Result<Job> {
    let id = random_hex(JOB_ID_BYTES);
    conn.execute(
        "INSERT INTO identity_job(id, user_id, kind, status, requested_at, artifact, bytes, error) \
         VALUES(?1, ?2, ?3, 'queued', ?4, '', 0, '')",
        params![id, user_id, kind.as_str(), now],
    )?;
    job(conn, &id)?.ok_or(IdError::NotFound("job"))
}

/// 单条作业。
pub fn job(conn: &Connection, id: &str) -> Result<Option<Job>> {
    let sql = format!("SELECT {JOB_COLS} FROM identity_job WHERE id = ?1");
    conn.query_row(&sql, params![id], raw_job)
        .optional()?
        .map(decode_job)
        .transpose()
}

/// 后台 worker 取一条排队作业（条件 UPDATE 保证只有一个 worker 拿到）。
///
/// 取之前先验原文：一行已经坏掉的作业**不该被标记成 running**，否则它从此
/// 既读不出状态、也不会再被任何 worker 取走（取的是 `status='queued'`）。
pub fn claim(conn: &Connection, now: i64) -> Result<Option<Job>> {
    let next: Option<(String, String, String)> = conn
        .query_row(
            "SELECT id, kind, status FROM identity_job WHERE status = 'queued' \
             ORDER BY requested_at ASC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((id, kind, status)) = next else {
        return Ok(None);
    };
    JobKind::parse(&kind).ok_or(IdError::Corrupt("identity_job.kind"))?;
    JobStatus::parse(&status).ok_or(IdError::Corrupt("identity_job.status"))?;
    let n = conn.execute(
        "UPDATE identity_job SET status = 'running', started_at = ?2 \
         WHERE id = ?1 AND status = 'queued'",
        params![&id, now],
    )?;
    if n != 1 {
        return Ok(None);
    }
    job(conn, &id)
}

/// 完成作业（写入产物路径与大小）。
pub fn finish(conn: &Connection, id: &str, artifact: &str, bytes: i64, now: i64) -> Result<()> {
    let n = conn.execute(
        "UPDATE identity_job SET status = 'done', artifact = ?2, bytes = ?3, finished_at = ?4 \
         WHERE id = ?1 AND status = 'running'",
        params![id, artifact, bytes, now],
    )?;
    if n != 1 {
        return Err(IdError::Conflict("job not running"));
    }
    Ok(())
}

/// 作业失败（保留原因，供用户看到"为什么没导出来"）。
pub fn fail(conn: &Connection, id: &str, reason: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE identity_job SET status = 'failed', error = ?2, finished_at = ?3 WHERE id = ?1",
        params![id, crate::check_label(reason, "error")?, now],
    )?;
    Ok(())
}

/// 取消一条**还在排队**的作业。
///
/// 只有 `queued` 能取消：一条已经 `running` 的作业正在读库/写媒体仓，把它标成
/// 取消只会得到"状态说没做、产物却真的存在"的两份真相。已经 `done` 的导出更不
/// 可能"取消"——东西已经打包好了，能做的是让下载链接过期。
///
/// 注销作业按种类直接拒绝：`request_delete` 撤销会话与刷新链是**不可逆**的，
/// 现在把作业标成取消，用户面对的是一个"已恢复"却没恢复任何凭证的账号。
/// 错误串是 `'static` 常量，平台层照原样映射成 409，不掺用户数据。
pub fn cancel(conn: &Connection, job_id: &str, user_id: &str, now: i64) -> Result<Job> {
    let Some(existing) = job(conn, job_id)? else {
        return Err(IdError::NotFound("job"));
    };
    // 别人的作业与不存在同形：作业 id 不是存在性探测面。
    if existing.user_id != user_id {
        return Err(IdError::NotFound("job"));
    }
    if existing.kind == JobKind::Delete {
        return Err(IdError::Conflict("deletion cannot be cancelled"));
    }
    if existing.status != JobStatus::Queued {
        return Err(IdError::Conflict("job already left the queue"));
    }
    let n = conn.execute(
        "UPDATE identity_job SET status = 'cancelled', finished_at = ?2 \
         WHERE id = ?1 AND status = 'queued'",
        params![job_id, now],
    )?;
    if n != 1 {
        // 并发：worker 正好在这一刻把它领走了。状态是一致的，但这次取消没做成。
        return Err(IdError::Conflict("job already claimed"));
    }
    job(conn, job_id)?.ok_or(IdError::NotFound("job"))
}

/// 账号的全部作业，最近在前。
pub fn list(conn: &Connection, user_id: &str) -> Result<Vec<Job>> {
    let sql = format!(
        "SELECT {JOB_COLS} FROM identity_job WHERE user_id = ?1 ORDER BY requested_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![user_id], raw_job)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(decode_job(row?)?);
    }
    Ok(out)
}

/// 为已完成的导出签发短时下载令牌（只存摘要）。
pub fn export_link(conn: &Connection, job_id: &str, user_id: &str, now: i64) -> Result<String> {
    let Some(job) = job(conn, job_id)? else {
        return Err(IdError::NotFound("job"));
    };
    if job.user_id != user_id || job.kind != JobKind::Export || job.status != JobStatus::Done {
        return Err(IdError::Forbidden("export not ready"));
    }
    let raw = random_hex(crate::TOKEN_BYTES);
    conn.execute(
        "INSERT INTO identity_export_link(token_hash, job_id, user_id, expires_at, used_at) \
         VALUES(?1, ?2, ?3, ?4, NULL)",
        params![
            crate::digest(&raw),
            job_id,
            user_id,
            now.saturating_add(EXPORT_LINK_TTL_MS)
        ],
    )?;
    Ok(raw)
}

/// 兑换下载令牌：单次、限时。返回产物路径。
pub fn redeem_link(conn: &Connection, raw: &str, now: i64) -> Result<String> {
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "UPDATE identity_export_link SET used_at = ?2 \
         WHERE token_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
        params![crate::digest(raw), now],
    )?;
    if n != 1 {
        return Err(IdError::Unauthorized("link expired or already used"));
    }
    let artifact: String = tx.query_row(
        "SELECT j.artifact FROM identity_job j \
         JOIN identity_export_link l ON l.job_id = j.id WHERE l.token_hash = ?1",
        params![crate::digest(raw)],
        |r| r.get(0),
    )?;
    tx.commit()?;
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    fn patch_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn local_prefs_never_touch_the_db() {
        let conn = db();
        let mut local = LocalPrefs::default();
        local.apply(&serde_json::json!({"theme": "dark", "font_scale": 1.15}));
        assert_eq!(local.theme, "dark");
        assert!((local.font_scale - 1.15).abs() < f32::EPSILON);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM identity_pref", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "本地偏好不得落服务端库");
    }

    #[test]
    fn patch_is_field_level_and_syncs_incrementally() {
        let conn = db();
        let head = head_seq(&conn, "u1").unwrap();
        assert_eq!(head, 0);
        patch(
            &conn,
            "u1",
            &patch_of(&[("notify.dm", "off")]),
            "dev-a",
            1_000,
        )
        .unwrap();
        let after_first = head_seq(&conn, "u1").unwrap();
        patch(
            &conn,
            "u1",
            &patch_of(&[("privacy.discover_by_phone", "off")]),
            "dev-b",
            2_000,
        )
        .unwrap();
        let delta = since(&conn, "u1", after_first, 100).unwrap();
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].key, "privacy.discover_by_phone");
        assert_eq!(delta[0].device_id, "dev-b");

        let prefs = get(&conn, "u1").unwrap();
        assert!(!prefs.get_bool("notify.dm", true));
        assert!(!prefs.get_bool("privacy.discover_by_phone", true));
        assert!(
            prefs.get_bool("notify.group", true),
            "未设置的键用产品默认值"
        );
    }

    #[test]
    fn patch_is_idempotent_and_validated() {
        let conn = db();
        let p = patch_of(&[("general.language", "zh-CN")]);
        let a = patch(&conn, "u1", &p, "dev-a", 1_000).unwrap();
        let b = patch(&conn, "u1", &p, "dev-a", 2_000).unwrap();
        assert_eq!(a.values, b.values);
        assert!(
            matches!(
                patch(&conn, "u1", &patch_of(&[("theme", "dark")]), "dev-a", 3_000),
                Err(IdError::BadInput("pref key"))
            ),
            "主题是本地偏好，不能走服务端通道"
        );
        assert!(matches!(
            patch(
                &conn,
                "u1",
                &patch_of(&[("notify.dm", "o\u{7}ff")]),
                "dev-a",
                3_000
            ),
            Err(IdError::BadInput("pref value"))
        ));
    }

    #[test]
    fn export_job_lifecycle_and_single_use_link() {
        let conn = db();
        let job = request_export(&conn, "u1", 1_000).unwrap();
        assert_eq!(job.status, JobStatus::Queued);
        let again = request_export(&conn, "u1", 1_100).unwrap();
        assert_eq!(again.id, job.id, "重复申请不重复打包");
        let claimed = claim(&conn, 1_200).unwrap().unwrap();
        assert_eq!(claimed.id, job.id);
        assert!(claim(&conn, 1_300).unwrap().is_none(), "同一条不会被取两次");
        finish(&conn, &job.id, "/tmp/export.zip", 4_096, 1_400).unwrap();

        let link = export_link(&conn, &job.id, "u1", 1_500).unwrap();
        assert_eq!(redeem_link(&conn, &link, 1_600).unwrap(), "/tmp/export.zip");
        assert!(matches!(
            redeem_link(&conn, &link, 1_700),
            Err(IdError::Unauthorized(_))
        ));
        let stale = export_link(&conn, &job.id, "u1", 1_800).unwrap();
        assert!(matches!(
            redeem_link(&conn, &stale, 1_800 + EXPORT_LINK_TTL_MS + 1),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn link_is_scoped_to_owner() {
        let conn = db();
        let job = request_export(&conn, "u1", 1_000).unwrap();
        claim(&conn, 1_100).unwrap();
        finish(&conn, &job.id, "/tmp/x.zip", 1, 1_200).unwrap();
        assert!(matches!(
            export_link(&conn, &job.id, "u2", 1_300),
            Err(IdError::Forbidden(_))
        ));
    }

    #[test]
    fn delete_revokes_every_session_first() {
        let conn = db();
        let reg = crate::device::Registration {
            platform: "ios".into(),
            ..Default::default()
        };
        crate::device::register(&conn, "u1", "dev-aaaa1111", &reg, 1_000).unwrap();
        let grant = crate::session::open_default(
            &conn,
            "u1",
            "dev-aaaa1111",
            crate::Audience::User,
            "",
            1_000,
        )
        .unwrap();
        crate::step_up::grant_default(&conn, "u1", &grant.sid, "account.delete", 1_000).unwrap();
        let job = request_delete(&conn, "u1", 1_100).unwrap();
        assert_eq!(job.kind, JobKind::Delete);
        assert!(crate::session::authenticate(
            &conn,
            &grant.access_token,
            crate::Audience::User,
            1_200
        )
        .is_err());
        assert!(crate::refresh::find(&conn, &grant.refresh_token)
            .unwrap()
            .unwrap()
            .revoked_at
            .is_some());
        assert_eq!(list(&conn, "u1").unwrap().len(), 1);
    }

    #[test]
    fn fail_marks_reason() {
        let conn = db();
        let job = request_export(&conn, "u1", 1_000).unwrap();
        claim(&conn, 1_100).unwrap();
        fail(&conn, &job.id, "storage unavailable", 1_200).unwrap();
        let after = job_by_id(&conn, &job.id);
        assert_eq!(after.status, JobStatus::Failed);
        assert!(after.error.contains("storage"));
    }

    #[test]
    fn cancel_only_stops_a_queued_export() {
        let conn = db();
        let job = request_export(&conn, "u1", 1_000).unwrap();

        // 别人的作业与不存在同形
        assert!(matches!(
            cancel(&conn, &job.id, "u2", 1_050),
            Err(IdError::NotFound("job"))
        ));

        let after = cancel(&conn, &job.id, "u1", 1_100).unwrap();
        assert_eq!(after.status, JobStatus::Cancelled);
        // 取消之后 worker 再也领不到它
        assert!(claim(&conn, 1_200).unwrap().is_none());
        // 重复取消：已经离开队列，如实拒绝，不"再成功一次"
        assert!(matches!(
            cancel(&conn, &job.id, "u1", 1_300),
            Err(IdError::Conflict("job already left the queue"))
        ));
        // 排队状态没被"复活"
        assert_eq!(job_by_id(&conn, &job.id).status, JobStatus::Cancelled);
        // 取消之后再申请是一条新作业（那条不算在排）
        let fresh = request_export(&conn, "u1", 1_400).unwrap();
        assert_ne!(fresh.id, job.id);
    }

    #[test]
    fn cancel_refuses_a_delete_because_revocation_is_irreversible() {
        let conn = db();
        let job = request_delete(&conn, "u1", 1_000).unwrap();
        assert!(matches!(
            cancel(&conn, &job.id, "u1", 1_100),
            Err(IdError::Conflict("deletion cannot be cancelled"))
        ));
        // 拒绝不改状态：它仍然在排，等 worker 清理
        assert_eq!(job_by_id(&conn, &job.id).status, JobStatus::Queued);
        // 已经 running 的导出同样不能取消（正在写产物）
        let exp = request_export(&conn, "u2", 1_500).unwrap();
        // 队首是上面那条 delete 作业（requested_at 更早），先领走它再领导出
        claim(&conn, 1_600).unwrap().expect("领走 delete");
        let claimed = claim(&conn, 1_700).unwrap().expect("领走 export");
        assert_eq!(claimed.id, exp.id);
        assert!(matches!(
            cancel(&conn, &exp.id, "u2", 1_800),
            Err(IdError::Conflict("job already left the queue"))
        ));
    }

    fn job_by_id(conn: &Connection, id: &str) -> Job {
        job(conn, id).unwrap().unwrap()
    }
}
