use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// 与 vision 域共享同一供应商错误形态：两个编排层面对的是同类模型供应商，
/// 没有必要各写一份枚举再互相映射。
pub use wxlike_vision::ProviderError;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Translation {
    pub text: String,
    pub source_language: String,
    pub target_language: String,
    pub confidence: f32,
    pub provider: String,
}

#[async_trait]
pub trait TranslationProvider: Send + Sync {
    /// `source` 为 `None` 时由供应商自动检测，并在结果里返回检测语言。
    async fn translate(
        &self,
        text: &str,
        source: Option<&str>,
        target: &str,
    ) -> std::result::Result<Translation, ProviderError>;
}
