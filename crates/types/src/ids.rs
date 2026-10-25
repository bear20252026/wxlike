//! 不透明 ID。
//!
//! 全仓库曾经到处是 `String` 当 ID：`fn kick(conv: &str, user: &str)` 传反了
//! 也能编译，写进库里才发现"群 id 里存了个用户 id"。这里把每种 ID 变成
//! 独立的 newtype，字段私有、只能 `parse`、只能借出 `&str`：
//!
//! * 传错类型 = 编译错误，不是线上事故；
//! * 校验在**构造时**做一次，之后每个消费点都不用再判空；
//! * 序列化形态固定为字符串（`MessageId` 是 32 位小写十六进制），
//!   与 TS / Dart 的生成类型一字不差。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// ID 解析失败。
///
/// 单独一个类型而不是 `String`：调用方要能区分"空"、"太长"、"有非法字符"，
/// 才能给出准确的参数错误（`DomainError::InvalidArgument` 的 `field`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    /// 空字符串。
    Empty,
    /// 超过 [`MAX_ID_LEN`] 字节。
    TooLong {
        /// 实际长度。
        len: usize,
        /// 上限。
        max: usize,
    },
    /// 含不允许的字符。
    BadChar {
        /// 违规字符。
        ch: char,
        /// 字节偏移。
        index: usize,
    },
    /// 形状不对（目前只有十六进制类 ID 会报）。
    BadFormat(&'static str),
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdError::Empty => f.write_str("id 不能为空"),
            IdError::TooLong { len, max } => write!(f, "id 太长：{len} 字节，上限 {max}"),
            IdError::BadChar { ch, index } => write!(f, "id 第 {index} 字节含非法字符 {ch:?}"),
            IdError::BadFormat(what) => write!(f, "id 形状不对：{what}"),
        }
    }
}

impl std::error::Error for IdError {}

/// ID 允许的最大字节长度。
///
/// 64 是"服务端生成 + 客户端后缀"都够用的上界；更重要的是**有上界**——
/// 没有它，一个 10MB 的请求体能换一个进库的 key。
pub const MAX_ID_LEN: usize = 64;

/// 通用 ID 校验：非空、不超长、只含 URL 安全字符。
///
/// `~` 是历史包袱：直聊会话 id 形如 `d-alice~bob`（见 [`crate::conv`]）。
fn check_generic(raw: &str) -> Result<(), IdError> {
    if raw.is_empty() {
        return Err(IdError::Empty);
    }
    if raw.len() > MAX_ID_LEN {
        return Err(IdError::TooLong {
            len: raw.len(),
            max: MAX_ID_LEN,
        });
    }
    for (index, ch) in raw.char_indices() {
        let allowed = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~' | ':' | '@');
        if !allowed {
            return Err(IdError::BadChar { ch, index });
        }
    }
    Ok(())
}

/// 生成一个"字段私有、只能 parse"的 ID 类型。
///
/// 第二形态可以带一个额外的校验函数（`UserId` 用它挡掉 `~`）。
macro_rules! opaque_id {
    ($(#[$attr:meta])* $name:ident, $kind:literal) => {
        opaque_id!(@build $(#[$attr])* $name, $kind, check_generic);
    };
    ($(#[$attr:meta])* $name:ident, $kind:literal, $check:path) => {
        opaque_id!(@build $(#[$attr])* $name, $kind, $check);
    };
    (@build $(#[$attr:meta])* $name:ident, $kind:literal, $check:path) => {
        $(#[$attr])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// 类型标签，用于日志与生成代码里的 `kind` 字段。
            pub const KIND: &'static str = $kind;

            /// 解析并校验。
            ///
            /// # Errors
            /// 空、超长或含非法字符时返回 [`IdError`]。
            pub fn parse(raw: impl Into<String>) -> Result<Self, IdError> {
                let raw = raw.into();
                $check(&raw)?;
                Ok(Self(raw))
            }

            /// 借用内部文本。**不要**把它存起来当"已校验"的证明。
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}(\"{}\")", Self::KIND, self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::parse(s)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(d)?;
                Self::parse(raw).map_err(serde::de::Error::custom)
            }
        }
    };
}

/// 用户 id 的额外规则：不含 `~`。
///
/// 直聊会话 id 是 `d-<小>~<大>`（见 [`crate::conv`]），用户 id 里再出现 `~`
/// 就没法无歧义地拆回来——那是"同一个人两个会话"级别的 bug，所以在入口挡掉。
fn check_user(raw: &str) -> Result<(), IdError> {
    check_generic(raw)?;
    if let Some(index) = raw.find('~') {
        return Err(IdError::BadChar { ch: '~', index });
    }
    Ok(())
}

opaque_id!(
    /// 用户 id。服务端生成，客户端不得构造。
    UserId,
    "user",
    check_user
);
opaque_id!(
    /// 会话 id：群 id 或直聊 id（`d-<小>-<大>`）。
    ConvId,
    "conv"
);
opaque_id!(
    /// 设备 id（每台登录设备一个）。
    DeviceId,
    "device"
);
opaque_id!(
    /// 命令 id，形如 `msg.send`。与 `crates/cmd` 的 catalog 一一对应。
    ActionId,
    "action"
);
opaque_id!(
    /// UI 入口 id，形如 `desk.chat.send`。G00-01 的入口总账用它。
    EntryId,
    "entry"
);
opaque_id!(
    /// 事件 id。同一事件在重放时必须得到同一个 id（去重的依据）。
    EventId,
    "event"
);
opaque_id!(
    /// 链路追踪 id。客户端生成，服务端原样带回。
    TraceId,
    "trace"
);

impl TraceId {
    /// 由任意文本派生一个合法的追踪 id。
    ///
    /// 追踪 id 是**客户端生成**的，来源往往是幂等键、请求序号这类不一定
    /// 合规的文本。派生规则：非法字符换成 `-`、超长截断、空串补 `trace`。
    /// 这样调用点不用处理"派生失败"这个几乎不可能发生却必须写的分支。
    #[must_use]
    pub fn derive(seed: &str) -> Self {
        let mut out = String::with_capacity(MAX_ID_LEN);
        for ch in seed.chars() {
            if out.len() >= MAX_ID_LEN {
                break;
            }
            let allowed =
                ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~' | ':' | '@');
            out.push(if allowed { ch } else { '-' });
        }
        if out.is_empty() {
            out.push_str("trace");
        }
        Self(out)
    }
}

/// 幂等键。
///
/// 单独一个类型是因为它有一条额外规则：**写命令必须带、读命令不许带**。
/// 用 `String` 时这条规则只能靠 code review 守（历史上漏过：
/// `group.create` 没带键，重试一次就是两个群）。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// 类型标签。
    pub const KIND: &'static str = "idem";
    /// 幂等键允许的最大长度。比通用 ID 短：它只活一次重试窗口。
    pub const MAX_LEN: usize = 96;

    /// 解析并校验。
    ///
    /// # Errors
    /// 空、超长、含非法字符时返回 [`IdError`]。
    pub fn parse(raw: impl Into<String>) -> Result<Self, IdError> {
        let raw = raw.into();
        if raw.len() > Self::MAX_LEN {
            return Err(IdError::TooLong {
                len: raw.len(),
                max: Self::MAX_LEN,
            });
        }
        check_generic(&raw)?;
        Ok(Self(raw))
    }

    /// 借用内部文本。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for IdempotencyKey {
    /// 只打长度：幂等键进日志后能被重放，不该明文落盘。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IdempotencyKey(<{} 字节>)", self.0.len())
    }
}

impl FromStr for IdempotencyKey {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for IdempotencyKey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for IdempotencyKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// 消息 id：16 字节，序列化成 32 位小写十六进制。
///
/// 与 `crates/core::types::MsgId`（`[u8; 16]`）**同一批字节**，只是这里换了
/// 表示：JSON 上必须是字符串，否则 TS 端拿 `number` 接会丢精度、Dart web
/// 端更是直接溢出。bincode 线上的 `[u8; 16]` 不动（那是另一条通道）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId([u8; 16]);

/// 全零消息 id：表示"没有去重键"（协议 v1 的老消息）。
pub const NO_MESSAGE_ID: MessageId = MessageId([0u8; 16]);

impl MessageId {
    /// 类型标签。
    pub const KIND: &'static str = "msg";
    /// 字节长度。
    pub const LEN: usize = 16;

    /// 从原始字节构造。
    #[must_use]
    pub fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    /// 全零 id（"没有去重键"）。
    #[must_use]
    pub fn nil() -> Self {
        NO_MESSAGE_ID
    }

    /// 是否全零。
    #[must_use]
    pub fn is_nil(&self) -> bool {
        self.0.iter().all(|b| *b == 0)
    }

    /// 原始字节。
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// 小写十六进制（32 字符）。
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// 从十六进制解析。
    ///
    /// # Errors
    /// 长度不是 32 或含非十六进制字符时返回 [`IdError::BadFormat`]。
    pub fn parse_hex(raw: &str) -> Result<Self, IdError> {
        let bytes = hex::decode(raw).map_err(|_| IdError::BadFormat("期望 32 位十六进制"))?;
        let arr: [u8; Self::LEN] = bytes
            .try_into()
            .map_err(|_| IdError::BadFormat("期望 32 位十六进制（16 字节）"))?;
        Ok(Self(arr))
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MessageId(\"{}\")", self.to_hex())
    }
}

impl FromStr for MessageId {
    type Err = IdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_hex(s)
    }
}

impl Serialize for MessageId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for MessageId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Self::parse_hex(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_and_overlong() {
        assert_eq!(UserId::parse(""), Err(IdError::Empty));
        let long = "u".repeat(MAX_ID_LEN + 1);
        assert!(matches!(UserId::parse(long), Err(IdError::TooLong { .. })));
    }

    #[test]
    fn rejects_separators_that_break_conv_ids() {
        // `~` 允许（直聊 id），`|` 不允许：它会让服务端的复合 key 歧义
        assert!(UserId::parse("a|b").is_err());
        assert!(ConvId::parse("d-a~b").is_ok());
        // 用户 id 不能带 `~`：直聊 id 用它当分隔符
        assert!(UserId::parse("a~b").is_err());
    }

    #[test]
    fn message_id_is_32_hex() {
        let id = MessageId::parse_hex("00112233445566778899aabbccddeeff").expect("hex");
        assert_eq!(id.to_hex(), "00112233445566778899aabbccddeeff");
        assert!(!id.is_nil());
        assert!(MessageId::nil().is_nil());
        assert!(MessageId::parse_hex("0011").is_err());
        assert!(MessageId::parse_hex("zz112233445566778899aabbccddeeff").is_err());
    }

    #[test]
    fn deserialize_validates() {
        assert!(serde_json::from_str::<UserId>("\"\"").is_err());
        let ok: UserId = serde_json::from_str("\"alice\"").expect("user");
        assert_eq!(ok.as_str(), "alice");
    }

    #[test]
    fn idempotency_key_debug_does_not_leak() {
        let key = IdempotencyKey::parse("secret-key-value").expect("key");
        let shown = format!("{key:?}");
        assert!(!shown.contains("secret"), "{shown}");
    }
}
