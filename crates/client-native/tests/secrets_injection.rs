//! D4 平台端口：`NativeCore` 的密钥后端注入缝隙。
//!
//! 桌面默认走 `FileSecrets`；移动端要把密钥放进系统安全存储（iOS Keychain /
//! Android Keystore）。`Secrets` 在 `Ports`/`Session` 层本就是 `Arc<dyn Secrets>`
//! 注入，这里验装配层也把缝隙开好了：
//!
//! 1. 注入的后端**真的被用上**（缓存密钥落进注入方，而不是默认文件）；
//! 2. 不注入时默认行为不变（仍 `FileSecrets`，`secrets.tsv` 照常生成）；
//! 3. 后端拒绝访问时**明确报错**，不静默吞掉（与"解不开就报错"同口径）。

use std::sync::{Arc, Mutex};

use wxlike_client_core::ports::{Secret, SecretError, Secrets};
use wxlike_client_native::{NativeConfig, NativeCore, CACHE_KEY};

/// 记录型密钥后端：内存存放 + 记下每次写入的 key，用来证明"注入方被用到"。
#[derive(Default)]
struct RecordingSecrets {
    entries: Mutex<Vec<(String, Vec<u8>)>>,
    puts: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Secrets for RecordingSecrets {
    async fn get(&self, key: &str) -> Result<Option<Secret>, SecretError> {
        let entries = self.entries.lock().unwrap();
        Ok(entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| Secret::new(v.clone())))
    }

    async fn put(&self, key: &str, value: Secret) -> Result<(), SecretError> {
        self.puts.lock().unwrap().push(key.to_string());
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|(k, _)| k != key);
        entries.push((key.to_string(), value.expose().to_vec()));
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.entries.lock().unwrap().retain(|(k, _)| k != key);
        Ok(())
    }
}

/// 一律拒绝的后端：模拟系统密钥库不可用 / 用户拒绝生物识别。
struct DenyingSecrets;

#[async_trait::async_trait]
impl Secrets for DenyingSecrets {
    async fn get(&self, _key: &str) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::Denied)
    }
    async fn put(&self, _key: &str, _value: Secret) -> Result<(), SecretError> {
        Err(SecretError::Denied)
    }
    async fn delete(&self, _key: &str) -> Result<(), SecretError> {
        Err(SecretError::Denied)
    }
}

fn config(dir: &std::path::Path) -> NativeConfig {
    NativeConfig::new("http://127.0.0.1:1", dir)
}

#[tokio::test]
async fn injected_backend_is_used_for_cache_key() {
    let dir = tempfile::tempdir().expect("临时目录");
    let backend = Arc::new(RecordingSecrets::default());
    let core = NativeCore::open_with_secrets(config(dir.path()), backend.clone())
        .await
        .expect("注入打开");

    // 敏感缓存往返：加解密用的是注入后端里的密钥。
    core.put_cache("note", b"hello", true)
        .await
        .expect("写缓存");
    assert_eq!(
        core.get_cache("note").await.expect("读缓存"),
        Some(b"hello".to_vec())
    );

    // 缓存密钥确实落进了注入方，而不是默认文件。
    // 锁在 await 前释放（ADR-28：async 路径不持同步锁）。
    let wrote_cache_key = {
        let puts = backend.puts.lock().unwrap();
        puts.iter().any(|k| k == CACHE_KEY)
    };
    assert!(wrote_cache_key, "注入后端应被写入缓存密钥");
    assert!(
        backend.get(CACHE_KEY).await.expect("读").is_some(),
        "注入后端应持有缓存密钥"
    );
    // 默认密钥文件不该出现（密钥没走 FileSecrets）。
    assert!(
        !dir.path().join("secrets.tsv").exists(),
        "注入后端时不应生成默认 secrets.tsv"
    );
    core.shutdown();
}

#[tokio::test]
async fn default_open_still_uses_file_secrets() {
    let dir = tempfile::tempdir().expect("临时目录");
    let core = NativeCore::open(config(dir.path()))
        .await
        .expect("默认打开");
    core.put_cache("note", b"hi", true).await.expect("写缓存");
    assert_eq!(
        core.get_cache("note").await.expect("读缓存"),
        Some(b"hi".to_vec())
    );
    // 默认路径行为不变：密钥文件照常生成。
    assert!(
        dir.path().join("secrets.tsv").exists(),
        "默认 open 应仍用 FileSecrets 生成 secrets.tsv"
    );
    core.shutdown();
}

#[tokio::test]
async fn denying_backend_fails_loudly() {
    let dir = tempfile::tempdir().expect("临时目录");
    let result = NativeCore::open_with_secrets(config(dir.path()), Arc::new(DenyingSecrets)).await;
    let err = match result {
        Err(err) => err,
        Ok(_) => panic!("拒绝访问必须报错，却打开成功了"),
    };
    assert!(
        err.to_string().contains("密钥"),
        "错误应指明密钥存储不可用：{err}"
    );
}
