//! `wxlike-vision` — G07 的识物、OCR 与语音转写编排层。
//!
//! 推理引擎不写死：本地 Candle/whisper 或远端供应商都通过 [`VisionProvider`]
//! 注入。没有配置 provider 时返回 `DEPENDENCY_UNAVAILABLE`，**绝不返回随机
//! 样例结果**。用户图片/音频不进入日志，结果有保留期与删除入口。

pub mod error;
pub mod ports;
pub mod service;

pub use error::{VisionError, VisionErrorCode};
pub use ports::{
    AudioInput, IdentifyResult, ImageInput, OcrBlock, OcrResult, ProviderError, Transcript,
    VisionProvider,
};
pub use service::{
    CancelToken, JobKind, JobResult, JobView, VisionConfig, VisionOutput, VisionRequest,
    VisionService,
};
