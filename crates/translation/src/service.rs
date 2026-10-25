use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::Semaphore;

use wxlike_vision::CancelToken;

use crate::error::{Result, TranslationError, TranslationErrorCode as Code};
use crate::ports::{ProviderError, Translation, TranslationProvider};

const MAX_ACTOR_LEN: usize = 128;
const MAX_TEXT_BYTES: usize = 10_000;
const MAX_OUTPUT_BYTES: usize = 40_000;
const MAX_PROVIDER_ID: usize = 64;
const RATE_WINDOW_MS: i64 = 60_000;
const MAX_CONFIDENCE: f32 = 1.0;
const MIN_CONFIDENCE: f32 = 0.0;

#[derive(Clone, Debug)]
pub struct TranslationConfig {
    pub max_text_bytes: usize,
    pub max_concurrency: usize,
    pub timeout_ms: u64,
    pub consent_ttl_ms: i64,
    pub requests_per_minute: u32,
}

impl Default for TranslationConfig {
    fn default() -> Self {
        Self {
            max_text_bytes: MAX_TEXT_BYTES,
            max_concurrency: 4,
            timeout_ms: 10_000,
            consent_ttl_ms: 10 * 60 * 1000,
            requests_per_minute: 60,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TranslationRequest {
    pub text: String,
    /// `None` 表示自动检测源语言。
    pub source: Option<String>,
    pub target: String,
}

#[derive(Default)]
struct ActorState {
    consent_until_ms: i64,
    recent: Vec<i64>,
}

impl ActorState {
    fn admit(&mut self, now_ms: i64, limit: u32) -> Result<()> {
        if self.consent_until_ms <= now_ms {
            return Err(TranslationError::new(
                Code::ConsentRequired,
                "upload consent required",
            ));
        }
        self.recent.retain(|ts| now_ms - ts < RATE_WINDOW_MS);
        if self.recent.len() >= limit as usize {
            return Err(TranslationError::new(
                Code::RateLimited,
                "translation rate limit",
            ));
        }
        self.recent.push(now_ms);
        Ok(())
    }
}

pub struct TranslationService<P: TranslationProvider> {
    provider: Option<P>,
    config: TranslationConfig,
    permits: Semaphore,
    actors: std::sync::Mutex<HashMap<String, ActorState>>,
}

impl<P: TranslationProvider> TranslationService<P> {
    pub fn new(config: TranslationConfig, provider: Option<P>) -> Self {
        let permits = Semaphore::new(config.max_concurrency.max(1));
        Self {
            provider,
            config,
            permits,
            actors: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// 用户显式授权后才允许把选中文本送供应商；到期自动失效。
    pub fn grant_consent(&self, actor: &str, now_ms: i64, ttl_ms: Option<i64>) -> Result<i64> {
        validate_actor(actor)?;
        let ttl_ms = ttl_ms.unwrap_or(self.config.consent_ttl_ms).max(1);
        let expires_at_ms = now_ms + ttl_ms;
        let mut actors = self.actors.lock().map_err(|_| internal())?;
        actors.entry(actor.to_owned()).or_default().consent_until_ms = expires_at_ms;
        Ok(expires_at_ms)
    }

    pub fn revoke_consent(&self, actor: &str) {
        if let Ok(mut actors) = self.actors.lock() {
            actors.remove(actor);
        }
    }

    /// 删除该用户的授权与限流痕迹；译文从不持久化。
    pub fn delete_actor_data(&self, actor: &str) -> bool {
        self.actors
            .lock()
            .map(|mut actors| actors.remove(actor).is_some())
            .unwrap_or(false)
    }

    pub async fn run(
        &self,
        actor: &str,
        request: TranslationRequest,
        cancel: &CancelToken,
        now_ms: i64,
    ) -> Result<Translation> {
        validate_actor(actor)?;
        self.validate_request(&request)?;
        self.admit(actor, now_ms)?;
        let _permit = self.permits.try_acquire().map_err(|_| {
            TranslationError::new(Code::RateLimited, "translator concurrency exhausted")
        })?;
        let provider = self.provider.as_ref().ok_or_else(|| {
            TranslationError::new(Code::DependencyUnavailable, "no translation provider")
        })?;
        let source = request.source.as_deref();
        let timeout = Duration::from_millis(self.config.timeout_ms.max(1));
        let call = provider.translate(&request.text, source, &request.target);
        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(TranslationError::new(Code::Cancelled, "cancelled by user"));
            }
            result = tokio::time::timeout(timeout, call) => result,
        };
        let translation = match outcome {
            Err(_) => return Err(TranslationError::new(Code::Timeout, "translator timed out")),
            Ok(Err(err)) => return Err(map_provider_error(err)),
            Ok(Ok(value)) => value,
        };
        validate_translation(&translation, &request.target)?;
        Ok(translation)
    }

    fn admit(&self, actor: &str, now_ms: i64) -> Result<()> {
        let mut actors = self.actors.lock().map_err(|_| internal())?;
        let state = actors.get_mut(actor).ok_or_else(|| {
            TranslationError::new(Code::ConsentRequired, "upload consent required")
        })?;
        state.admit(now_ms, self.config.requests_per_minute)
    }

    fn validate_request(&self, request: &TranslationRequest) -> Result<()> {
        if request.text.is_empty() {
            return Err(TranslationError::new(Code::InvalidArgument, "empty text"));
        }
        if request.text.len() > self.config.max_text_bytes {
            return Err(TranslationError::new(
                Code::InvalidArgument,
                "text too long",
            ));
        }
        validate_language(&request.target)?;
        if let Some(source) = &request.source {
            validate_language(source)?;
            if source.eq_ignore_ascii_case(&request.target) {
                return Err(TranslationError::new(
                    Code::InvalidArgument,
                    "source and target must differ",
                ));
            }
        }
        Ok(())
    }
}

fn validate_actor(actor: &str) -> Result<()> {
    if actor.is_empty() || actor.len() > MAX_ACTOR_LEN {
        return Err(TranslationError::new(Code::Unauthorized, "invalid actor"));
    }
    Ok(())
}

fn validate_language(tag: &str) -> Result<()> {
    if tag.len() < 2
        || tag.len() > MAX_PROVIDER_ID
        || !tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(TranslationError::new(Code::InvalidArgument, "language tag"));
    }
    Ok(())
}

fn validate_translation(value: &Translation, target: &str) -> Result<()> {
    if value.provider.len() > MAX_PROVIDER_ID {
        return Err(TranslationError::new(
            Code::Internal,
            "invalid translation result",
        ));
    }
    validate_language(&value.source_language).map_err(|_| invalid_result())?;
    validate_language(&value.target_language).map_err(|_| invalid_result())?;
    if !value.target_language.eq_ignore_ascii_case(target) {
        return Err(TranslationError::new(
            Code::Internal,
            "unexpected target language",
        ));
    }
    if !value.confidence.is_finite()
        || !(MIN_CONFIDENCE..=MAX_CONFIDENCE).contains(&value.confidence)
    {
        return Err(invalid_result());
    }
    if value.text.len() > MAX_OUTPUT_BYTES {
        return Err(invalid_result());
    }
    Ok(())
}

fn invalid_result() -> TranslationError {
    TranslationError::new(Code::Internal, "invalid translation result")
}

fn internal() -> TranslationError {
    TranslationError::new(Code::Internal, "internal state")
}

fn map_provider_error(error: ProviderError) -> TranslationError {
    match error {
        ProviderError::Unavailable | ProviderError::BadGateway => TranslationError::new(
            Code::DependencyUnavailable,
            "translation provider unavailable",
        ),
        ProviderError::Timeout => TranslationError::new(Code::Timeout, "translator timed out"),
        ProviderError::Cancelled => {
            TranslationError::new(Code::Cancelled, "translation provider cancelled")
        }
    }
}
