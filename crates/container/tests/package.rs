//! P5 小程序包签名契约：验签、防降级、权限白名单。
//!
//! 这里刻意覆盖"签名合法但业务不该接受"的路径——防降级与重复安装是
//! 签名机制本身挡不住的攻击面，必须由注册表单独裁决。

use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;
use wxlike_container::package::{
    generate_key, sign_manifest, verify_package, Manifest, PackageError, PackageRegistry,
};

fn manifest(app_id: &str, version: &str, perms: &[&str]) -> Manifest {
    Manifest {
        app_id: app_id.into(),
        name: "演示小程序".into(),
        version: version.into(),
        entry: "index.js".into(),
        permissions: perms.iter().map(|s| s.to_string()).collect(),
    }
}

fn bytes(m: &Manifest) -> Vec<u8> {
    serde_json::to_vec(m).unwrap()
}

fn registry_with(key: &SigningKey, key_id: &str) -> PackageRegistry {
    let mut trusted = BTreeMap::new();
    trusted.insert(key_id.to_string(), key.verifying_key());
    PackageRegistry::new(trusted)
}

#[test]
fn signed_manifest_verifies_and_grants_only_whitelist() {
    let key = generate_key();
    let m = manifest(
        "com.demo.chat",
        "1.2.0",
        &["im.send_message", "im.delete_user", "pay.request"],
    );
    let raw = bytes(&m);
    let sig = sign_manifest(&raw, &key, "k1");

    let mut trusted = BTreeMap::new();
    trusted.insert("k1".to_string(), key.verifying_key());
    let parsed = verify_package(&raw, &sig, &trusted).unwrap();
    assert_eq!(parsed, m);
    assert_eq!(
        parsed.granted_permissions(),
        vec!["im.send_message".to_string(), "pay.request".to_string()],
        "白名单外权限不授予"
    );
    assert_eq!(
        parsed.denied_permissions(),
        vec!["im.delete_user".to_string()]
    );
}

#[test]
fn tampered_manifest_fails_verification() {
    let key = generate_key();
    let raw = bytes(&manifest("com.demo.app", "1.0.0", &["im.send_message"]));
    let sig = sign_manifest(&raw, &key, "k1");

    // 攻击者把权限改大（签名不变）
    let mut evil = manifest("com.demo.app", "1.0.0", &["im.send_message", "im.admin"]);
    evil.permissions.push("pay.request".into());
    let evil_raw = bytes(&evil);

    let mut trusted = BTreeMap::new();
    trusted.insert("k1".to_string(), key.verifying_key());
    assert_eq!(
        verify_package(&evil_raw, &sig, &trusted).unwrap_err(),
        PackageError::BadSigning
    );
}

#[test]
fn unknown_key_and_malformed_signature_are_rejected() {
    let key = generate_key();
    let raw = bytes(&manifest("com.demo.app", "1.0.0", &[]));
    let sig = sign_manifest(&raw, &key, "k1");

    // 信任表里没有这个 key_id
    let trusted = BTreeMap::new();
    assert_eq!(
        verify_package(&raw, &sig, &trusted).unwrap_err(),
        PackageError::BadSigning
    );

    // 签名不是 64 字节
    let mut short = sig.clone();
    short.sig = "abcd".into();
    let mut trusted2 = BTreeMap::new();
    trusted2.insert("k1".to_string(), key.verifying_key());
    assert_eq!(
        verify_package(&raw, &short, &trusted2).unwrap_err(),
        PackageError::BadSignature
    );
}

#[test]
fn registry_rejects_downgrade_and_duplicate_but_allows_upgrade() {
    let key = generate_key();
    let mut reg = registry_with(&key, "k1");

    let v1 = bytes(&manifest("com.demo.app", "1.0.0", &["im.send_message"]));
    let granted = reg.install(&v1, &sign_manifest(&v1, &key, "k1")).unwrap();
    assert_eq!(granted, vec!["im.send_message".to_string()]);

    // 同版本重复安装
    assert_eq!(
        reg.install(&v1, &sign_manifest(&v1, &key, "k1"))
            .unwrap_err(),
        PackageError::Duplicate("com.demo.app".into())
    );

    // 升级允许
    let v2 = bytes(&manifest("com.demo.app", "1.1.0", &["im.send_message"]));
    reg.install(&v2, &sign_manifest(&v2, &key, "k1")).unwrap();
    assert_eq!(reg.installed("com.demo.app").unwrap().version, "1.1.0");

    // 降级拒绝（签名合法也拒）
    let err = reg
        .install(&v1, &sign_manifest(&v1, &key, "k1"))
        .unwrap_err();
    assert_eq!(
        err,
        PackageError::Downgrade("com.demo.app".into(), "1.0.0".into(), "1.1.0".into())
    );
}

#[test]
fn manifest_validation_rejects_path_traversal_and_bad_version() {
    let key = generate_key();
    let mut trusted = BTreeMap::new();
    trusted.insert("k1".to_string(), key.verifying_key());

    let cases = [
        Manifest {
            entry: "../escape.js".into(),
            ..manifest("com.demo.app", "1.0.0", &[])
        },
        Manifest {
            entry: "/abs.js".into(),
            ..manifest("com.demo.app", "1.0.0", &[])
        },
        Manifest {
            app_id: "../evil".into(),
            ..manifest("com.demo.app", "1.0.0", &[])
        },
        Manifest {
            version: "not-a-version".into(),
            ..manifest("com.demo.app", "1.0.0", &[])
        },
    ];
    for m in cases {
        let raw = bytes(&m);
        let sig = sign_manifest(&raw, &key, "k1");
        assert!(
            verify_package(&raw, &sig, &trusted).is_err(),
            "should reject {m:?}"
        );
    }
}

#[test]
fn semver_ignores_prerelease_suffix() {
    let m = manifest("com.demo.app", "2.3.4-beta.1", &[]);
    assert_eq!(m.semver(), Some((2, 3, 4)));
    assert_eq!(manifest("a", "1", &[]).semver(), Some((1, 0, 0)));
}

#[test]
fn uninstall_allows_reinstall_same_version() {
    let key = generate_key();
    let mut reg = registry_with(&key, "k1");
    let raw = bytes(&manifest("com.demo.app", "1.0.0", &[]));
    reg.install(&raw, &sign_manifest(&raw, &key, "k1")).unwrap();
    assert!(reg.uninstall("com.demo.app"));
    assert!(!reg.uninstall("com.demo.app"));
    reg.install(&raw, &sign_manifest(&raw, &key, "k1")).unwrap();
    assert_eq!(reg.list().len(), 1);
}
