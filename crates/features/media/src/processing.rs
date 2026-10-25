//! Isolated media decoding and transcoding jobs.
//!
//! Every job has one row keyed by `(object_id, preset)`, so a retried submit is
//! the same job instead of a second copy. Inputs are already stored objects
//! copied into a private work directory: remote URLs never reach a decoder, and
//! the FFmpeg child runs with `-protocol_whitelist file`, an empty environment,
//! a wall-clock deadline and (on Unix) CPU/memory/file-size rlimits.
//!
//! The runner is injected. Production uses [`FfmpegRunner`]; tests use a
//! fixture runner to exercise scheduling, budgets and cancellation without
//! depending on a machine-installed codec.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};

use crate::access::{add_ref_conn, RefSpec};
use crate::db::now_ms;
use crate::error::{MediaError, Result};
use crate::ids;
use crate::model::{Artifact, Audience, JobState, ObjectRecord, Preset, Purpose, RefType};
use crate::store::stream_digest_file;
use crate::{MediaService, GIB, MIB};

pub const MAX_DURATION_MS: u64 = 2 * 3600 * 1000;
pub const MAX_PIXELS: u64 = 3840 * 2160;
pub const MAX_FRAMES: u64 = 30 * 60 * 30;
pub const MAX_INPUT_BYTES: u64 = 2 * GIB;
pub const MAX_OUTPUT_BYTES: u64 = 512 * MIB;
pub const JOB_TIMEOUT_MS: u64 = 10 * 60 * 1000;
pub const MAX_CPU_SECONDS: u64 = 600;
pub const MAX_MEMORY_BYTES: u64 = GIB;
pub const MAX_THREADS: u16 = 2;
pub const WAVEFORM_RATE: u32 = 8000;
pub const WAVEFORM_BARS: usize = 40;
pub const WAVEFORM_MAX: u8 = 100;
pub const PROBE_HEAD_BYTES: usize = 4096;
pub const THUMBNAIL_EDGE: u32 = 640;
pub const AUDIO_BITRATE_KBPS: u32 = 96;
pub const RETRY_BACKOFF_MS: i64 = 30 * 1000;
pub const SAMPLE_BYTES: usize = 2;
pub const BYTE_MAX: u8 = 255;
const PERCENT: u8 = 100;
const POLL_MS: u64 = 50;
const SAMPLE_MAX: i16 = 32767;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Budget {
    pub max_input_bytes: u64,
    pub max_duration_ms: u64,
    pub max_pixels: u64,
    pub max_frames: u64,
    pub max_output_bytes: u64,
    pub timeout_ms: u64,
    pub cpu_seconds: u64,
    pub memory_bytes: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_input_bytes: MAX_INPUT_BYTES,
            max_duration_ms: MAX_DURATION_MS,
            max_pixels: MAX_PIXELS,
            max_frames: MAX_FRAMES,
            max_output_bytes: MAX_OUTPUT_BYTES,
            timeout_ms: JOB_TIMEOUT_MS,
            cpu_seconds: MAX_CPU_SECONDS,
            memory_bytes: MAX_MEMORY_BYTES,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeInfo {
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub has_video: bool,
    pub has_audio: bool,
    pub format: String,
}

#[derive(Clone, Debug)]
pub struct TransformRequest {
    pub input: PathBuf,
    pub preset: Preset,
    pub output_dir: PathBuf,
    pub budget: Budget,
    pub deadline_ms: i64,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Clone, Debug)]
pub struct TransformOutput {
    pub kind: String,
    pub path: PathBuf,
    pub mime: String,
}

/// The decoding seam. Implementations must refuse URLs, repeated presets and
/// inputs outside `request.input`.
pub trait TransformRunner: Send + Sync {
    fn probe(&self, input: &Path, budget: &Budget) -> Result<ProbeInfo>;
    fn transform(&self, request: &TransformRequest) -> Result<Vec<TransformOutput>>;
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct JobView {
    pub job_id: String,
    pub object_id: String,
    pub preset: Preset,
    pub state: JobState,
    pub attempts: u32,
    pub progress: u8,
    pub outputs: Vec<Artifact>,
    pub error: Option<String>,
    pub can_cancel: bool,
}

#[derive(Clone, Debug)]
struct JobRow {
    job_id: String,
    owner: String,
    object_id: String,
    preset: Preset,
    state: JobState,
    attempts: u32,
    budget: Budget,
    outputs: Vec<Artifact>,
    error: Option<String>,
}

fn row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
    let budget: String = row.get(6)?;
    let outputs: String = row.get(7)?;
    Ok(JobRow {
        job_id: row.get(0)?,
        owner: row.get(1)?,
        object_id: row.get(2)?,
        preset: Preset::parse(&row.get::<_, String>(3)?).map_err(to_sql)?,
        state: JobState::parse(&row.get::<_, String>(4)?).map_err(to_sql)?,
        attempts: row.get::<_, i64>(5)?.max(0) as u32,
        budget: serde_json::from_str(&budget).unwrap_or_default(),
        outputs: serde_json::from_str(&outputs).unwrap_or_default(),
        error: row.get(8)?,
    })
}

fn to_sql(err: MediaError) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(err))
}

fn view_of(row: JobRow) -> JobView {
    let progress = if row.state == JobState::Succeeded {
        PERCENT
    } else {
        0
    };
    JobView {
        job_id: row.job_id,
        object_id: row.object_id,
        preset: row.preset,
        state: row.state,
        attempts: row.attempts,
        progress,
        outputs: row.outputs,
        error: row.error,
        can_cancel: !row.state.terminal(),
    }
}

/// Match the first bytes against known container signatures. Executables and
/// scripts are refused before a decoder ever sees them.
pub fn sniff_container(head: &[u8]) -> Result<&'static str> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G'];
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF];
    const GIF: &[u8] = b"GIF8";
    const RIFF: &[u8] = b"RIFF";
    const WEBP: &[u8] = b"WEBP";
    const FTYP: &[u8] = b"ftyp";
    if head.starts_with(PNG) {
        return Ok("image/png");
    }
    if head.starts_with(JPEG) {
        return Ok("image/jpeg");
    }
    if head.starts_with(GIF) {
        return Ok("image/gif");
    }
    if head.starts_with(RIFF) && head.len() > 12 && &head[8..12] == WEBP {
        return Ok("image/webp");
    }
    if head.len() > 12 && &head[4..8] == FTYP {
        return Ok("video/mp4");
    }
    if head.starts_with(b"ELF") || head.starts_with(b"MZ") || head.starts_with(b"#!") {
        return Err(MediaError::Unsupported("executables are not media".into()));
    }
    Err(MediaError::Unsupported("unrecognised container".into()))
}

fn validate_probe(probe: &ProbeInfo, preset: Preset, budget: &Budget) -> Result<()> {
    if probe.duration_ms == 0 || probe.duration_ms > budget.max_duration_ms {
        return Err(MediaError::BudgetExceeded("duration".into()));
    }
    if probe.frames > budget.max_frames {
        return Err(MediaError::BudgetExceeded("frame count".into()));
    }
    if preset != Preset::Waveform {
        if !probe.has_video {
            return Err(MediaError::Unsupported(
                "preset needs a video stream".into(),
            ));
        }
        let pixels = u64::from(probe.width) * u64::from(probe.height);
        if probe.width == 0 || probe.height == 0 || pixels > budget.max_pixels {
            return Err(MediaError::BudgetExceeded("pixel count".into()));
        }
    }
    if preset == Preset::Waveform && !probe.has_audio {
        return Err(MediaError::Unsupported(
            "waveform needs an audio stream".into(),
        ));
    }
    Ok(())
}

impl MediaService {
    /// Submit is idempotent per `(object, preset)`: the second call returns the
    /// first job. The caller must be allowed to read the source object.
    pub fn submit_job(&self, user_id: &str, object_id: &str, preset: Preset) -> Result<JobView> {
        self.authorize_read(user_id, object_id)?;
        let existing = self.find_job(object_id, preset)?;
        if let Some(row) = existing {
            if row.owner == user_id {
                return Ok(view_of(row));
            }
            return Err(MediaError::Forbidden);
        }
        let now = now_ms();
        let job_id = ids::opaque_id("job");
        let budget = serde_json::to_string(&Budget::default())?;
        {
            let c = self.db.lock();
            c.execute(
                "INSERT OR IGNORE INTO media_job
                    (job_id, owner, object_id, preset, state, attempts, next_retry_at,
                     budget, outputs, error, created_at, updated_at)
                 VALUES(?1, ?2, ?3, ?4, 'pending', 0, ?5, ?6, '[]', NULL, ?7, ?7)",
                params![
                    job_id,
                    user_id,
                    object_id,
                    preset.as_str(),
                    now,
                    budget,
                    now
                ],
            )?;
        }
        let row = self
            .find_job(object_id, preset)?
            .ok_or_else(|| MediaError::internal("job insert vanished"))?;
        Ok(view_of(row))
    }

    pub fn job_status(&self, user_id: &str, job_id: &str) -> Result<JobView> {
        Ok(view_of(self.owned_job(user_id, job_id)?))
    }

    pub fn cancel_job(&self, user_id: &str, job_id: &str) -> Result<JobView> {
        let row = self.owned_job(user_id, job_id)?;
        if row.state.terminal() {
            return Ok(view_of(row));
        }
        self.flag_cancel(job_id);
        let now = now_ms();
        {
            let c = self.db.lock();
            c.execute(
                "UPDATE media_job SET state = 'canceled', updated_at = ?1
                 WHERE job_id = ?2 AND owner = ?3 AND state IN ('pending', 'running')",
                params![now, job_id, user_id],
            )?;
        }
        self.remove_work_dir(job_id);
        self.owned_job(user_id, job_id).map(view_of)
    }

    fn flag_cancel(&self, job_id: &str) {
        if let Some(flag) = self
            .cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(job_id)
        {
            flag.store(true, Ordering::SeqCst);
        }
    }

    fn remove_work_dir(&self, job_id: &str) {
        std::fs::remove_dir_all(self.store.work_path(job_id)).ok();
    }

    fn find_job(&self, object_id: &str, preset: Preset) -> Result<Option<JobRow>> {
        Ok(self
            .db
            .lock()
            .query_row(
                "SELECT job_id, owner, object_id, preset, state, attempts, budget, outputs, error
                 FROM media_job WHERE object_id = ?1 AND preset = ?2",
                params![object_id, preset.as_str()],
                row_from_sql,
            )
            .optional()?)
    }

    fn owned_job(&self, user_id: &str, job_id: &str) -> Result<JobRow> {
        let row = self
            .db
            .lock()
            .query_row(
                "SELECT job_id, owner, object_id, preset, state, attempts, budget, outputs, error
                 FROM media_job WHERE job_id = ?1",
                params![job_id],
                row_from_sql,
            )
            .optional()?;
        match row {
            Some(row) if row.owner == user_id => Ok(row),
            _ => Err(MediaError::NotFound),
        }
    }

    fn due_job_ids(&self, now: i64, limit: u32) -> Result<Vec<String>> {
        let c = self.db.lock();
        let mut stmt = c.prepare(
            "SELECT job_id FROM media_job
             WHERE (state = 'pending' AND next_retry_at <= ?1)
                OR (state = 'running' AND updated_at < ?2)
             ORDER BY created_at LIMIT ?3",
        )?;
        let stale_before = now - self.config.job_stale_ms;
        let rows = stmt.query_map(params![now, stale_before, i64::from(limit)], |r| r.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Process due jobs synchronously. Call from the scheduler thread; each job
    /// owns a private work directory and never holds the database lock while a
    /// child process runs.
    pub fn run_due_jobs(&self, limit: u32) -> Result<usize> {
        let now = now_ms();
        let ids = self.due_job_ids(now, limit)?;
        let mut done = 0;
        for job_id in ids {
            if self.claim_job(&job_id, now)? {
                self.execute_job(&job_id);
                done += 1;
            }
        }
        Ok(done)
    }

    fn claim_job(&self, job_id: &str, now: i64) -> Result<bool> {
        let c = self.db.lock();
        let changed = c.execute(
            "UPDATE media_job SET state = 'running', attempts = attempts + 1, updated_at = ?1
             WHERE job_id = ?2 AND (state = 'pending' OR state = 'running')",
            params![now, job_id],
        )?;
        Ok(changed > 0)
    }

    fn execute_job(&self, job_id: &str) {
        let outcome = self.run_job(job_id);
        self.remove_work_dir(job_id);
        if let Err(err) = outcome {
            self.settle_failure(job_id, err);
        }
    }

    fn run_job(&self, job_id: &str) -> Result<()> {
        let row = self.job_row_any(job_id)?;
        if row.state == JobState::Canceled {
            return Ok(());
        }
        let source = self.object(&row.object_id)?;
        let work = self.store.work_path(job_id);
        std::fs::create_dir_all(&work)?;
        let input = work.join("input.bin");
        self.copy_source(&source, &input)?;
        self.sniff_input(&input)?;
        let probe = self.runner.probe(&input, &row.budget)?;
        validate_probe(&probe, row.preset, &row.budget)?;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job_id.to_string(), Arc::clone(&cancel));
        let request = TransformRequest {
            input,
            preset: row.preset,
            output_dir: work.join("out"),
            budget: row.budget.clone(),
            deadline_ms: now_ms() + row.budget.timeout_ms as i64,
            cancel,
        };
        std::fs::create_dir_all(&request.output_dir)?;
        let result = self.runner.transform(&request);
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(job_id);
        let outputs = result?;
        self.publish_artifacts(&row, &outputs)
    }

    fn job_row_any(&self, job_id: &str) -> Result<JobRow> {
        let row = self
            .db
            .lock()
            .query_row(
                "SELECT job_id, owner, object_id, preset, state, attempts, budget, outputs, error
                 FROM media_job WHERE job_id = ?1",
                params![job_id],
                row_from_sql,
            )
            .optional()?;
        row.ok_or(MediaError::NotFound)
    }

    fn copy_source(&self, source: &ObjectRecord, input: &Path) -> Result<()> {
        let Some(mut file) = self.store.open(&source.object_id)? else {
            return Err(MediaError::NotFound);
        };
        let mut out = std::fs::File::create(input)?;
        std::io::copy(&mut file, &mut out)?;
        Ok(())
    }

    fn sniff_input(&self, input: &Path) -> Result<()> {
        let mut file = std::fs::File::open(input)?;
        let mut head = vec![0u8; PROBE_HEAD_BYTES];
        let read = file.read(&mut head)?;
        head.truncate(read);
        sniff_container(&head).map(|_| ())
    }

    /// Derived artifacts live exactly as long as their source. Called from
    /// [`crate::files::MediaService::gc_object`] before the source blob is
    /// removed, so a cleanup never leaves an orphan rendition behind.
    pub(crate) fn purge_derived_for_source(&self, source_id: &str) -> Result<usize> {
        let jobs: Vec<(String, String)> = {
            let c = self.db.lock();
            let mut stmt =
                c.prepare("SELECT job_id, outputs FROM media_job WHERE object_id = ?1")?;
            let rows = stmt.query_map(params![source_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let mut removed = 0;
        for (job_id, raw) in jobs {
            let outputs: Vec<Artifact> = serde_json::from_str(&raw).unwrap_or_default();
            for artifact in outputs {
                {
                    let c = self.db.lock();
                    crate::access::remove_ref_conn(
                        &c,
                        &artifact.object_id,
                        RefType::Derived,
                        &job_id,
                        now_ms(),
                    )?;
                }
                if self.gc_object(&artifact.object_id)? {
                    removed += 1;
                }
            }
            let c = self.db.lock();
            c.execute(
                "UPDATE media_job SET outputs = '[]' WHERE job_id = ?1",
                params![job_id],
            )?;
        }
        Ok(removed)
    }

    fn publish_artifacts(&self, row: &JobRow, outputs: &[TransformOutput]) -> Result<()> {
        if outputs.is_empty() {
            return Err(MediaError::Unsupported("runner produced no outputs".into()));
        }
        let mut artifacts = Vec::new();
        for output in outputs {
            let size = std::fs::metadata(&output.path)?.len();
            if size == 0 || size > row.budget.max_output_bytes {
                return Err(MediaError::BudgetExceeded(format!(
                    "{} output size",
                    output.kind
                )));
            }
            let digest = stream_digest_file(&output.path)?;
            self.store.put_file(&digest, &output.path)?;
            artifacts.push(Artifact {
                kind: output.kind.clone(),
                object_id: digest.clone(),
                size,
                digest,
                mime: output.mime.clone(),
            });
        }
        self.finish_success(row, artifacts)
    }

    fn finish_success(&self, row: &JobRow, artifacts: Vec<Artifact>) -> Result<()> {
        let now = now_ms();
        let outputs = serde_json::to_string(&artifacts)?;
        {
            let c = self.db.lock();
            let tx = c.unchecked_transaction()?;
            for artifact in &artifacts {
                tx.execute(
                    "INSERT INTO media_object
                        (object_id, owner, size, digest, mime, status, access_epoch, created_at)
                     VALUES(?1, ?2, ?3, ?1, ?4, 'ready', 0, ?5)
                     ON CONFLICT(object_id) DO NOTHING",
                    params![
                        artifact.object_id,
                        row.owner,
                        artifact.size as i64,
                        artifact.mime,
                        now
                    ],
                )?;
                let audience = Audience::User {
                    id: row.owner.clone(),
                };
                let spec = RefSpec {
                    ref_type: RefType::Derived,
                    ext_id: &row.job_id,
                    owner: &row.owner,
                    audience: &audience,
                    purpose: Purpose::Derived,
                };
                add_ref_conn(&tx, &artifact.object_id, &spec, now)?;
            }
            tx.execute(
                "UPDATE media_job SET state = 'succeeded', outputs = ?1, error = NULL,
                        updated_at = ?2 WHERE job_id = ?3",
                params![outputs, now, row.job_id],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    fn settle_failure(&self, job_id: &str, err: MediaError) {
        let Ok(row) = self.job_row_any(job_id) else {
            return;
        };
        if row.state == JobState::Canceled {
            return;
        }
        let terminal = row.state.terminal() || row.attempts >= self.config.max_job_attempts;
        let retryable = !matches!(
            err,
            MediaError::Canceled | MediaError::Unsupported(_) | MediaError::BudgetExceeded(_)
        );
        let now = now_ms();
        let (state, next_retry) = if retryable && !terminal {
            (JobState::Pending, now + RETRY_BACKOFF_MS)
        } else {
            (JobState::Failed, now)
        };
        {
            let c = self.db.lock();
            c.execute(
                "UPDATE media_job SET state = ?1, error = ?2, next_retry_at = ?3, updated_at = ?4
                 WHERE job_id = ?5 AND state <> 'canceled'",
                params![state.as_str(), err.to_string(), next_retry, now, job_id],
            )
            .ok();
        }
        tracing::warn!(job_id, error = %err, "media job failed");
    }
}

/// Real FFmpeg adapter. `ffmpeg`/`ffprobe` are looked up next to the binary
/// (`WXLIKE_FFMPEG_DIR`) or on `PATH`; nothing is executed through a shell.
pub struct FfmpegRunner {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

impl FfmpegRunner {
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
        }
    }

    pub fn from_env() -> Self {
        let dir = std::env::var_os("WXLIKE_FFMPEG_DIR").map(PathBuf::from);
        let join = |name: &str| match &dir {
            Some(dir) => dir.join(name),
            None => PathBuf::from(name),
        };
        Self::new(join("ffmpeg"), join("ffprobe"))
    }

    fn base_command(&self, program: &Path, cwd: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(cwd)
            .env_clear();
        cmd
    }

    fn probe_json(&self, input: &Path, budget: &Budget) -> Result<serde_json::Value> {
        let cwd = input.parent().unwrap_or(Path::new("."));
        let mut cmd = self.base_command(&self.ffprobe, cwd);
        cmd.args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(input);
        let deadline = Instant::now() + Duration::from_millis(budget.timeout_ms);
        let output = self.wait_with_deadline(&mut cmd, deadline, &AtomicBool::new(false))?;
        serde_json::from_slice(&output.stdout).map_err(MediaError::from)
    }

    #[cfg(unix)]
    fn apply_rlimits(cmd: &mut Command, budget: &Budget) {
        use std::os::unix::process::CommandExt as _;
        let cpu = budget.cpu_seconds;
        let memory = budget.memory_bytes;
        let fsize = budget.max_output_bytes;
        let configure = move || {
            // SAFETY: setrlimit/prctl only mutate the child after fork; no
            // allocation or locks happen in this pre-exec closure.
            unsafe {
                let cpu_limit = libc::rlimit {
                    rlim_cur: cpu,
                    rlim_max: cpu,
                };
                libc::setrlimit(libc::RLIMIT_CPU, &cpu_limit);
                let mem_limit = libc::rlimit {
                    rlim_cur: memory,
                    rlim_max: memory,
                };
                libc::setrlimit(libc::RLIMIT_AS, &mem_limit);
                let file_limit = libc::rlimit {
                    rlim_cur: fsize,
                    rlim_max: fsize,
                };
                libc::setrlimit(libc::RLIMIT_FSIZE, &file_limit);
                // prctl(PR_SET_NO_NEW_PRIVS) is Linux-only; rlimits cover the
                // rest of the platforms.
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
            }
            Ok(())
        };
        // SAFETY: see `configure`.
        unsafe {
            cmd.pre_exec(configure);
        }
    }

    #[cfg(not(unix))]
    fn apply_rlimits(_cmd: &mut Command, _budget: &Budget) {}

    fn wait_with_deadline(
        &self,
        cmd: &mut Command,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> Result<std::process::Output> {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(map_spawn_error)?;
        loop {
            check_stop(&mut child, deadline, cancel)?;
            if let Some(status) = child.try_wait()? {
                return finish_process(child, status);
            }
            std::thread::sleep(Duration::from_millis(POLL_MS));
        }
    }
}

fn check_stop(child: &mut Child, deadline: Instant, cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        kill(child);
        return Err(MediaError::Canceled);
    }
    if Instant::now() >= deadline {
        kill(child);
        return Err(MediaError::BudgetExceeded("wall clock".into()));
    }
    Ok(())
}

fn finish_process(child: Child, status: ExitStatus) -> Result<std::process::Output> {
    let output = child.wait_with_output()?;
    if status.success() {
        Ok(output)
    } else {
        Err(status_error(status, &output.stderr))
    }
}

impl TransformRunner for FfmpegRunner {
    fn probe(&self, input: &Path, budget: &Budget) -> Result<ProbeInfo> {
        let json = self.probe_json(input, budget)?;
        Ok(probe_from_json(&json))
    }

    fn transform(&self, request: &TransformRequest) -> Result<Vec<TransformOutput>> {
        validate_input_path(&request.input)?;
        match request.preset {
            Preset::Thumbnail => self.render_thumbnail(request),
            Preset::Waveform => self.render_waveform(request),
            preset => self.render_playback(request, preset),
        }
    }
}

fn probe_from_json(json: &serde_json::Value) -> ProbeInfo {
    let format = &json["format"];
    let streams = json["streams"].as_array().cloned().unwrap_or_default();
    let video = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video");
    let audio = streams
        .iter()
        .find(|stream| stream["codec_type"] == "audio");
    let fps = video
        .and_then(|v| v["avg_frame_rate"].as_str())
        .and_then(parse_fraction)
        .unwrap_or_default();
    let duration_ms = format["duration"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok())
        .map(|secs| (secs * 1000.0) as u64)
        .unwrap_or_default();
    let frames = video
        .and_then(|v| v["nb_frames"].as_str())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| (duration_ms as f64 / 1000.0 * fps) as u64);
    ProbeInfo {
        duration_ms,
        width: video.and_then(|v| v["width"].as_u64()).unwrap_or_default() as u32,
        height: video.and_then(|v| v["height"].as_u64()).unwrap_or_default() as u32,
        frames,
        has_video: video.is_some(),
        has_audio: audio.is_some(),
        format: format["format_name"].as_str().unwrap_or("unknown").into(),
    }
}

fn parse_fraction(raw: &str) -> Option<f64> {
    let (num, den) = raw.split_once('/')?;
    let num: f64 = num.parse().ok()?;
    let den: f64 = den.parse().ok()?;
    if den <= 0.0 {
        return None;
    }
    Some(num / den)
}

fn validate_input_path(input: &Path) -> Result<()> {
    let raw = input.to_string_lossy();
    if raw.contains("://") {
        return Err(MediaError::Unsupported(
            "remote inputs are not allowed".into(),
        ));
    }
    if !input.is_file() {
        return Err(MediaError::NotFound);
    }
    Ok(())
}

fn map_spawn_error(err: std::io::Error) -> MediaError {
    if err.kind() == std::io::ErrorKind::NotFound {
        return MediaError::Unsupported("ffmpeg is not installed".into());
    }
    MediaError::Io(err)
}

fn status_error(status: ExitStatus, stderr: &[u8]) -> MediaError {
    let text = String::from_utf8_lossy(stderr);
    let tail: String = text
        .chars()
        .rev()
        .take(400)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    tracing::warn!(?status, %tail, "ffmpeg failed");
    MediaError::Unsupported("decoder rejected the input".into())
}

fn kill(child: &mut Child) {
    child.kill().ok();
    child.wait().ok();
}

impl FfmpegRunner {
    fn render_thumbnail(&self, request: &TransformRequest) -> Result<Vec<TransformOutput>> {
        let path = request.output_dir.join("thumbnail.jpg");
        let scale = format!("scale={THUMBNAIL_EDGE}:-2");
        let mut cmd = self.base_command(&self.ffmpeg, &request.output_dir);
        cmd.args([
            "-nostdin",
            "-loglevel",
            "error",
            "-protocol_whitelist",
            "file",
        ])
        .args(["-i"])
        .arg(&request.input)
        .args(["-frames:v", "1", "-vf"])
        .arg(scale)
        .arg(&path);
        Self::apply_rlimits(&mut cmd, &request.budget);
        let deadline = deadline_of(request);
        self.wait_with_deadline(&mut cmd, deadline, &request.cancel)?;
        Ok(vec![output_of("thumbnail", path, "image/jpeg")])
    }

    fn render_playback(
        &self,
        request: &TransformRequest,
        preset: Preset,
    ) -> Result<Vec<TransformOutput>> {
        let height = preset
            .height()
            .ok_or_else(|| MediaError::bad("preset is not a playback rendition"))?;
        let path = request.output_dir.join(format!("playback_{height}.mp4"));
        let scale = format!("scale=-2:{height}");
        let bitrate = format!("{AUDIO_BITRATE_KBPS}k");
        let limit = format!("{}", request.budget.max_duration_ms as f64 / 1000.0);
        let threads = MAX_THREADS.to_string();
        let mut cmd = self.base_command(&self.ffmpeg, &request.output_dir);
        cmd.args([
            "-nostdin",
            "-loglevel",
            "error",
            "-protocol_whitelist",
            "file",
        ])
        .args(["-i"])
        .arg(&request.input)
        .args(["-t"])
        .arg(limit)
        .args(["-vf"])
        .arg(scale)
        .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "28"])
        .args(["-c:a", "aac", "-b:a"])
        .arg(bitrate)
        .args(["-movflags", "+faststart", "-threads"])
        .arg(threads)
        .arg(&path);
        Self::apply_rlimits(&mut cmd, &request.budget);
        let deadline = deadline_of(request);
        self.wait_with_deadline(&mut cmd, deadline, &request.cancel)?;
        Ok(vec![output_of("playback", path, "video/mp4")])
    }

    fn render_waveform(&self, request: &TransformRequest) -> Result<Vec<TransformOutput>> {
        let mut cmd = self.base_command(&self.ffmpeg, &request.output_dir);
        cmd.args([
            "-nostdin",
            "-loglevel",
            "error",
            "-protocol_whitelist",
            "file",
        ])
        .args(["-i"])
        .arg(&request.input)
        .args(["-vn", "-ac", "1", "-ar"])
        .arg(WAVEFORM_RATE.to_string())
        .args(["-f", "s16le", "-"]);
        let deadline = deadline_of(request);
        let output = self.wait_with_deadline(&mut cmd, deadline, &request.cancel)?;
        let bars = waveform_bars(&output.stdout)?;
        let path = request.output_dir.join("waveform.json");
        let duration_ms = request
            .budget
            .max_duration_ms
            .min(samples_duration(&output.stdout));
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "bars": bars,
                "duration_ms": duration_ms,
            }))?,
        )?;
        Ok(vec![output_of("waveform", path, "application/json")])
    }
}

fn deadline_of(request: &TransformRequest) -> Instant {
    let remaining = (request.deadline_ms - now_ms()).max(0) as u64;
    Instant::now() + Duration::from_millis(remaining)
}

fn output_of(kind: &str, path: PathBuf, mime: &str) -> TransformOutput {
    TransformOutput {
        kind: kind.into(),
        path,
        mime: mime.into(),
    }
}

fn samples_duration(bytes: &[u8]) -> u64 {
    let rate = u64::from(WAVEFORM_RATE);
    let samples = bytes.len() as u64 / SAMPLE_BYTES as u64;
    samples * 1000 / rate.max(1)
}

fn waveform_bars(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.is_empty() {
        return Err(MediaError::Unsupported("empty audio stream".into()));
    }
    let samples: Vec<i16> = bytes
        .as_chunks::<SAMPLE_BYTES>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect();
    if samples.is_empty() {
        return Err(MediaError::Unsupported("empty audio stream".into()));
    }
    let chunk = samples.len().div_ceil(WAVEFORM_BARS).max(1);
    let mut bars = Vec::with_capacity(WAVEFORM_BARS);
    for bucket in samples.chunks(chunk).take(WAVEFORM_BARS) {
        let peak = bucket
            .iter()
            .map(|sample| (*sample).unsigned_abs())
            .max()
            .unwrap_or_default();
        let scaled = (u32::from(peak) * u32::from(WAVEFORM_MAX)
            / u32::from(SAMPLE_MAX.unsigned_abs()))
        .min(u32::from(WAVEFORM_MAX));
        bars.push(scaled as u8);
    }
    while bars.len() < WAVEFORM_BARS {
        bars.push(0);
    }
    Ok(bars)
}

/// A deterministic runner for tests and for environments without a codec. It
/// still honours deadlines and cancellation, so scheduling tests are real.
pub struct FixtureRunner {
    pub probe: ProbeInfo,
}

impl FixtureRunner {
    pub fn new(probe: ProbeInfo) -> Self {
        Self { probe }
    }
}

impl TransformRunner for FixtureRunner {
    fn probe(&self, _input: &Path, _budget: &Budget) -> Result<ProbeInfo> {
        Ok(self.probe.clone())
    }

    fn transform(&self, request: &TransformRequest) -> Result<Vec<TransformOutput>> {
        if request.cancel.load(Ordering::SeqCst) {
            return Err(MediaError::Canceled);
        }
        let (kind, name, mime) = match request.preset {
            Preset::Thumbnail => ("thumbnail", "thumbnail.jpg", "image/jpeg"),
            Preset::Waveform => ("waveform", "waveform.json", "application/json"),
            _ => ("playback", "playback.mp4", "video/mp4"),
        };
        let path = request.output_dir.join(name);
        let payload = match request.preset {
            Preset::Waveform => serde_json::to_vec(&serde_json::json!({
                "bars": vec![0u8; WAVEFORM_BARS],
                "duration_ms": self.probe.duration_ms,
            }))?,
            _ => vec![BYTE_MAX; 64],
        };
        std::fs::write(&path, payload)?;
        Ok(vec![output_of(kind, path, mime)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executables_are_refused_before_decoding() {
        assert!(sniff_container(b"MZ\x90\x00").is_err());
        assert!(sniff_container(b"#!/bin/sh\n").is_err());
        assert!(sniff_container(b"\x89PNG\r\n").is_ok());
        assert!(sniff_container(b"not media").is_err());
    }

    #[test]
    fn budget_rejects_oversized_metadata() {
        let probe = ProbeInfo {
            duration_ms: MAX_DURATION_MS + 1,
            width: 10,
            height: 10,
            frames: 1,
            has_video: true,
            has_audio: true,
            format: "test".into(),
        };
        assert!(matches!(
            validate_probe(&probe, Preset::Thumbnail, &Budget::default()),
            Err(MediaError::BudgetExceeded(_))
        ));
    }

    struct DenyAll;
    impl crate::access::AudiencePort for DenyAll {
        fn may_read_conversation(&self, _: &str, _: &str) -> bool {
            false
        }
        fn may_read_group(&self, _: &str, _: &str) -> bool {
            false
        }
    }

    fn test_service(dir: &Path, runner: Arc<dyn TransformRunner>) -> MediaService {
        MediaService::open(
            crate::MediaConfig::new(dir).with_limits(64 * MIB, MIB),
            None,
            Arc::new(DenyAll),
        )
        .unwrap()
        .with_runner(runner)
    }

    fn upload_png(service: &MediaService) -> crate::uploads::CompletedUpload {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend(b"fixture-bytes");
        let start = service
            .upload_start(
                "u1",
                crate::UploadStart {
                    purpose: Purpose::Video,
                    name: "clip.png".into(),
                    size: bytes.len() as u64,
                    mime: "image/png".into(),
                    digest: ids::sha256_hex(&bytes),
                },
            )
            .unwrap();
        service
            .upload_chunk("u1", &start.upload_id, 0, &bytes, None)
            .unwrap();
        service
            .upload_complete("u1", &start.upload_id, Some(&ids::sha256_hex(&bytes)))
            .unwrap()
    }

    #[test]
    fn stale_running_jobs_are_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let service = test_service(
            dir.path(),
            Arc::new(FixtureRunner::new(ProbeInfo {
                duration_ms: 1000,
                width: 64,
                height: 64,
                frames: 5,
                has_video: true,
                has_audio: false,
                format: "fixture".into(),
            })),
        );
        let done = upload_png(&service);
        let job = service
            .submit_job("u1", &done.object_id, Preset::Thumbnail)
            .unwrap();
        {
            let c = service.db.lock();
            c.execute(
                "UPDATE media_job SET state = 'running', updated_at = 0 WHERE job_id = ?1",
                params![job.job_id],
            )
            .unwrap();
        }
        assert_eq!(service.run_due_jobs(10).unwrap(), 1);
        assert_eq!(
            service.job_status("u1", &job.job_id).unwrap().state,
            JobState::Succeeded
        );
    }

    #[test]
    fn derived_artifacts_are_removed_with_their_source() {
        let dir = tempfile::tempdir().unwrap();
        let service = test_service(
            dir.path(),
            Arc::new(FixtureRunner::new(ProbeInfo {
                duration_ms: 1000,
                width: 64,
                height: 64,
                frames: 5,
                has_video: true,
                has_audio: false,
                format: "fixture".into(),
            })),
        );
        let done = upload_png(&service);
        let job = service
            .submit_job("u1", &done.object_id, Preset::Thumbnail)
            .unwrap();
        service.run_due_jobs(10).unwrap();
        let artifact = service.job_status("u1", &job.job_id).unwrap().outputs[0]
            .object_id
            .clone();
        assert!(service.open_object(&artifact).unwrap().is_some());
        service.trash_file("u1", &done.file_id).unwrap();
        service.purge_file("u1", &done.file_id).unwrap();
        assert!(service.open_object(&done.object_id).unwrap().is_none());
        assert!(service.open_object(&artifact).unwrap().is_none());
    }

    #[test]
    #[ignore = "requires ffmpeg and ffprobe on PATH"]
    fn ffmpeg_thumbnail_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.mp4");
        let generated = Command::new("ffmpeg")
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=64x64:rate=5",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&input)
            .status();
        let Ok(status) = generated else {
            eprintln!("ffmpeg is not installed; skipping");
            return;
        };
        assert!(status.success());
        let runner = FfmpegRunner::from_env();
        let probe = runner.probe(&input, &Budget::default()).unwrap();
        assert!(probe.has_video);
        let out = dir.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        let request = TransformRequest {
            input,
            preset: Preset::Thumbnail,
            output_dir: out,
            budget: Budget::default(),
            deadline_ms: now_ms() + JOB_TIMEOUT_MS as i64,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let outputs = runner.transform(&request).unwrap();
        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].path.is_file());
    }

    #[test]
    fn waveform_bars_are_bounded() {
        let samples: Vec<u8> = (0..WAVEFORM_RATE)
            .flat_map(|i| ((i % 1000) as i16).to_le_bytes())
            .collect();
        let bars = waveform_bars(&samples).unwrap();
        assert_eq!(bars.len(), WAVEFORM_BARS);
        assert!(bars.iter().all(|bar| *bar <= WAVEFORM_MAX));
    }
}
