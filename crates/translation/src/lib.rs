//! `wxlike-translation` — G07 的文本翻译编排层。
//!
//! 引擎通过 [`TranslationProvider`] 注入（本地模型或远端服务）。翻译结果只
//! 在内存中返回，不落日志、不持久化用户原文；供应商不可用返回
//! `DEPENDENCY_UNAVAILABLE`，绝不返回样例译文。

pub mod error;
pub mod ports;
pub mod service;

pub use error::{TranslationError, TranslationErrorCode};
pub use ports::{ProviderError, Translation, TranslationProvider};
pub use service::{TranslationConfig, TranslationRequest, TranslationService};
pub use wxlike_vision::CancelToken;
