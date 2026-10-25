use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::RngCore;
use tokio::sync::{Notify, Semaphore};

use crate::error::{Result, VisionError, VisionErrorCode as Code};
use crate::ports::{
    AudioInput, IdentifyResult, ImageInput, OcrResult, ProviderError, Transcript, VisionProvider,
};

const MAX_LANGUAGES: usize = 5;
const MAX_RESULT_TEXT: usize = 200_000;
const MAX_LABELS: usize = 100;
const MAX_ACTOR_LEN: usize = 128;
const MAX_PROVIDER_ID: usize = 64;
const MAX_CONFIDENCE: f32 = 1.0;
const MIN_CONFIDENCE: f32 = 0.0;
const DEFAULT_JOB_BYTES: usize = 16;
const MAX_MEDIA_BYTES: usize = 20 * 1024 * 1024;
const DEFAULT_REQUESTS_PER_MINUTE: u32 = 30;

#[derive(Clone, Debug)]
pub struct VisionConfig {
    pub max_image_bytes: usize,
    pub max_audio_bytes: usize,
    pub max_concurrency: usize,
    pub timeout_ms: u64,
    pub consent_ttl_ms: i64,
    pub result_ttl_ms: i64,
    pub requests_per_minute: u32,
}

impl Default for VisionConfig {
    fn default() -> Self {
        Self {
            max_image_bytes: MAX_MEDIA_BYTES,
            max_audio_bytes: MAX_MEDIA_BYTES,
            max_concurrency: 4,
            timeout_ms: 15_000,
            consent_ttl_ms: 10 * 60 * 1000,
            result_ttl_ms: 60 * 60 * 1000,
            requests_per_minute: DEFAULT_REQUESTS_PER_MINUTE,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Identify,
    Ocr,
    Transcribe,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identify => "identify",
            Self::Ocr => "ocr",
            Self::Transcribe => "transcribe",
        }
    }
}

#[derive(Clone, Debug)]
pub enum VisionInput {
    Image(ImageInput),
    Audio(AudioInput),
}

impl VisionInput {
    /// 仅返回字节数，供审计摘要使用；不暴露内容。
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Image(image) => image.bytes.len(),
            Self::Audio(audio) => audio.bytes.len(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct VisionRequest {
    pub kind: JobKind,
    pub input: VisionInput,
    pub languages: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VisionOutput {
    Identify(IdentifyResult),
    Ocr(OcrResult),
    Transcribe(Transcript),
}

#[derive(Clone, Debug, PartialEq)]
pub struct JobResult {
    pub job_id: String,
    pub kind: JobKind,
    pub output: VisionOutput,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JobView {
    pub job_id: String,
    pub kind: JobKind,
    pub output: VisionOutput,
    pub expires_at_ms: i64,
}

struct JobRecord {
    actor: String,
    kind: JobKind,
    output: VisionOutput,
    expires_at_ms: i64,
}

#[derive(Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// 等待取消信号；可被多个执行点同时 `select!`。
    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

pub struct VisionService<P: VisionProvider> {
    provider: Option<P>,
    config: VisionConfig,
    permits: Semaphore,
    consents: Mutex<HashMap<String, i64>>,
    jobs: Mutex<HashMap<String, JobRecord>>,
    request_log: Mutex<HashMap<String, VecDeque<i64>>>,
}

impl<P: VisionProvider> VisionService<P> {
    pub fn new(config: VisionConfig, provider: Option<P>) -> Self {
        let permits = Semaphore::new(config.max_concurrency.max(1));
        Self {
            provider,
            config,
            permits,
            consents: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            request_log: Mutex::new(HashMap::new()),
        }
    }

    /// 上传前必须由用户显式授权；到期自动失效。
    pub fn grant_consent(&self, actor: &str, now_ms: i64, ttl_ms: Option<i64>) -> Result<i64> {
        validate_actor(actor)?;
        let ttl_ms = ttl_ms.unwrap_or(self.config.consent_ttl_ms).max(1);
        let expires_at_ms = now_ms + ttl_ms;
        self.consents
            .lock()
            .map_err(|_| internal())?
            .insert(actor.to_owned(), expires_at_ms);
        Ok(expires_at_ms)
    }

    pub fn revoke_consent(&self, actor: &str) {
        if let Ok(mut consents) = self.consents.lock() {
            consents.remove(actor);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &self,
        actor: &str,
        request: VisionRequest,
        cancel: &CancelToken,
        now_ms: i64,
    ) -> Result<JobResult> {
        validate_actor(actor)?;
        self.validate_request(&request)?;
        self.require_consent(actor, now_ms)?;
        self.check_rate_limit(actor, now_ms)?;
        let _permit = self
            .permits
            .try_acquire()
            .map_err(|_| VisionError::new(Code::RateLimited, "provider concurrency exhausted"))?;
        let provider = self
            .provider
            .as_ref()
            .ok_or_else(|| VisionError::new(Code::DependencyUnavailable, "no vision provider"))?;
        let output = self.execute(provider, &request, cancel).await?;
        self.store_job(actor, request.kind, output, now_ms)
    }

    pub fn job(&self, actor: &str, job_id: &str, now_ms: i64) -> Result<JobView> {
        let jobs = self.jobs.lock().map_err(|_| internal())?;
        let record = jobs
            .get(job_id)
            .ok_or_else(|| VisionError::new(Code::NotFound, "job not found"))?;
        if record.actor != actor || record.expires_at_ms <= now_ms {
            return Err(VisionError::new(Code::NotFound, "job not found"));
        }
        Ok(JobView {
            job_id: job_id.to_owned(),
            kind: record.kind,
            output: record.output.clone(),
            expires_at_ms: record.expires_at_ms,
        })
    }

    /// 保留期清理；返回删除条数。
    pub fn purge_expired(&self, now_ms: i64) -> usize {
        let Ok(mut jobs) = self.jobs.lock() else {
            return 0;
        };
        let before = jobs.len();
        jobs.retain(|_, record| record.expires_at_ms > now_ms);
        before - jobs.len()
    }

    /// 用户删除请求：清掉结果与授权，不保留输入内容（输入从未落盘/落日志）。
    pub fn delete_actor_data(&self, actor: &str) -> usize {
        let mut removed = 0;
        if let Ok(mut jobs) = self.jobs.lock() {
            let before = jobs.len();
            jobs.retain(|_, record| record.actor != actor);
            removed = before - jobs.len();
        }
        if let Ok(mut consents) = self.consents.lock() {
            consents.remove(actor);
        }
        removed
    }

    async fn execute(
        &self,
        provider: &P,
        request: &VisionRequest,
        cancel: &CancelToken,
    ) -> Result<VisionOutput> {
        let timeout = Duration::from_millis(self.config.timeout_ms.max(1));
        let call = self.call_provider(provider, request);
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(VisionError::new(Code::Cancelled, "cancelled by user")),
            outcome = tokio::time::timeout(timeout, call) => outcome,
        };
        let output = match result {
            Err(_) => {
                return Err(VisionError::new(Code::Timeout, "provider timed out"));
            }
            Ok(Err(err)) => return Err(map_provider_error(err)),
            Ok(Ok(output)) => output,
        };
        validate_output(&output)?;
        Ok(output)
    }

    async fn call_provider(
        &self,
        provider: &P,
        request: &VisionRequest,
    ) -> std::result::Result<VisionOutput, ProviderError> {
        match (&request.kind, &request.input) {
            (JobKind::Identify, VisionInput::Image(image)) => {
                provider.identify(image).await.map(VisionOutput::Identify)
            }
            (JobKind::Ocr, VisionInput::Image(image)) => provider
                .ocr(image, &request.languages)
                .await
                .map(VisionOutput::Ocr),
            (JobKind::Transcribe, VisionInput::Audio(audio)) => provider
                .transcribe(audio)
                .await
                .map(VisionOutput::Transcribe),
            _ => Err(ProviderError::Unavailable),
        }
    }

    fn validate_request(&self, request: &VisionRequest) -> Result<()> {
        match (&request.kind, &request.input) {
            (JobKind::Transcribe, VisionInput::Audio(audio)) => {
                validate_bytes(
                    &audio.bytes,
                    self.config.max_audio_bytes,
                    &audio.mime,
                    &["audio/wav", "audio/ogg"],
                )?;
            }
            (JobKind::Identify | JobKind::Ocr, VisionInput::Image(image)) => {
                validate_bytes(
                    &image.bytes,
                    self.config.max_image_bytes,
                    &image.mime,
                    &["image/png", "image/jpeg"],
                )?;
            }
            _ => {
                return Err(VisionError::new(
                    Code::InvalidArgument,
                    "input does not match job kind",
                ));
            }
        }
        if request.languages.len() > MAX_LANGUAGES {
            return Err(VisionError::new(
                Code::InvalidArgument,
                "too many languages",
            ));
        }
        for language in &request.languages {
            validate_language(language)?;
        }
        Ok(())
    }

    fn require_consent(&self, actor: &str, now_ms: i64) -> Result<()> {
        let consents = self.consents.lock().map_err(|_| internal())?;
        match consents.get(actor) {
            Some(expires_at_ms) if *expires_at_ms > now_ms => Ok(()),
            _ => Err(VisionError::new(
                Code::ConsentRequired,
                "upload consent required",
            )),
        }
    }

    fn check_rate_limit(&self, actor: &str, now_ms: i64) -> Result<()> {
        let mut log = self.request_log.lock().map_err(|_| internal())?;
        let entries = log.entry(actor.to_owned()).or_default();
        while entries.front().is_some_and(|ts| now_ms - ts >= 60_000) {
            entries.pop_front();
        }
        if entries.len() >= self.config.requests_per_minute as usize {
            return Err(VisionError::new(Code::RateLimited, "vision rate limit"));
        }
        entries.push_back(now_ms);
        Ok(())
    }

    fn store_job(
        &self,
        actor: &str,
        kind: JobKind,
        output: VisionOutput,
        now_ms: i64,
    ) -> Result<JobResult> {
        let job_id = new_job_id();
        let record = JobRecord {
            actor: actor.to_owned(),
            kind,
            output: output.clone(),
            expires_at_ms: now_ms + self.config.result_ttl_ms.max(1),
        };
        self.jobs
            .lock()
            .map_err(|_| internal())?
            .insert(job_id.clone(), record);
        Ok(JobResult {
            job_id,
            kind,
            output,
        })
    }
}

fn validate_actor(actor: &str) -> Result<()> {
    if actor.is_empty() || actor.len() > MAX_ACTOR_LEN {
        return Err(VisionError::new(Code::Unauthorized, "invalid actor"));
    }
    Ok(())
}

fn validate_bytes(bytes: &[u8], max_bytes: usize, mime: &str, allowed: &[&str]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > max_bytes {
        return Err(VisionError::new(Code::InvalidArgument, "payload size"));
    }
    if !allowed.contains(&mime) {
        return Err(VisionError::new(Code::InvalidArgument, "mime type"));
    }
    Ok(())
}

fn validate_language(language: &str) -> Result<()> {
    if language.len() < 2
        || language.len() > MAX_PROVIDER_ID
        || !language
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(VisionError::new(Code::InvalidArgument, "language tag"));
    }
    Ok(())
}

pub fn validate_confidence(value: f32) -> Result<()> {
    if !value.is_finite() || !(MIN_CONFIDENCE..=MAX_CONFIDENCE).contains(&value) {
        return Err(VisionError::new(
            Code::Internal,
            "invalid provider confidence",
        ));
    }
    Ok(())
}

fn validate_output(output: &VisionOutput) -> Result<()> {
    match output {
        VisionOutput::Identify(result) => validate_identify(result),
        VisionOutput::Ocr(result) => validate_ocr(result),
        VisionOutput::Transcribe(result) => validate_transcribe(result),
    }
}

fn validate_identify(result: &IdentifyResult) -> Result<()> {
    if result.labels.len() > MAX_LABELS || result.provider.len() > MAX_PROVIDER_ID {
        return Err(VisionError::new(Code::Internal, "invalid identify result"));
    }
    for label in &result.labels {
        if label.name.is_empty() || label.name.len() > MAX_PROVIDER_ID {
            return Err(VisionError::new(Code::Internal, "invalid label"));
        }
        validate_confidence(label.confidence)?;
    }
    Ok(())
}

fn validate_ocr(result: &OcrResult) -> Result<()> {
    if result.text.len() > MAX_RESULT_TEXT
        || result.language.is_empty()
        || result.provider.len() > MAX_PROVIDER_ID
    {
        return Err(VisionError::new(Code::Internal, "invalid ocr result"));
    }
    validate_language(&result.language)?;
    validate_confidence(result.confidence)?;
    for block in &result.blocks {
        validate_confidence(block.confidence)?;
    }
    Ok(())
}

fn validate_transcribe(result: &Transcript) -> Result<()> {
    if result.text.len() > MAX_RESULT_TEXT || result.provider.len() > MAX_PROVIDER_ID {
        return Err(VisionError::new(Code::Internal, "invalid transcript"));
    }
    if let Some(language) = &result.language {
        validate_language(language)?;
    }
    validate_confidence(result.confidence)?;
    Ok(())
}

fn map_provider_error(error: ProviderError) -> VisionError {
    match error {
        ProviderError::Unavailable | ProviderError::BadGateway => {
            VisionError::new(Code::DependencyUnavailable, "vision provider unavailable")
        }
        ProviderError::Timeout => VisionError::new(Code::Timeout, "vision provider timed out"),
        ProviderError::Cancelled => VisionError::new(Code::Cancelled, "vision provider cancelled"),
    }
}

fn internal() -> VisionError {
    VisionError::new(Code::Internal, "internal state")
}

fn new_job_id() -> String {
    let mut bytes = [0u8; DEFAULT_JOB_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("vjob_{}", hex::encode(bytes))
}

/// 只输出 actor/类型/字节数，供审计与排障；不含图片、文本或识别结果。
pub fn audit_summary(actor: &str, kind: JobKind, input_bytes: usize) -> String {
    format!(
        "vision actor={actor} kind={} input_bytes={input_bytes}",
        kind.as_str()
    )
}
