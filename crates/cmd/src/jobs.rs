//! 长任务（作业）控制：**可查、可取消、输出脱敏**。
//!
//! 这一层管的是"协议侧作业账"：谁提交的、跑到哪、有没有被取消。
//! 真正的执行仍在服务端（转码、导出、批量操作）；这里不假装能远程杀进程——
//! 取消只把本地账改成 `Cancelled` 并回一句"远端取消待 G04 的作业 API"，
//! 宁可说清边界，也不让调用方以为已经停下来了。

use std::collections::HashMap;

use serde_json::{json, Value};

/// 作业状态。终态一旦到达就不能再变（除取消外的二次变更一律拒绝）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobState {
    /// 运行中，带 0–100 的进度。
    Running { percent: u8 },
    /// 完成，带（已脱敏的）结果。
    Done,
    /// 失败，带可读原因。
    Failed { error: String },
    /// 已取消（协议侧）。
    Cancelled,
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, JobState::Running { .. })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            JobState::Running { .. } => "running",
            JobState::Done => "done",
            JobState::Failed { .. } => "failed",
            JobState::Cancelled => "cancelled",
        }
    }
}

/// 目前唯一会长时间运行的命令（转码）。新增长任务时把它加到 `LONG_RUNNING` 表。
pub const LONG_RUNNING_ACTION: &str = "video.job.create";

/// 一个作业。`operation_id` 与 `request_id` 是审计追踪用的把手（G15-05）。
#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub action_id: String,
    pub operation_id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub state: JobState,
    pub output: Option<Value>,
}

impl Job {
    pub fn to_json(&self) -> Value {
        json!({
            "job_id": self.id,
            "action_id": self.action_id,
            "operation_id": self.operation_id,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "state": self.state.as_str(),
            "percent": match self.state {
                JobState::Running { percent } => json!(percent),
                _ => Value::Null,
            },
            "output": self.output,
        })
    }
}

/// 作业操作失败的原因。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobError {
    Unknown(String),
    /// 已经结束：取消/更新都没意义了。
    AlreadyFinished {
        id: String,
        state: &'static str,
    },
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobError::Unknown(id) => write!(f, "unknown job {id:?} (call jobs/list)"),
            JobError::AlreadyFinished { id, state } => {
                write!(f, "job {id} already finished ({state})")
            }
        }
    }
}

impl std::error::Error for JobError {}

/// 取消的结果：本地账一定变了，远端是否停住要如实说。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelOutcome {
    pub job_id: String,
    /// 服务端是否已确认停止。当前没有远端取消 API，所以恒为 false。
    pub remote_stopped: bool,
}

impl CancelOutcome {
    pub fn to_json(&self) -> Value {
        json!({
            "job_id": self.job_id,
            "cancelled": true,
            "remote_stopped": self.remote_stopped,
            "note": if self.remote_stopped {
                "服务端已确认停止"
            } else {
                "仅协议侧取消：服务端作业取消 API 待 G04 提供，结果可能仍会写入"
            },
        })
    }
}

/// 作业账本（进程内）。MCP 一次会话一个实例足够：跨进程的作业要靠服务端 API。
#[derive(Debug, Default)]
pub struct JobRegistry {
    jobs: HashMap<String, Job>,
    seq: u64,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个新作业。`id` 递增且带时间戳，避免不同进程撞号。
    pub fn submit(&mut self, action_id: &str, operation_id: &str, now: i64) -> Job {
        self.seq += 1;
        let id = format!("job-{now}-{:04}", self.seq);
        let job = Job {
            id: id.clone(),
            action_id: action_id.to_string(),
            operation_id: operation_id.to_string(),
            created_at: now,
            updated_at: now,
            state: JobState::Running { percent: 0 },
            output: None,
        };
        self.jobs.insert(id, job.clone());
        job
    }

    /// 更新进度。进度只增不减，且被夹在 0–100：倒退的进度条没有意义。
    pub fn progress(&mut self, id: &str, percent: u8, now: i64) -> Result<(), JobError> {
        let job = self.job_mut(id)?;
        let current = match job.state {
            JobState::Running { percent: p } => p,
            _ => 0,
        };
        job.state = JobState::Running {
            percent: percent.clamp(current, 100),
        };
        job.updated_at = now;
        Ok(())
    }

    /// 完成并附结果（结果在入库前先脱敏）。
    pub fn finish(&mut self, id: &str, output: Value, now: i64) -> Result<(), JobError> {
        let job = self.job_mut(id)?;
        job.output = Some(redact(&output));
        job.state = JobState::Done;
        job.updated_at = now;
        Ok(())
    }

    pub fn fail(&mut self, id: &str, error: &str, now: i64) -> Result<(), JobError> {
        let job = self.job_mut(id)?;
        job.state = JobState::Failed {
            error: redact_text(error),
        };
        job.updated_at = now;
        Ok(())
    }

    /// 取消。已经结束的作业再取消会被拒——静默成功会让调用方以为它停了。
    pub fn cancel(&mut self, id: &str, now: i64) -> Result<CancelOutcome, JobError> {
        let job = self.job_mut(id)?;
        job.state = JobState::Cancelled;
        job.updated_at = now;
        Ok(CancelOutcome {
            job_id: id.to_string(),
            remote_stopped: false,
        })
    }

    pub fn get(&self, id: &str) -> Option<&Job> {
        self.jobs.get(id)
    }

    /// 按创建顺序列出，方便人从上到下读。
    pub fn list(&self) -> Vec<&Job> {
        let mut jobs: Vec<&Job> = self.jobs.values().collect();
        jobs.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        jobs
    }

    pub fn running(&self) -> usize {
        self.jobs
            .values()
            .filter(|j| !j.state.is_terminal())
            .count()
    }

    fn job_mut(&mut self, id: &str) -> Result<&mut Job, JobError> {
        let job = self
            .jobs
            .get_mut(id)
            .ok_or_else(|| JobError::Unknown(id.to_string()))?;
        if job.state.is_terminal() {
            return Err(JobError::AlreadyFinished {
                id: id.to_string(),
                state: job.state.as_str(),
            });
        }
        Ok(job)
    }
}

/// 需要脱敏的键名（小写比较，包含即命中）。
const SENSITIVE_KEYS: [&str; 8] = [
    "token",
    "secret",
    "password",
    "passwd",
    "authorization",
    "cookie",
    "private_key",
    "id_card",
];

/// 输出脱敏：默认脱敏，只有明确不需要的例子才逐条豁免。
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let key = k.to_lowercase();
                    let masked = SENSITIVE_KEYS.iter().any(|s| key.contains(s));
                    (k.clone(), if masked { json!("***") } else { redact(v) })
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

/// 文本脱敏：错误消息里常见的 `key=value` / `Bearer xxx` 形态。
fn redact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_inclusive(' ') {
        let lower = word.to_lowercase();
        let hit = SENSITIVE_KEYS.iter().any(|k| lower.contains(k));
        out.push_str(if hit { "*** " } else { word });
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_lifecycle_is_append_only() {
        let mut reg = JobRegistry::new();
        let job = reg.submit("video.job.create", "op-1", 100);
        assert_eq!(job.state, JobState::Running { percent: 0 });

        reg.progress(&job.id, 30, 101).expect("progress");
        assert_eq!(
            reg.get(&job.id).expect("job").state,
            JobState::Running { percent: 30 }
        );

        reg.finish(&job.id, json!({"job_id": "j-1"}), 102)
            .expect("finish");
        let done = reg.get(&job.id).expect("job");
        assert_eq!(done.state, JobState::Done);
        assert_eq!(done.output.as_ref().expect("output")["job_id"], "j-1");
        assert_eq!(done.updated_at, 102);
    }

    /// 进度只增不减，且被夹在 0–100。
    #[test]
    fn progress_never_goes_backwards_or_past_100() {
        let mut reg = JobRegistry::new();
        let job = reg.submit("video.job.create", "op-1", 100);
        reg.progress(&job.id, 60, 101).expect("progress");
        reg.progress(&job.id, 20, 102).expect("progress");
        assert_eq!(
            reg.get(&job.id).expect("job").state,
            JobState::Running { percent: 60 }
        );
        reg.progress(&job.id, 250, 103).expect("progress");
        assert_eq!(
            reg.get(&job.id).expect("job").state,
            JobState::Running { percent: 100 }
        );
    }

    #[test]
    fn cancelling_finished_work_is_refused_loudly() {
        let mut reg = JobRegistry::new();
        let job = reg.submit("video.job.create", "op-1", 100);
        reg.cancel(&job.id, 101).expect("cancel");
        assert_eq!(reg.get(&job.id).expect("job").state, JobState::Cancelled);

        let err = reg.cancel(&job.id, 102).unwrap_err();
        assert!(
            matches!(err, JobError::AlreadyFinished { .. }),
            "重复取消不该假装成功：{err:?}"
        );
        assert_eq!(reg.running(), 0);
    }

    /// 取消要如实说"远端没停"：不能让调用方以为作业已经终止。
    #[test]
    fn cancel_reports_that_remote_side_is_not_stopped() {
        let mut reg = JobRegistry::new();
        let job = reg.submit("video.job.create", "op-1", 100);
        let outcome = reg.cancel(&job.id, 101).expect("cancel");
        assert!(!outcome.remote_stopped);
        let payload = outcome.to_json();
        assert_eq!(payload["cancelled"], true);
        assert_eq!(payload["remote_stopped"], false);
        assert!(payload["note"].as_str().expect("note").contains("G04"));
    }

    #[test]
    fn unknown_jobs_are_reported_as_such() {
        let mut reg = JobRegistry::new();
        let err = reg.cancel("job-nope", 1).unwrap_err();
        assert_eq!(err, JobError::Unknown("job-nope".into()));
        assert!(err.to_string().contains("jobs/list"), "{err}");
    }

    /// 输出默脱敏：令牌、密码、Authorization 一律星号。
    #[test]
    fn outputs_are_redacted_before_being_stored() {
        let mut reg = JobRegistry::new();
        let job = reg.submit("video.job.create", "op-1", 100);
        reg.finish(
            &job.id,
            json!({
                "download_url": "https://x/y.mp4",
                "access_token": "wxlike-secret",
                "nested": {"Authorization": "Bearer abc", "ok": 1},
                "items": [{"password": "p"}],
            }),
            101,
        )
        .expect("finish");
        let out = reg.get(&job.id).expect("job").output.clone().expect("out");
        assert_eq!(out["access_token"], "***");
        assert_eq!(out["nested"]["Authorization"], "***");
        assert_eq!(out["items"][0]["password"], "***");
        assert_eq!(out["download_url"], "https://x/y.mp4", "非敏感字段要保留");
        assert_eq!(out["nested"]["ok"], 1);
    }

    #[test]
    fn failure_messages_are_redacted_too() {
        let mut reg = JobRegistry::new();
        let job = reg.submit("video.job.create", "op-1", 100);
        reg.fail(&job.id, "auth failed: token=abc123 expired", 101)
            .expect("fail");
        let state = reg.get(&job.id).expect("job").state.clone();
        let JobState::Failed { error } = state else {
            panic!("expected failed");
        };
        assert!(!error.contains("abc123"), "{error}");
        assert!(error.contains("***"), "{error}");
    }
}
