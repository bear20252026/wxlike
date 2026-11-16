//! S4 / ADR-29：实时订阅由平台能力驱动（治根因 2）。
//!
//! `PlatformCaps` 此前只写不读（仅 Debug 用到）。这里验它**真正驱动装配**：
//! 有长连接能力才尝试建流，没有就明确拒绝、让宿主改走推送 + 拉取。

use std::sync::Arc;

use wxlike_client_core::ports::{PlatformCaps, Secret, SecretError, Secrets};
use wxlike_client_native::{FileLockPlatform, NativeConfig, NativeCore, NativeError};
use wxlike_types::SubscribeRequest;

/// 空密钥后端（订阅测试不关心密钥内容，只要装配能过）。
#[derive(Default)]
struct NullSecrets;

#[async_trait::async_trait]
impl Secrets for NullSecrets {
    async fn get(&self, _key: &str) -> Result<Option<Secret>, SecretError> {
        Ok(None)
    }
    async fn put(&self, _key: &str, _value: Secret) -> Result<(), SecretError> {
        Ok(())
    }
    async fn delete(&self, _key: &str) -> Result<(), SecretError> {
        Ok(())
    }
}

fn config(dir: &std::path::Path) -> NativeConfig {
    NativeConfig::new("http://127.0.0.1:1", dir)
}

/// 无长连接能力（minimal caps）→ 订阅明确拒绝，不静默挂起。
#[tokio::test]
async fn subscribe_without_websocket_capability_is_rejected() {
    let dir = tempfile::tempdir().expect("临时目录");
    let platform = Arc::new(
        FileLockPlatform::new(dir.path().join("client.lock")).with_caps(PlatformCaps::minimal()),
    );
    let core = NativeCore::open_with_platform(config(dir.path()), Arc::new(NullSecrets), platform)
        .await
        .expect("打开");
    let result = core.subscribe_events(SubscribeRequest::all()).await;
    match result {
        Err(NativeError::RealtimeUnavailable(why)) => {
            assert!(
                why.contains("caps.websocket=false"),
                "应是能力拒绝路径：{why}"
            );
        }
        Err(other) => panic!("预期能力拒绝，实际：{other}"),
        Ok(_) => panic!("无长连接能力不该订阅成功"),
    }
}

/// 有长连接能力（native caps）→ 订阅尝试建流（而非能力拒绝）。
/// 网络不可达 → 建流失败映射成 RealtimeUnavailable(原因)，证明走的是
/// "尝试订阅"路径而不是"能力拒绝"路径。
#[tokio::test]
async fn subscribe_with_websocket_capability_attempts_stream() {
    let dir = tempfile::tempdir().expect("临时目录");
    let core = NativeCore::open_with_secrets(config(dir.path()), Arc::new(NullSecrets))
        .await
        .expect("打开");
    let result = core.subscribe_events(SubscribeRequest::all()).await;
    match result {
        Err(NativeError::RealtimeUnavailable(why)) => {
            assert!(
                !why.contains("caps.websocket=false"),
                "不应是能力拒绝路径：{why}"
            );
        }
        Err(other) => panic!("预期 RealtimeUnavailable（建流失败），实际：{other}"),
        Ok(_) => panic!("网络不可达不该订阅成功"),
    }
}
