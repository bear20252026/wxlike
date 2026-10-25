use async_trait::async_trait;

use super::scanner::{QrScanner, ScanResult};
use crate::crypto::NonceStore;
use crate::error::Result;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraSession {
    pub id: String,
    pub width: u32,
    pub height: u32,
}

/// 相机、闪光灯与帧采集由平台桥实现；本域只编排"开灯→取帧→关灯"。
#[async_trait]
pub trait CameraPort: Send + Sync {
    async fn start(&self) -> Result<CameraSession>;
    async fn set_flashlight(&self, enabled: bool) -> Result<()>;
    async fn capture_frame(&self) -> Result<Vec<u8>>;
    async fn stop(&self) -> Result<()>;
}

impl<N: NonceStore> QrScanner<N> {
    pub async fn scan_camera<P: CameraPort>(
        &self,
        camera: &P,
        actor: &str,
        flashlight: bool,
        now_ms: i64,
    ) -> Result<ScanResult> {
        let outcome = async {
            camera.start().await?;
            camera.set_flashlight(flashlight).await?;
            camera.capture_frame().await
        }
        .await;
        let stopped = camera.stop().await;
        let frame = outcome?;
        stopped?;
        self.scan_image(actor, &frame, now_ms)
    }
}
