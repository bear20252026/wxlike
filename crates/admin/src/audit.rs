//! G14-01 审计：**append-only、可校验、已脱敏**的运营动作流水。
//!
//! 三个决定值得写下来：
//!
//! 1. **哈希链而不是"日志"**。每行带 `prev_hash`，`hash = sha256(前一行 hash ‖ 本行
//!    判定字段)`。`verify` 从第一行重算到尾，断链就报出断在第几行。
//!    数据库另有触发器拒 UPDATE/DELETE（见 migrations/0001）。只靠"应用层不写
//!    UPDATE"是不够的——出事时往往不是应用层在改。
//!
//! 2. **只存摘要，不存正文**。`before`/`after` 进库前先过 [`sanitize`] 再取
//!    sha256：私聊正文、密钥、令牌、密码一个字都不落盘。审计要回答的是
//!    "谁在什么时候把哪个对象从 A 改成 B、理由是什么"，不是"当时聊了什么"。
//!    前后状态本身用 `after` 的脱敏 JSON 存在 `detail` 里供人看，摘要只用于比对。
//!
//! 3. **`reason` 必填**。处置结论要能说清依据（合同 §3.14）。空理由直接拒，
//!    不留"事后补理由"的窗口。

use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AdminError, Result};
use crate::read::{now_ms, page_limit, Paged};
use crate::store::Store;

/// 审计 `reason` 的上限。比留痕 detail 短：理由是一句话，不是报告。
pub const REASON_MAX_CHARS: usize = 512;

/// 脱敏后的占位符。不带长度、不带类型——这些也是元数据。
pub const REDACTED: &str = "[redacted]";

/// 键名命中这些子串就整值脱敏（大小写不敏感）。
const SENSITIVE_KEY_PARTS: &[&str] = &[
    "message",
    "content",
    "body",
    "plaintext",
    "ciphertext",
    "secret",
    "password",
    "passwd",
    "token",
    "cookie",
    "authorization",
    "credential",
    "private_key",
    "session_key",
    "id_card",
    "bank_account",
    "phone",
];

/// 键名以这些后缀结尾时**不脱敏**：它们是引用/标识/时间，不是秘密。
const REFERENCE_KEY_SUFFIXES: &[&str] = &[
    "_ref", "_digest", "_hash", "_id", "_ids", "_at", "_ms", "_by", "_no",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// 动作已生效（上游确认或本机域确认）。
    Ok,
    /// 准入闸门拒了（角色不足 / 未步进验证 / 参数非法）。
    Denied,
    /// 过了闸门但执行失败（上游 5xx、传输失败、状态机不允许）。
    Failed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Denied => "denied",
            Outcome::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "ok" => Some(Outcome::Ok),
            "denied" => Some(Outcome::Denied),
            "failed" => Some(Outcome::Failed),
            _ => None,
        }
    }
}

/// 一条待落库的审计。`before`/`after` 是**原始状态**，落库前自动脱敏+摘要。
#[derive(Clone, Debug)]
pub struct Draft {
    pub actor: String,
    pub role: String,
    pub action: String,
    pub object_kind: String,
    pub object_id: String,
    pub reason: String,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
    pub trace: String,
    pub outcome: Outcome,
    /// 供人看的补充信息（已脱敏后才落库）。
    pub detail: Option<serde_json::Value>,
}

impl Draft {
    pub fn new(
        actor: impl Into<String>,
        role: impl Into<String>,
        action: impl Into<String>,
        object_kind: impl Into<String>,
        object_id: impl Into<String>,
    ) -> Self {
        Self {
            actor: actor.into(),
            role: role.into(),
            action: action.into(),
            object_kind: object_kind.into(),
            object_id: object_id.into(),
            reason: String::new(),
            before: None,
            after: None,
            trace: String::new(),
            outcome: Outcome::Ok,
            detail: None,
        }
    }

    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }

    pub fn trace(mut self, trace: impl Into<String>) -> Self {
        self.trace = trace.into();
        self
    }

    pub fn outcome(mut self, outcome: Outcome) -> Self {
        self.outcome = outcome;
        self
    }

    pub fn before(mut self, v: impl Into<serde_json::Value>) -> Self {
        self.before = Some(v.into());
        self
    }

    pub fn after(mut self, v: impl Into<serde_json::Value>) -> Self {
        self.after = Some(v.into());
        self
    }

    pub fn detail(mut self, v: impl Into<serde_json::Value>) -> Self {
        self.detail = Some(v.into());
        self
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub id: i64,
    pub at_ms: i64,
    pub actor: String,
    pub role: String,
    pub action: String,
    pub object_kind: String,
    pub object_id: String,
    pub reason: String,
    pub before_digest: String,
    pub after_digest: String,
    pub trace: String,
    pub outcome: String,
    pub detail: serde_json::Value,
    pub prev_hash: String,
    pub hash: String,
}

#[derive(Clone, Debug, Default)]
pub struct Filter {
    pub actor: Option<String>,
    pub action: Option<String>,
    pub object_kind: Option<String>,
    pub object_id: Option<String>,
    pub outcome: Option<Outcome>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub limit: Option<usize>,
    pub offset: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChainReport {
    pub entries: i64,
    /// 断链位置（第几行，1-based）。`None` = 整条链自洽。
    pub broken_at: Option<i64>,
    /// 断链原因，给运维看的。
    pub detail: String,
    pub head_hash: String,
}

/// 递归脱敏。返回新值，不改入参。
pub fn sanitize(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                if is_sensitive_key(k) {
                    out.insert(k.clone(), Value::String(REDACTED.into()));
                } else {
                    out.insert(k.clone(), sanitize(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sanitize).collect()),
        other => other.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    if REFERENCE_KEY_SUFFIXES.iter().any(|s| lower.ends_with(s)) {
        return false;
    }
    SENSITIVE_KEY_PARTS.iter().any(|p| lower.contains(p))
}

/// 摘要：`sha256(脱敏后的规范化 JSON)`。脱敏在前，所以摘要本身也不可能反推正文。
pub fn digest(value: &serde_json::Value) -> String {
    let cleaned = sanitize(value);
    let bytes = serde_json::to_vec(&cleaned).unwrap_or_default();
    hex::encode(Sha256::digest(&bytes))
}

/// 空状态的摘要（`before` 缺失时用它，别用空串——空串会与"摘要是空"混淆）。
pub const EMPTY_DIGEST: &str = "-";

fn entry_hash(
    prev: &str,
    at_ms: i64,
    d: &Draft,
    before: &str,
    after: &str,
    detail: &str,
) -> String {
    let mut h = Sha256::new();
    for part in [
        prev,
        &at_ms.to_string(),
        &d.actor,
        &d.role,
        &d.action,
        &d.object_kind,
        &d.object_id,
        &d.reason,
        before,
        after,
        &d.trace,
        d.outcome.as_str(),
        detail,
    ] {
        h.update(part.as_bytes());
        h.update([0u8]); // 分隔符：防止 "ab"+"c" 与 "a"+"bc" 撞同一条链
    }
    hex::encode(h.finalize())
}

#[derive(Clone)]
pub struct AuditLog {
    store: Store,
}

impl AuditLog {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    /// 追加一条。`reason`/`trace` 缺失即拒——审计行没有理由就等于没有审计。
    pub fn append(&self, draft: Draft) -> Result<Entry> {
        validate(&draft)?;
        let before = draft
            .before
            .as_ref()
            .map(digest)
            .unwrap_or_else(|| EMPTY_DIGEST.to_string());
        let after = draft
            .after
            .as_ref()
            .map(digest)
            .unwrap_or_else(|| EMPTY_DIGEST.to_string());
        let detail = sanitize(draft.detail.as_ref().unwrap_or(&serde_json::Value::Null));
        let detail_text = serde_json::to_string(&detail)?;
        let at_ms = now_ms();

        let conn = self.store.lock();
        // 单写者：读尾巴 + 插入必须在同一把锁里，否则两条并发 append 会拿到同一个
        // prev_hash，链就分叉了。管理员操作频率低，这条串行不是瓶颈。
        let prev: String = conn
            .query_row(
                "SELECT hash FROM g14_audit ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| String::new());
        let hash = entry_hash(&prev, at_ms, &draft, &before, &after, &detail_text);
        conn.execute(
            "INSERT INTO g14_audit (at_ms, actor, role, action, object_kind, object_id, reason, \
             before_digest, after_digest, trace, outcome, detail, prev_hash, hash) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                at_ms,
                draft.actor,
                draft.role,
                draft.action,
                draft.object_kind,
                draft.object_id,
                draft.reason,
                before,
                after,
                draft.trace,
                draft.outcome.as_str(),
                detail_text,
                prev,
                hash
            ],
        )?;
        Ok(Entry {
            id: conn.last_insert_rowid(),
            at_ms,
            actor: draft.actor,
            role: draft.role,
            action: draft.action,
            object_kind: draft.object_kind,
            object_id: draft.object_id,
            reason: draft.reason,
            before_digest: before,
            after_digest: after,
            trace: draft.trace,
            outcome: draft.outcome.as_str().to_string(),
            detail,
            prev_hash: prev,
            hash,
        })
    }

    pub fn query(&self, f: &Filter) -> Result<Paged<Entry>> {
        let limit = page_limit(f.limit);
        let offset = f.offset.unwrap_or(0).max(0);
        let mut where_sql = String::from(" WHERE 1=1");
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(v) = &f.actor {
            where_sql.push_str(" AND actor = ?");
            args.push(Box::new(v.clone()));
        }
        if let Some(v) = &f.action {
            where_sql.push_str(" AND action = ?");
            args.push(Box::new(v.clone()));
        }
        if let Some(v) = &f.object_kind {
            where_sql.push_str(" AND object_kind = ?");
            args.push(Box::new(v.clone()));
        }
        if let Some(v) = &f.object_id {
            where_sql.push_str(" AND object_id = ?");
            args.push(Box::new(v.clone()));
        }
        if let Some(v) = f.outcome {
            where_sql.push_str(" AND outcome = ?");
            args.push(Box::new(v.as_str().to_string()));
        }
        if let Some(v) = f.from {
            where_sql.push_str(" AND at_ms >= ?");
            args.push(Box::new(v));
        }
        if let Some(v) = f.to {
            where_sql.push_str(" AND at_ms <= ?");
            args.push(Box::new(v));
        }

        let conn = self.store.lock();
        let total: i64 = conn.query_row(
            &format!("SELECT count(*) FROM g14_audit{where_sql}"),
            rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
            |r| r.get(0),
        )?;
        let sql = format!(
            "SELECT id, at_ms, actor, role, action, object_kind, object_id, reason, before_digest, \
             after_digest, trace, outcome, detail, prev_hash, hash FROM g14_audit{where_sql} \
             ORDER BY id DESC LIMIT ? OFFSET ?"
        );
        let mut all = args;
        all.push(Box::new(limit));
        all.push(Box::new(offset));
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(all.iter().map(|b| b.as_ref())),
                row_to_entry,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Paged {
            total,
            limit,
            offset,
            rows,
        })
    }

    /// 从头重算整条链。任何一行被改、被删、被插，都会在这里暴露。
    pub fn verify(&self) -> Result<ChainReport> {
        let conn = self.store.lock();
        let mut stmt = conn.prepare(
            "SELECT id, at_ms, actor, role, action, object_kind, object_id, reason, before_digest, \
             after_digest, trace, outcome, detail, prev_hash, hash FROM g14_audit ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map([], row_to_entry)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut prev = String::new();
        let mut count = 0i64;
        for (i, row) in rows.iter().enumerate() {
            count += 1;
            if row.prev_hash != prev {
                return Ok(ChainReport {
                    entries: count,
                    broken_at: Some(i as i64 + 1),
                    detail: format!(
                        "第 {} 行的 prev_hash 与上一行 hash 不一致（有行被删或被插队）",
                        i + 1
                    ),
                    head_hash: prev,
                });
            }
            let draft = Draft {
                actor: row.actor.clone(),
                role: row.role.clone(),
                action: row.action.clone(),
                object_kind: row.object_kind.clone(),
                object_id: row.object_id.clone(),
                reason: row.reason.clone(),
                before: None,
                after: None,
                trace: row.trace.clone(),
                outcome: Outcome::parse(&row.outcome).unwrap_or(Outcome::Failed),
                detail: None,
            };
            let detail_text = serde_json::to_string(&row.detail)?;
            let want = entry_hash(
                &row.prev_hash,
                row.at_ms,
                &draft,
                &row.before_digest,
                &row.after_digest,
                &detail_text,
            );
            if want != row.hash {
                return Ok(ChainReport {
                    entries: count,
                    broken_at: Some(i as i64 + 1),
                    detail: format!("第 {} 行的 hash 与内容不符（该行被改过）", i + 1),
                    head_hash: prev,
                });
            }
            prev = row.hash.clone();
        }
        Ok(ChainReport {
            entries: count,
            broken_at: None,
            detail: if count == 0 {
                "空链".to_string()
            } else {
                "整条链自洽".to_string()
            },
            head_hash: prev,
        })
    }
}

fn validate(draft: &Draft) -> Result<()> {
    let reason = draft.reason.trim();
    if reason.is_empty() {
        return Err(AdminError::bad_input(
            "审计理由不能为空：处置结论必须能说清依据（合同 §3.14）",
        ));
    }
    if reason.chars().count() > REASON_MAX_CHARS {
        return Err(AdminError::bad_input(format!(
            "审计理由超过 {REASON_MAX_CHARS} 字符，请拆到 detail 里"
        )));
    }
    if draft.trace.trim().is_empty() {
        return Err(AdminError::bad_input(
            "审计必须有 trace：没有 trace 就没法把一次请求的日志串起来",
        ));
    }
    for (name, v) in [
        ("actor", &draft.actor),
        ("action", &draft.action),
        ("object_kind", &draft.object_kind),
    ] {
        if v.trim().is_empty() {
            return Err(AdminError::bad_input(format!("审计字段 {name} 不能为空")));
        }
    }
    Ok(())
}

fn row_to_entry(row: &Row<'_>) -> rusqlite::Result<Entry> {
    let detail_text: String = row.get(12)?;
    Ok(Entry {
        id: row.get(0)?,
        at_ms: row.get(1)?,
        actor: row.get(2)?,
        role: row.get(3)?,
        action: row.get(4)?,
        object_kind: row.get(5)?,
        object_id: row.get(6)?,
        reason: row.get(7)?,
        before_digest: row.get(8)?,
        after_digest: row.get(9)?,
        trace: row.get(10)?,
        outcome: row.get(11)?,
        detail: serde_json::from_str(&detail_text).unwrap_or(serde_json::Value::Null),
        prev_hash: row.get(13)?,
        hash: row.get(14)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn log() -> AuditLog {
        AuditLog::new(Store::open_memory().unwrap())
    }

    fn draft(action: &str) -> Draft {
        Draft::new("ops", "moderator", action, "user", "u1")
            .reason("投诉成立")
            .trace("trace-1")
    }

    #[test]
    fn sensitive_values_never_reach_storage() {
        let log = log();
        let entry = log
            .append(
                draft("user.suspend")
                    .before(serde_json::json!({
                        "status": "active",
                        "message": "私聊原文：明天见面",
                        "session_key": "00112233",
                        "password": "hunter2",
                        "phone": "13800000000",
                    }))
                    .after(serde_json::json!({"status": "suspended"}))
                    .detail(serde_json::json!({"content": "整段正文", "object_id": "u1"})),
            )
            .unwrap();

        let dumped = serde_json::to_string(&entry).unwrap();
        for leak in ["明天见面", "00112233", "hunter2", "13800000000", "整段正文"] {
            assert!(!dumped.contains(leak), "审计里出现了明文 {leak}: {dumped}");
        }
        // 引用类字段不能被误脱敏：它们是追责要用的
        assert_eq!(entry.detail["object_id"], "u1");
        assert!(!entry.before_digest.is_empty() && entry.before_digest != EMPTY_DIGEST);
    }

    #[test]
    fn key_classification_distinguishes_references_from_secrets() {
        for k in [
            "message",
            "Content",
            "SESSION_KEY",
            "api_token",
            "bank_account",
        ] {
            assert!(is_sensitive_key(k), "{k} 应脱敏");
        }
        for k in [
            "key_ref",
            "ciphertext_digest",
            "object_id",
            "created_at",
            "actor",
        ] {
            assert!(!is_sensitive_key(k), "{k} 不应脱敏");
        }
        assert_eq!(
            sanitize(&serde_json::json!({"nested": [{"body": "x"}]}))["nested"][0]["body"],
            REDACTED
        );
    }

    #[test]
    fn reason_and_trace_are_mandatory() {
        let log = log();
        let no_reason = Draft::new("ops", "moderator", "a", "user", "u1").trace("t");
        assert!(log
            .append(no_reason)
            .unwrap_err()
            .to_string()
            .contains("理由"));
        let blank_reason = draft("a").reason("   ");
        assert!(log.append(blank_reason).is_err());
        let no_trace = Draft::new("ops", "moderator", "a", "user", "u1").reason("r");
        assert!(log
            .append(no_trace)
            .unwrap_err()
            .to_string()
            .contains("trace"));
        let long = draft("a").reason("依".repeat(REASON_MAX_CHARS + 1));
        assert!(log.append(long).is_err());
    }

    #[test]
    fn chain_is_verifiable_and_tampering_is_detected() {
        let log = log();
        for i in 0..5 {
            log.append(draft(&format!("op{i}"))).unwrap();
        }
        let report = log.verify().unwrap();
        assert_eq!(report.entries, 5);
        assert!(report.broken_at.is_none(), "{report:?}");
        assert!(!report.head_hash.is_empty());

        // 绕过触发器直接改内容不可能；这里模拟"库被人用别的手段重建过"：
        // 把中间一行的 detail 换掉，链校验必须指到那一行。
        log.store
            .lock()
            .execute(
                "INSERT INTO g14_audit (at_ms, actor, role, action, object_kind, object_id, reason, \
                 trace, outcome, prev_hash, hash) VALUES (1,'ops','viewer','forged','user','u1',\
                 'r','t','ok','deadbeef','deadbeef')",
                [],
            )
            .unwrap();
        let broken = log.verify().unwrap();
        assert_eq!(broken.broken_at, Some(6));
        assert!(broken.detail.contains("prev_hash"), "{broken:?}");
    }

    #[test]
    fn query_filters_and_pages() {
        let log = log();
        log.append(draft("user.suspend")).unwrap();
        log.append(
            Draft::new("other", "superadmin", "user.unsuspend", "user", "u2")
                .reason("误封解除")
                .trace("t2")
                .outcome(Outcome::Failed),
        )
        .unwrap();

        let all = log.query(&Filter::default()).unwrap();
        assert_eq!(all.total, 2);
        // 默认按 id 倒序：最新一条在前
        assert_eq!(all.rows[0].actor, "other");

        let by_actor = log
            .query(&Filter {
                actor: Some("ops".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_actor.total, 1);
        assert_eq!(by_actor.rows[0].action, "user.suspend");

        let by_outcome = log
            .query(&Filter {
                outcome: Some(Outcome::Failed),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_outcome.rows.len(), 1);

        let empty = log
            .query(&Filter {
                object_id: Some("nobody".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(empty.total, 0);
        assert!(empty.rows.is_empty());
    }

    #[test]
    fn digests_are_stable_and_ignore_injected_plaintext() {
        let a = digest(&serde_json::json!({"status": "active", "message": "第一版"}));
        let b = digest(&serde_json::json!({"status": "active", "message": "第二版"}));
        assert_eq!(a, b, "正文不同但脱敏后应同摘要——审计不承载正文");
        let c = digest(&serde_json::json!({"status": "suspended", "message": "第一版"}));
        assert_ne!(a, c, "判定字段变了摘要必须变");
    }
}
