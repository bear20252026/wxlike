//! P5 小程序包签名与权限白名单（ADR-14 M5：包管理 / 签名 / 权限）。
//!
//! 三件事，都是"没有它就不该上线"的底线：
//!
//! 1. **签名**：包体（manifest + 资源清单）由发布方用 ed25519 私钥签名，宿主
//!    只信任内置公钥。签名覆盖 manifest 原文，任何字段被改都会验签失败；
//! 2. **防降级**：同 `app_id` 只允许版本单调上升——否则攻击者可以拿一个旧版
//!    有漏洞的签名包覆盖新版（签名本身是合法的，光验签挡不住）；
//! 3. **权限白名单**：`im_bridge` 只暴露受限子集（合同 §4.1 不变式 6）。
//!    清单里写了白名单外的权限**不报错但不授予**——授予面只由宿主代码决定，
//!    永远不由包清单扩大。
//!
//! 包格式复用 DPG1（`wxlike-dpage`）：签名不塞进包体，而是旁挂的
//! `(key_id, signature)`——这样包体可以按内容寻址（sha256），签名单独轮换。

use std::collections::BTreeMap;

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// 宿主允许 `im_bridge` 暴露的方法（白名单即全部授予面）。
pub const IM_BRIDGE_ALLOWED: [&str; 4] = [
    "im.send_message",
    "im.profile.read",
    "im.conversation.read",
    "pay.request",
];

/// 包清单。序列化后即为签名对象，字段顺序由 serde 固定。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub app_id: String,
    pub name: String,
    /// 语义化版本，只比较前三段数字（`1.2.3-beta` 取 `(1,2,3)`）。
    pub version: String,
    /// 入口脚本相对路径。
    pub entry: String,
    /// 包清单声明的权限（可能包含白名单外的项，见 [`granted_permissions`]）。
    #[serde(default)]
    pub permissions: Vec<String>,
}

impl Manifest {
    /// 版本三元组；解析失败返回 `None`（调用方按"非法清单"拒绝）。
    pub fn semver(&self) -> Option<(u64, u64, u64)> {
        let core = self.version.split(['-', '+']).next()?;
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;
        Some((major, minor, patch))
    }

    /// 清单自检：id/name/entry 的形状、入口路径不得穿越。
    pub fn validate(&self) -> Result<()> {
        if self.app_id.is_empty() || self.app_id.len() > 64 {
            return Err(PackageError::BadManifest("app_id"));
        }
        if !self
            .app_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        {
            return Err(PackageError::BadManifest("app_id charset"));
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 64 {
            return Err(PackageError::BadManifest("name"));
        }
        if self.entry.is_empty()
            || self.entry.starts_with('/')
            || self.entry.split('/').any(|seg| seg == "..")
            || self.entry.contains('\\')
        {
            return Err(PackageError::BadManifest("entry path"));
        }
        if self.semver().is_none() {
            return Err(PackageError::BadManifest("version"));
        }
        Ok(())
    }

    /// 实际授予的权限 = 清单声明 ∩ 宿主白名单（顺序按白名单，稳定输出）。
    pub fn granted_permissions(&self) -> Vec<String> {
        IM_BRIDGE_ALLOWED
            .iter()
            .filter(|allowed| self.permissions.iter().any(|p| p == *allowed))
            .map(|p| (*p).to_string())
            .collect()
    }

    /// 被清单声明但白名单不授予的权限（运营审计用）。
    pub fn denied_permissions(&self) -> Vec<String> {
        self.permissions
            .iter()
            .filter(|p| !IM_BRIDGE_ALLOWED.contains(&p.as_str()))
            .cloned()
            .collect()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PackageError {
    #[error("manifest invalid: {0}")]
    BadManifest(&'static str),
    #[error("manifest is not valid JSON")]
    BadJson,
    #[error("signature malformed")]
    BadSignature,
    #[error("signature does not verify against the trusted key")]
    BadSigning,
    #[error("package {0:?} version {1} is not newer than the installed {2}")]
    Downgrade(String, String, String),
    #[error("package {0:?} already installed at this version")]
    Duplicate(String),
}

pub type Result<T> = std::result::Result<T, PackageError>;

/// 旁挂签名（不进入包体，便于按内容寻址与密钥轮换）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PackageSignature {
    pub key_id: String,
    /// hex(ed25519 signature over `manifest_bytes`)
    pub sig: String,
}

/// 发布方签名（开发/CI 用；线上私钥不进仓库）。
pub fn sign_manifest(manifest_bytes: &[u8], key: &SigningKey, key_id: &str) -> PackageSignature {
    PackageSignature {
        key_id: key_id.to_string(),
        sig: hex::encode(key.sign(manifest_bytes).to_bytes()),
    }
}

/// 验签 + 清单校验。`trusted` 是 `key_id -> 公钥` 的信任表。
pub fn verify_package(
    manifest_bytes: &[u8],
    sig: &PackageSignature,
    trusted: &BTreeMap<String, VerifyingKey>,
) -> Result<Manifest> {
    let key = trusted.get(&sig.key_id).ok_or(PackageError::BadSigning)?;
    let raw = hex::decode(&sig.sig).map_err(|_| PackageError::BadSignature)?;
    let bytes: [u8; 64] = raw.try_into().map_err(|_| PackageError::BadSignature)?;
    let signature = Signature::from_bytes(&bytes);
    key.verify_strict(manifest_bytes, &signature)
        .map_err(|_| PackageError::BadSigning)?;

    let manifest: Manifest =
        serde_json::from_slice(manifest_bytes).map_err(|_| PackageError::BadJson)?;
    manifest.validate()?;
    Ok(manifest)
}

/// 已安装包表：只接受"验签通过 + 版本严格上升"的包。
#[derive(Default)]
pub struct PackageRegistry {
    trusted: BTreeMap<String, VerifyingKey>,
    installed: BTreeMap<String, Manifest>,
}

impl PackageRegistry {
    pub fn new(trusted: BTreeMap<String, VerifyingKey>) -> Self {
        Self {
            trusted,
            installed: BTreeMap::new(),
        }
    }

    /// 安装：验签 → 清单校验 → 防降级 → 落表。返回授予的权限。
    pub fn install(
        &mut self,
        manifest_bytes: &[u8],
        sig: &PackageSignature,
    ) -> Result<Vec<String>> {
        let manifest = verify_package(manifest_bytes, sig, &self.trusted)?;
        if let Some(current) = self.installed.get(&manifest.app_id) {
            let new = manifest
                .semver()
                .ok_or(PackageError::BadManifest("version"))?;
            let old = current
                .semver()
                .ok_or(PackageError::BadManifest("version"))?;
            if new == old {
                return Err(PackageError::Duplicate(manifest.app_id.clone()));
            }
            if new < old {
                return Err(PackageError::Downgrade(
                    manifest.app_id.clone(),
                    manifest.version.clone(),
                    current.version.clone(),
                ));
            }
        }
        let granted = manifest.granted_permissions();
        self.installed.insert(manifest.app_id.clone(), manifest);
        Ok(granted)
    }

    pub fn installed(&self, app_id: &str) -> Option<&Manifest> {
        self.installed.get(app_id)
    }

    pub fn list(&self) -> Vec<&Manifest> {
        self.installed.values().collect()
    }

    /// 卸载（保留版本水位由调用方决定；这里直接删除，允许重装同版本）。
    pub fn uninstall(&mut self, app_id: &str) -> bool {
        self.installed.remove(app_id).is_some()
    }
}

/// 生成测试/开发用密钥对（线上私钥必须离线保管）。
///
/// 用 `rand` 直接填 32 字节种子再 `from_bytes`，避免把 rand_core 的版本
/// 约束暴露给调用方（ed25519-dalek 2 的 `generate` 需要特定 rand_core）。
pub fn generate_key() -> SigningKey {
    use rand::RngCore as _;
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    SigningKey::from_bytes(&seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_grants_only_whitelisted_permissions() {
        let m = Manifest {
            app_id: "com.demo.app".into(),
            name: "demo".into(),
            version: "1.0.0".into(),
            entry: "index.js".into(),
            permissions: vec![
                "im.send_message".into(),
                "im.delete_user".into(),
                "pay.request".into(),
            ],
        };
        assert_eq!(
            m.granted_permissions(),
            vec!["im.send_message".to_string(), "pay.request".to_string()]
        );
        assert_eq!(m.denied_permissions(), vec!["im.delete_user".to_string()]);
    }
}
