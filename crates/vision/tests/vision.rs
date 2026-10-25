use std::time::Duration;

use async_trait::async_trait;
use wxlike_vision::error::VisionErrorCode as Code;
use wxlike_vision::ports::{
    AudioInput, IdentifyResult, ImageInput, Label, OcrBlock, OcrResult, ProviderError, Transcript,
    VisionProvider,
};
use wxlike_vision::service::{audit_summary, VisionInput};
use wxlike_vision::{
    CancelToken, JobKind, VisionConfig, VisionOutput, VisionRequest, VisionService,
};

struct Stub {
    delay_ms: u64,
    error: Option<ProviderError>,
    confidence: f32,
    language: String,
}

impl Stub {
    fn ok() -> Self {
        Self {
            delay_ms: 0,
            error: None,
            confidence: 0.93,
            language: "zh-CN".to_owned(),
        }
    }
}

#[async_trait]
impl VisionProvider for Stub {
    async fn identify(
        &self,
        image: &ImageInput,
    ) -> std::result::Result<IdentifyResult, ProviderError> {
        sleep(self.delay_ms).await;
        if let Some(err) = self.error {
            return Err(err);
        }
        Ok(IdentifyResult {
            labels: vec![Label {
                name: format!("cat-{}", image.bytes.len()),
                confidence: self.confidence,
            }],
            provider: "stub".to_owned(),
        })
    }

    async fn ocr(
        &self,
        image: &ImageInput,
        languages: &[String],
    ) -> std::result::Result<OcrResult, ProviderError> {
        sleep(self.delay_ms).await;
        if let Some(err) = self.error {
            return Err(err);
        }
        let text = String::from_utf8_lossy(&image.bytes).to_string();
        Ok(OcrResult {
            blocks: vec![OcrBlock {
                text: text.clone(),
                confidence: self.confidence,
            }],
            text,
            language: languages
                .first()
                .cloned()
                .unwrap_or_else(|| self.language.clone()),
            confidence: self.confidence,
            provider: "stub".to_owned(),
        })
    }

    async fn transcribe(
        &self,
        audio: &AudioInput,
    ) -> std::result::Result<Transcript, ProviderError> {
        sleep(self.delay_ms).await;
        if let Some(err) = self.error {
            return Err(err);
        }
        Ok(Transcript {
            text: format!("transcript:{}", audio.bytes.len()),
            language: Some(self.language.clone()),
            confidence: self.confidence,
            provider: "stub".to_owned(),
        })
    }
}

async fn sleep(ms: u64) {
    if ms > 0 {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }
}

fn image(bytes: &[u8]) -> VisionInput {
    VisionInput::Image(ImageInput::new(bytes.to_vec(), "image/png"))
}

fn ocr_request(bytes: &[u8], languages: &[&str]) -> VisionRequest {
    VisionRequest {
        kind: JobKind::Ocr,
        input: image(bytes),
        languages: languages.iter().map(|v| (*v).to_owned()).collect(),
    }
}

fn service(stub: Stub) -> VisionService<Stub> {
    VisionService::new(VisionConfig::default(), Some(stub))
}

#[tokio::test]
async fn ocr_success_returns_provider_text_and_language() {
    let svc = service(Stub::ok());
    svc.grant_consent("u1", 1_000, None).unwrap();
    let result = svc
        .run(
            "u1",
            ocr_request("你好".as_bytes(), &["zh-CN"]),
            &CancelToken::new(),
            2_000,
        )
        .await
        .unwrap();
    assert_eq!(result.kind, JobKind::Ocr);
    let VisionOutput::Ocr(ocr) = result.output else {
        panic!("expected ocr output");
    };
    assert_eq!(ocr.text, "你好");
    assert_eq!(ocr.language, "zh-CN");
    assert!(ocr.confidence > 0.9);
    assert!(ocr.blocks[0].confidence <= 1.0);
    assert!(result.job_id.starts_with("vjob_"));
}

#[tokio::test]
async fn identify_and_transcribe_use_matching_provider_calls() {
    let svc = service(Stub::ok());
    svc.grant_consent("u1", 0, None).unwrap();
    let identify = svc
        .run(
            "u1",
            VisionRequest {
                kind: JobKind::Identify,
                input: image(&[1, 2, 3]),
                languages: vec![],
            },
            &CancelToken::new(),
            0,
        )
        .await
        .unwrap();
    let VisionOutput::Identify(result) = identify.output else {
        panic!("expected identify");
    };
    assert_eq!(result.labels[0].name, "cat-3");

    let audio = svc
        .run(
            "u1",
            VisionRequest {
                kind: JobKind::Transcribe,
                input: VisionInput::Audio(AudioInput {
                    bytes: vec![0u8; 8],
                    mime: "audio/wav".to_owned(),
                }),
                languages: vec![],
            },
            &CancelToken::new(),
            0,
        )
        .await
        .unwrap();
    let VisionOutput::Transcribe(t) = audio.output else {
        panic!("expected transcript");
    };
    assert_eq!(t.text, "transcript:8");
}

#[tokio::test]
async fn upload_without_consent_is_rejected() {
    let svc = service(Stub::ok());
    let err = svc
        .run("u1", ocr_request(b"x", &["zh"]), &CancelToken::new(), 0)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::ConsentRequired);
}

#[tokio::test]
async fn consent_expiry_and_revocation_block_upload() {
    let svc = service(Stub::ok());
    svc.grant_consent("u1", 1_000, Some(500)).unwrap();
    let err = svc
        .run("u1", ocr_request(b"x", &["zh"]), &CancelToken::new(), 1_600)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::ConsentRequired);

    svc.grant_consent("u1", 1_600, None).unwrap();
    svc.revoke_consent("u1");
    let err = svc
        .run("u1", ocr_request(b"x", &["zh"]), &CancelToken::new(), 1_700)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::ConsentRequired);
}

#[tokio::test]
async fn empty_or_oversized_payload_and_bad_language_are_invalid() {
    let svc = service(Stub::ok());
    svc.grant_consent("u1", 0, None).unwrap();

    let empty = svc
        .run("u1", ocr_request(b"", &["zh"]), &CancelToken::new(), 0)
        .await
        .unwrap_err();
    assert_eq!(empty.code(), Code::InvalidArgument);

    let config = VisionConfig {
        max_image_bytes: 4,
        ..Default::default()
    };
    let small = VisionService::new(config, Some(Stub::ok()));
    small.grant_consent("u1", 0, None).unwrap();
    let too_big = small
        .run("u1", ocr_request(b"12345", &["zh"]), &CancelToken::new(), 0)
        .await
        .unwrap_err();
    assert_eq!(too_big.code(), Code::InvalidArgument);

    let bad_lang = svc
        .run("u1", ocr_request(b"x", &["zh CN!"]), &CancelToken::new(), 0)
        .await
        .unwrap_err();
    assert_eq!(bad_lang.code(), Code::InvalidArgument);

    let wrong_kind = svc
        .run(
            "u1",
            VisionRequest {
                kind: JobKind::Transcribe,
                input: image(b"x"),
                languages: vec![],
            },
            &CancelToken::new(),
            0,
        )
        .await
        .unwrap_err();
    assert_eq!(wrong_kind.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn invalid_actor_is_unauthorized() {
    let svc = service(Stub::ok());
    let err = svc
        .run("", ocr_request(b"x", &["zh"]), &CancelToken::new(), 0)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthorized);
}

#[tokio::test]
async fn provider_unavailable_maps_to_dependency_unavailable() {
    let svc = VisionService::<Stub>::new(VisionConfig::default(), None);
    svc.grant_consent("u1", 0, None).unwrap();
    let err = svc
        .run("u1", ocr_request(b"x", &["zh"]), &CancelToken::new(), 0)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::DependencyUnavailable);

    let failing = service(Stub {
        error: Some(ProviderError::BadGateway),
        ..Stub::ok()
    });
    failing.grant_consent("u1", 0, None).unwrap();
    let err = failing
        .run("u1", ocr_request(b"x", &["zh"]), &CancelToken::new(), 0)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::DependencyUnavailable);
}

#[tokio::test]
async fn invalid_provider_confidence_is_internal_error_not_a_sample_result() {
    let svc = service(Stub {
        confidence: 7.5,
        ..Stub::ok()
    });
    svc.grant_consent("u1", 0, None).unwrap();
    let err = svc
        .run("u1", ocr_request(b"x", &["zh"]), &CancelToken::new(), 0)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Internal);
}

#[tokio::test]
async fn provider_timeout_releases_and_reports_timeout() {
    let config = VisionConfig {
        timeout_ms: 20,
        ..Default::default()
    };
    let svc = VisionService::new(
        config,
        Some(Stub {
            delay_ms: 500,
            ..Stub::ok()
        }),
    );
    svc.grant_consent("u1", 0, None).unwrap();
    let err = svc
        .run("u1", ocr_request(b"x", &["zh"]), &CancelToken::new(), 0)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Timeout);
}

#[tokio::test]
async fn cancelled_run_returns_cancelled_and_releases_permit() {
    let config = VisionConfig {
        max_concurrency: 1,
        requests_per_minute: 100,
        ..Default::default()
    };
    let svc = VisionService::new(
        config,
        Some(Stub {
            delay_ms: 5_000,
            ..Stub::ok()
        }),
    );
    svc.grant_consent("u1", 0, None).unwrap();
    let token = CancelToken::new();
    let clone = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        clone.cancel();
    });
    let err = svc
        .run("u1", ocr_request(b"x", &["zh"]), &token, 0)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Cancelled);
}

#[tokio::test]
async fn concurrency_limit_is_enforced_and_recovers() {
    let config = VisionConfig {
        max_concurrency: 1,
        timeout_ms: 5_000,
        ..Default::default()
    };
    let svc = std::sync::Arc::new(VisionService::new(
        config,
        Some(Stub {
            delay_ms: 50,
            ..Stub::ok()
        }),
    ));
    svc.grant_consent("u1", 0, None).unwrap();

    let first = {
        let svc = svc.clone();
        tokio::spawn(async move {
            svc.run("u1", ocr_request(b"a", &["zh"]), &CancelToken::new(), 0)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    let second = svc
        .run("u1", ocr_request(b"b", &["zh"]), &CancelToken::new(), 0)
        .await
        .unwrap_err();
    assert_eq!(second.code(), Code::RateLimited);
    assert!(first.await.unwrap().is_ok());

    let third = svc
        .run("u1", ocr_request(b"c", &["zh"]), &CancelToken::new(), 0)
        .await;
    assert!(third.is_ok());
}

#[tokio::test]
async fn rate_limit_counts_requests_per_actor() {
    let config = VisionConfig {
        requests_per_minute: 2,
        ..Default::default()
    };
    let svc = VisionService::new(config, Some(Stub::ok()));
    svc.grant_consent("u1", 0, None).unwrap();
    svc.grant_consent("u2", 0, None).unwrap();
    for _ in 0..2 {
        svc.run("u1", ocr_request(b"x", &["zh"]), &CancelToken::new(), 100)
            .await
            .unwrap();
    }
    let err = svc
        .run("u1", ocr_request(b"x", &["zh"]), &CancelToken::new(), 100)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::RateLimited);
    assert!(svc
        .run("u2", ocr_request(b"x", &["zh"]), &CancelToken::new(), 100)
        .await
        .is_ok());
    assert!(svc
        .run(
            "u1",
            ocr_request(b"x", &["zh"]),
            &CancelToken::new(),
            60_100
        )
        .await
        .is_ok());
}

#[tokio::test]
async fn jobs_are_owner_scoped_expire_and_can_be_deleted() {
    let config = VisionConfig {
        result_ttl_ms: 1_000,
        ..Default::default()
    };
    let svc = VisionService::new(config, Some(Stub::ok()));
    svc.grant_consent("u1", 0, None).unwrap();
    let result = svc
        .run("u1", ocr_request(b"x", &["zh"]), &CancelToken::new(), 0)
        .await
        .unwrap();

    let view = svc.job("u1", &result.job_id, 500).unwrap();
    assert_eq!(view.expires_at_ms, 1_000);
    let denied = svc.job("u2", &result.job_id, 500).unwrap_err();
    assert_eq!(denied.code(), Code::NotFound);
    let expired = svc.job("u1", &result.job_id, 1_000).unwrap_err();
    assert_eq!(expired.code(), Code::NotFound);

    let second = svc
        .run("u1", ocr_request(b"y", &["zh"]), &CancelToken::new(), 0)
        .await
        .unwrap();
    assert_eq!(svc.delete_actor_data("u1"), 2);
    assert_eq!(
        svc.job("u1", &second.job_id, 0).unwrap_err().code(),
        Code::NotFound
    );
}

#[tokio::test]
async fn purge_expired_removes_only_stale_jobs() {
    let config = VisionConfig {
        requests_per_minute: 10,
        ..Default::default()
    };
    let svc = VisionService::new(config, Some(Stub::ok()));
    svc.grant_consent("u1", 0, None).unwrap();
    let stale = svc
        .run("u1", ocr_request(b"a", &["zh"]), &CancelToken::new(), 0)
        .await
        .unwrap();
    assert_eq!(svc.purge_expired(60 * 60 * 1000), 1);
    assert_eq!(
        svc.job("u1", &stale.job_id, 0).unwrap_err().code(),
        Code::NotFound
    );
}

#[test]
fn audit_summary_has_no_user_content() {
    let summary = audit_summary("u-secret", JobKind::Ocr, 42);
    assert_eq!(summary, "vision actor=u-secret kind=ocr input_bytes=42");
    assert!(!summary.contains("你好"));
    assert!(!summary.contains("base64"));
}
