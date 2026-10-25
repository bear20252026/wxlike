wxlike_vision::domain_error!(
    TranslationError,
    TranslationErrorCode,
    {
        ConsentRequired => "CONSENT_REQUIRED",
        InvalidArgument => "INVALID_ARGUMENT",
        RateLimited => "RATE_LIMITED",
        Unauthorized => "UNAUTHORIZED",
        Timeout => "TIMEOUT",
        Cancelled => "CANCELLED",
        DependencyUnavailable => "DEPENDENCY_UNAVAILABLE",
        Internal => "INTERNAL",
    }
);
