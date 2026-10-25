use std::time::Duration;

use async_trait::async_trait;
use wxlike_translation::error::TranslationErrorCode as Code;
use wxlike_translation::{
    CancelToken, ProviderError, Translation, TranslationConfig, TranslationProvider,
    TranslationRequest, TranslationService,
};

struct Stub {
    delay_ms: u64,
    error: Option<ProviderError>,
    confidence: f32,
    detected: String,
    empty_on_marker: bool,
}

impl Stub {
    fn ok() -> Self {
        Self {
            delay_ms: 0,
            error: None,
            confidence: 0.88,
            detected: "en".to_owned(),
            empty_on_marker: true,
        }
    }
}

#[async_trait]
impl TranslationProvider for Stub {
    async fn translate(
        &self,
        text: &str,
        source: Option<&str>,
        target: &str,
    ) -> std::result::Result<Translation, ProviderError> {
        if self.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        }
        if let Some(err) = self.error {
            return Err(err);
        }
        let text = if self.empty_on_marker && text == "empty" {
            String::new()
        } else {
            format!("{text}->{target}")
        };
        Ok(Translation {
            text,
            source_language: source.unwrap_or(&self.detected).to_owned(),
            target_language: target.to_owned(),
            confidence: self.confidence,
            provider: "stub".to_owned(),
        })
    }
}

fn request(text: &str, source: Option<&str>, target: &str) -> TranslationRequest {
    TranslationRequest {
        text: text.to_owned(),
        source: source.map(str::to_owned),
        target: target.to_owned(),
    }
}

fn service(stub: Stub) -> TranslationService<Stub> {
    let svc = TranslationService::new(TranslationConfig::default(), Some(stub));
    svc.grant_consent("u1", 0, None).unwrap();
    svc
}

#[tokio::test]
async fn translates_between_languages_with_detected_source() {
    let svc = service(Stub::ok());
    let out = svc
        .run(
            "u1",
            request("hello", None, "zh-CN"),
            &CancelToken::new(),
            10,
        )
        .await
        .unwrap();
    assert_eq!(out.text, "hello->zh-CN");
    assert_eq!(out.target_language, "zh-CN");
    assert_eq!(out.source_language, "en");
    assert!((out.confidence - 0.88).abs() < f32::EPSILON);

    let explicit = svc
        .run(
            "u1",
            request("bonjour", Some("fr"), "ja"),
            &CancelToken::new(),
            10,
        )
        .await
        .unwrap();
    assert_eq!(explicit.text, "bonjour->ja");
    assert_eq!(explicit.source_language, "fr");
}

#[tokio::test]
async fn empty_translation_result_is_returned_as_is_not_faked() {
    let svc = service(Stub::ok());
    let out = svc
        .run("u1", request("empty", None, "zh"), &CancelToken::new(), 10)
        .await
        .unwrap();
    assert!(out.text.is_empty());
}

#[tokio::test]
async fn invalid_requests_are_rejected_before_provider_call() {
    let svc = service(Stub::ok());
    let cases = [
        request("", None, "zh"),
        request("hello", Some("en"), "en"),
        request("hello", None, "z"),
        request("hello", Some("e n"), "zh"),
    ];
    for case in cases {
        let err = svc
            .run("u1", case, &CancelToken::new(), 10)
            .await
            .unwrap_err();
        assert_eq!(err.code(), Code::InvalidArgument);
    }

    let small = TranslationService::new(
        TranslationConfig {
            max_text_bytes: 4,
            ..Default::default()
        },
        Some(Stub::ok()),
    );
    small.grant_consent("u1", 0, None).unwrap();
    let err = small
        .run("u1", request("hello", None, "zh"), &CancelToken::new(), 10)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn unauthorized_actor_and_missing_consent_are_blocked() {
    let svc = service(Stub::ok());
    let err = svc
        .run("", request("hello", None, "zh"), &CancelToken::new(), 10)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Unauthorized);

    let no_consent = TranslationService::new(TranslationConfig::default(), Some(Stub::ok()));
    let err = no_consent
        .run("u1", request("hello", None, "zh"), &CancelToken::new(), 10)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::ConsentRequired);
}

#[tokio::test]
async fn consent_expires_revokes_and_user_deletion_drops_it() {
    let svc = TranslationService::new(TranslationConfig::default(), Some(Stub::ok()));
    svc.grant_consent("u1", 1_000, Some(100)).unwrap();
    let err = svc
        .run(
            "u1",
            request("hello", None, "zh"),
            &CancelToken::new(),
            1_100,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::ConsentRequired);

    svc.grant_consent("u1", 2_000, None).unwrap();
    svc.revoke_consent("u1");
    let err = svc
        .run(
            "u1",
            request("hello", None, "zh"),
            &CancelToken::new(),
            2_000,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::ConsentRequired);

    svc.grant_consent("u1", 3_000, None).unwrap();
    assert!(svc.delete_actor_data("u1"));
    let err = svc
        .run(
            "u1",
            request("hello", None, "zh"),
            &CancelToken::new(),
            3_000,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::ConsentRequired);
    assert!(!svc.delete_actor_data("u1"));
}

#[tokio::test]
async fn provider_failures_map_to_stable_codes() {
    let none = TranslationService::<Stub>::new(TranslationConfig::default(), None);
    none.grant_consent("u1", 0, None).unwrap();
    assert_eq!(
        none.run("u1", request("hello", None, "zh"), &CancelToken::new(), 10)
            .await
            .unwrap_err()
            .code(),
        Code::DependencyUnavailable
    );

    for (provider_error, expected) in [
        (ProviderError::Unavailable, Code::DependencyUnavailable),
        (ProviderError::BadGateway, Code::DependencyUnavailable),
        (ProviderError::Cancelled, Code::Cancelled),
        (ProviderError::Timeout, Code::Timeout),
    ] {
        let svc = service(Stub {
            error: Some(provider_error),
            ..Stub::ok()
        });
        let err = svc
            .run("u1", request("hello", None, "zh"), &CancelToken::new(), 10)
            .await
            .unwrap_err();
        assert_eq!(err.code(), expected);
    }
}

#[tokio::test]
async fn invalid_provider_output_is_internal_not_sample_text() {
    let svc = service(Stub {
        confidence: -1.0,
        ..Stub::ok()
    });
    let err = svc
        .run("u1", request("hello", None, "zh"), &CancelToken::new(), 10)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Internal);

    let bad_lang = service(Stub {
        detected: "en US!".to_owned(),
        ..Stub::ok()
    });
    let err = bad_lang
        .run("u1", request("hello", None, "zh"), &CancelToken::new(), 10)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::Internal);
}

#[tokio::test]
async fn timeout_and_cancel_release_the_provider_permit() {
    let slow = TranslationService::new(
        TranslationConfig {
            timeout_ms: 20,
            ..Default::default()
        },
        Some(Stub {
            delay_ms: 500,
            ..Stub::ok()
        }),
    );
    slow.grant_consent("u1", 0, None).unwrap();
    assert_eq!(
        slow.run("u1", request("a", None, "zh"), &CancelToken::new(), 10)
            .await
            .unwrap_err()
            .code(),
        Code::Timeout
    );

    let svc = TranslationService::new(
        TranslationConfig {
            max_concurrency: 1,
            requests_per_minute: 100,
            ..Default::default()
        },
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
    assert_eq!(
        svc.run("u1", request("a", None, "zh"), &token, 10)
            .await
            .unwrap_err()
            .code(),
        Code::Cancelled
    );
    assert!(svc.delete_actor_data("u1"));
}

#[tokio::test]
async fn rate_limit_per_actor_and_concurrency_limit() {
    let svc = TranslationService::new(
        TranslationConfig {
            requests_per_minute: 2,
            max_concurrency: 1,
            ..Default::default()
        },
        Some(Stub {
            delay_ms: 30,
            ..Stub::ok()
        }),
    );
    svc.grant_consent("u1", 0, None).unwrap();
    svc.grant_consent("u2", 0, None).unwrap();

    let first = {
        let svc = std::sync::Arc::new(svc);
        let handle = {
            let svc = svc.clone();
            tokio::spawn(async move {
                svc.run("u1", request("a", None, "zh"), &CancelToken::new(), 10)
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(5)).await;
        let busy = svc
            .run("u2", request("b", None, "zh"), &CancelToken::new(), 10)
            .await
            .unwrap_err();
        assert_eq!(busy.code(), Code::RateLimited);
        assert!(handle.await.unwrap().is_ok());
        svc
    };

    assert!(first
        .run("u1", request("c", None, "zh"), &CancelToken::new(), 10)
        .await
        .is_ok());
    let limited = first
        .run("u1", request("d", None, "zh"), &CancelToken::new(), 10)
        .await
        .unwrap_err();
    assert_eq!(limited.code(), Code::RateLimited);
    assert!(first
        .run("u1", request("e", None, "zh"), &CancelToken::new(), 60_010)
        .await
        .is_ok());
}
