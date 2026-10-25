use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageInput {
    pub bytes: Vec<u8>,
    pub mime: String,
}

impl ImageInput {
    pub fn new(bytes: Vec<u8>, mime: impl Into<String>) -> Self {
        Self {
            bytes,
            mime: mime.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioInput {
    pub bytes: Vec<u8>,
    pub mime: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    pub confidence: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IdentifyResult {
    pub labels: Vec<Label>,
    pub provider: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OcrBlock {
    pub text: String,
    pub confidence: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OcrResult {
    pub text: String,
    pub language: String,
    pub confidence: f32,
    pub blocks: Vec<OcrBlock>,
    pub provider: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub text: String,
    pub language: Option<String>,
    pub confidence: f32,
    pub provider: String,
}

/// 供应商侧失败原因。只到这个粒度，原始报文不透传给调用方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderError {
    /// 供应商未部署、网络不可达或尚未配置凭据。
    Unavailable,
    /// 供应商在约定时间内未返回结果。
    Timeout,
    /// 供应商主动中止任务；与本域取消令牌触发的取消区分开。
    Cancelled,
    /// 供应商返回了无法解析的网关错误。
    BadGateway,
}

#[async_trait]
pub trait VisionProvider: Send + Sync {
    async fn identify(
        &self,
        image: &ImageInput,
    ) -> std::result::Result<IdentifyResult, ProviderError>;

    async fn ocr(
        &self,
        image: &ImageInput,
        languages: &[String],
    ) -> std::result::Result<OcrResult, ProviderError>;

    async fn transcribe(
        &self,
        audio: &AudioInput,
    ) -> std::result::Result<Transcript, ProviderError>;
}
