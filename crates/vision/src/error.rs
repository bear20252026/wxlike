/// 域错误骨架：各域的 DomainError 都是"错误码枚举 + 固定字段结构体"同一形状，
/// 宏只生成骨架；变体与文案由各域声明，避免每个域手抄一遍同样的人生。
#[macro_export]
macro_rules! domain_error {
    ($name:ident, $code:ident, { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $code {
            $($variant,)+
        }

        impl $code {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }
        }

        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct $name {
            code: $code,
            detail: &'static str,
        }

        impl $name {
            pub fn new(code: $code, detail: &'static str) -> Self {
                Self { code, detail }
            }

            pub fn code(&self) -> $code {
                self.code
            }

            pub fn detail(&self) -> &'static str {
                self.detail
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}: {}", self.code.as_str(), self.detail)
            }
        }

        impl std::error::Error for $name {}

        pub type Result<T> = std::result::Result<T, $name>;
    };
}

crate::domain_error!(
    VisionError,
    VisionErrorCode,
    {
        ConsentRequired => "CONSENT_REQUIRED",
        NotFound => "NOT_FOUND",
        RateLimited => "RATE_LIMITED",
        InvalidArgument => "INVALID_ARGUMENT",
        Unauthorized => "UNAUTHORIZED",
        Timeout => "TIMEOUT",
        Cancelled => "CANCELLED",
        DependencyUnavailable => "DEPENDENCY_UNAVAILABLE",
        Internal => "INTERNAL",
    }
);
