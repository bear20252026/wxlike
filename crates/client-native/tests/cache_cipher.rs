//! 敏感缓存加密与设备密钥存储。
//!
//! 这一层出问题的表现很难看：**能存能读，但内容已经是错的**（密钥换了、
//! 密文被截断，却仍然被当成明文渲染）。所以这里专门验"解不开时明确报错"。

use wxlike_client_core::ports::{Secret, Secrets};
use wxlike_client_native::{CacheCipher, FileSecrets};

fn key_of(byte: u8) -> Vec<u8> {
    vec![byte; 32]
}

#[test]
fn seal_then_open_round_trips() {
    let cipher = CacheCipher::new(&key_of(7)).expect("cipher");
    let sealed = cipher
        .seal(b"{\"text\":\"\xe4\xbd\xa0\xe5\xa5\xbd\"}")
        .expect("加密");
    assert_ne!(
        sealed.as_slice(),
        b"{\"text\":\"\xe4\xbd\xa0\xe5\xa5\xbd\"}",
        "密文不该等于明文"
    );
    let opened = cipher.open(&sealed).expect("解密");
    assert_eq!(opened, b"{\"text\":\"\xe4\xbd\xa0\xe5\xa5\xbd\"}");
}

#[test]
fn same_plaintext_gets_different_ciphertext() {
    // nonce 每次随机：相同内容不能产生相同密文（否则等于泄露"这两条一样"）
    let cipher = CacheCipher::new(&key_of(3)).expect("cipher");
    let a = cipher.seal(b"same").expect("a");
    let b = cipher.seal(b"same").expect("b");
    assert_ne!(a, b);
}

#[test]
fn wrong_key_fails_loudly() {
    let sealed = CacheCipher::new(&key_of(1))
        .expect("a")
        .seal(b"secret")
        .expect("seal");
    let err = CacheCipher::new(&key_of(2))
        .expect("b")
        .open(&sealed)
        .expect_err("换密钥必须解不开");
    assert!(err.to_string().contains("解密失败"), "{err}");
}

#[test]
fn truncated_ciphertext_is_rejected() {
    let cipher = CacheCipher::new(&key_of(9)).expect("cipher");
    let sealed = cipher.seal(b"payload").expect("seal");
    let err = cipher.open(&sealed[..4]).expect_err("截断");
    assert!(err.to_string().contains("过短"), "{err}");
}

#[test]
fn bad_key_length_is_rejected() {
    assert!(CacheCipher::new(&[0u8; 8]).is_err());
}

#[tokio::test]
async fn file_secrets_persist_across_reopen() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("secrets.tsv");
    {
        let secrets = FileSecrets::open(&path).expect("打开");
        secrets
            .put("token", Secret::from_text("abc123"))
            .await
            .expect("写");
    }
    let reopened = FileSecrets::open(&path).expect("重开");
    let token = reopened.get("token").await.expect("读").expect("存在");
    assert_eq!(token.expose(), b"abc123");
    reopened.delete("token").await.expect("删");
    assert!(reopened.get("token").await.expect("读").is_none());
}

#[tokio::test]
async fn load_or_create_is_stable() {
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("secrets.tsv");
    let secrets = FileSecrets::open(&path).expect("打开");
    let first = CacheCipher::load_or_create(&secrets).await.expect("首次");
    let sealed = first.seal(b"x").expect("加密");
    let second = CacheCipher::load_or_create(&secrets).await.expect("再次");
    assert_eq!(second.open(&sealed).expect("同一密钥"), b"x");
}

#[test]
fn secrets_file_is_hex_encoded_not_raw() {
    // 文件里不该出现明文 token（备份/导出时先挡住一层）
    let dir = tempfile::tempdir().expect("临时目录");
    let path = dir.path().join("secrets.tsv");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let secrets = FileSecrets::open(&path).expect("打开");
        secrets
            .put("token", Secret::from_text("plain-token"))
            .await
            .expect("写");
    });
    let text = std::fs::read_to_string(&path).expect("读文件");
    assert!(!text.contains("plain-token"), "{text}");
    assert!(text.contains("token\t"), "{text}");
}
