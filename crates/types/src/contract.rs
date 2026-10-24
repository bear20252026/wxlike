//! 契约版本。
//!
//! 客户端与服务端的兼容性判断只有一条规则：**主版本相同、次版本不高于本端**。
//! 服务端加字段 = 次版本 +1（旧客户端忽略未知字段）；改语义 / 删字段 =
//! 主版本 +1（旧客户端必须拒绝，而不是"试试看"）。
//!
//! 生成物（TS / Dart 类型、fixture、CLI/MCP 描述）都带 [`SchemaStamp`]：
//! 版本 + 源文件摘要。摘要对不上说明"生成物比契约旧"，`--check` 直接红。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 契约版本号，`major.minor`。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct ContractVersion {
    /// 破坏兼容的变更。
    pub major: u16,
    /// 向后兼容的新增。
    pub minor: u16,
}

/// 本端实现的契约版本。
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(1, 0);

/// 生成物里写的 schema 目录名。
pub const SCHEMA_VERSION: &str = "contract-v1";

impl ContractVersion {
    /// 构造。
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// 本端能否与 `peer` 通信。
    ///
    /// 同主版本、且 `peer` 的次版本不高于本端——即"peer 没有用到本端不认识的
    /// 字段"。反过来（本端次版本更高）也是兼容的，由调用方按方向选择：
    /// 服务端用 `self.accepts(client)`，客户端用 `self.accepts(server)`。
    #[must_use]
    pub fn accepts(&self, peer: ContractVersion) -> bool {
        self.major == peer.major && peer.minor <= self.minor
    }
}

impl fmt::Display for ContractVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl Serialize for ContractVersion {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ContractVersion {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        parse_version(&raw).map_err(serde::de::Error::custom)
    }
}

fn parse_version(raw: &str) -> Result<ContractVersion, String> {
    let (major, minor) = raw
        .split_once('.')
        .ok_or_else(|| format!("契约版本必须是 major.minor：{raw:?}"))?;
    let major = major
        .parse::<u16>()
        .map_err(|_| format!("契约主版本不是数字：{raw:?}"))?;
    let minor = minor
        .parse::<u16>()
        .map_err(|_| format!("契约次版本不是数字：{raw:?}"))?;
    Ok(ContractVersion::new(major, minor))
}

/// 生成物里的"来源戳"：schema 版本 + 源文件摘要。
///
/// 摘要由 `tools/codegen` 计算（对所有 schema 文件内容做 SHA-256，取前 16 位
/// 十六进制）。生成物头部、fixture、CLI/MCP 描述里都写同一份，任何一处
/// 与契约源不一致都能被发现。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SchemaStamp {
    /// 例如 `contract-v1`。
    pub schema_version: String,
    /// 源文件摘要（16 位十六进制）。
    pub digest: String,
    /// 生成该戳的契约版本。
    pub contract: ContractVersion,
}

impl SchemaStamp {
    /// 构造。
    #[must_use]
    pub fn new(
        schema_version: impl Into<String>,
        digest: impl Into<String>,
        contract: ContractVersion,
    ) -> Self {
        Self {
            schema_version: schema_version.into(),
            digest: digest.into(),
            contract,
        }
    }
}

impl fmt::Display for SchemaStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}@{} (contract {})",
            self.schema_version, self.digest, self.contract
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compat_rule_is_one_directional() {
        let client = ContractVersion::new(1, 2);
        assert!(client.accepts(ContractVersion::new(1, 2)));
        assert!(client.accepts(ContractVersion::new(1, 0)));
        // 服务端比客户端新一个次版本：客户端不认它的新字段，但能解
        assert!(!client.accepts(ContractVersion::new(1, 3)));
        // 主版本不同：直接拒绝
        assert!(!client.accepts(ContractVersion::new(2, 0)));
    }

    #[test]
    fn version_round_trips_as_string() {
        let text = serde_json::to_string(&CONTRACT_VERSION).expect("ser");
        assert_eq!(text, "\"1.0\"");
        assert_eq!(
            serde_json::from_str::<ContractVersion>(&text).expect("de"),
            CONTRACT_VERSION
        );
        assert!(serde_json::from_str::<ContractVersion>("\"1\"").is_err());
    }
}
