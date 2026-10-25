//! 不透明游标：分页与事件订阅的"我读到哪了"。
//!
//! 为什么不是一个裸 `u64`：游标的含义一定会变（加个 `conv`、换成分片偏移、
//! 从"按 seq"改成"按时间+seq 混合"）。一旦客户端把它当数字，服务端就再也
//! 改不动了——所以这里给的是**只能原样带回**的 token：
//!
//! * 客户端不解析、不构造，只存和传；
//! * 服务端解码后拿到 `(kind, seq)`，带版本号，可以平滑演进；
//! * 带校验位：游标被截断/拼接/手改会**明确报错**，而不是静默从头开始
//!   （静默重放比报错更糟：用户看到的是消息重复或跳变）。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::seq::Seq;

/// 游标属于哪个流。不同类型不能混用（事件游标拿去翻消息页会漏数据）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorKind {
    /// 消息历史分页。
    Message = 1,
    /// 事件订阅（WebSocket / 长轮询）。
    Event = 2,
    /// 会话级增量同步。
    Sync = 3,
}

impl CursorKind {
    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(CursorKind::Message),
            2 => Some(CursorKind::Event),
            3 => Some(CursorKind::Sync),
            _ => None,
        }
    }

    fn tag(self) -> u8 {
        self as u8
    }
}

/// 游标解析失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorError {
    /// 形状不对（没有分隔符、base64 解不开）。
    Malformed,
    /// 校验位不匹配：被改过或截断了。
    Checksum,
    /// 版本号不认识（比本端新）。
    UnsupportedVersion(u8),
    /// 类型标签不认识。
    UnknownKind(u8),
}

impl fmt::Display for CursorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CursorError::Malformed => f.write_str("游标格式错误"),
            CursorError::Checksum => f.write_str("游标校验失败"),
            CursorError::UnsupportedVersion(v) => write!(f, "游标版本不支持：{v}"),
            CursorError::UnknownKind(k) => write!(f, "游标类型未知：{k}"),
        }
    }
}

impl std::error::Error for CursorError {}

/// 游标当前版本。
const CURSOR_VERSION: u8 = 1;

/// 游标解出来的内容。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CursorValue {
    /// 属于哪个流。
    pub kind: CursorKind,
    /// 已经消费到哪个序列号。
    pub seq: Seq,
}

/// 不透明游标。
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cursor(String);

impl Cursor {
    /// 构造一个游标。服务端返回给客户端用这个；客户端只会原样回传。
    #[must_use]
    pub fn at(kind: CursorKind, seq: Seq) -> Self {
        let raw = format!("{CURSOR_VERSION}:{}:{}", kind.tag(), seq.get());
        let tag = checksum(raw.as_bytes());
        Self(format!("{}.{tag:04x}", b64url_encode(raw.as_bytes())))
    }

    /// 借出 token 文本。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 由已经存在的 token 文本重建（格式非法时报错）。
    ///
    /// 本地库把游标存成文本，读回来要走这一步——**不能**直接 `Cursor(text)`，
    /// 那样坏数据会一路带到网络层。
    ///
    /// # Errors
    /// 同 [`Cursor::decode`]。
    pub fn parse_text(text: &str) -> Result<Self, CursorError> {
        let cursor = Self(text.to_string());
        cursor.decode()?;
        Ok(cursor)
    }

    /// 只取序列号（本地比较用）。
    ///
    /// # Errors
    /// 同 [`Cursor::decode`]。
    pub fn decode_seq(text: &str) -> Result<u64, CursorError> {
        Self::parse_text(text).map(|cursor| cursor.decode().map(|v| v.seq.get()).unwrap_or(0))
    }

    /// 解码。客户端正常情况下不需要调用它（服务端才解析），
    /// 但本地缓存要按 `kind` 排序、按 `seq` 去重时会用到。
    ///
    /// # Errors
    /// 格式、校验位、版本、类型任一不对时返回 [`CursorError`]。
    pub fn decode(&self) -> Result<CursorValue, CursorError> {
        let (body, tag) = self.0.rsplit_once('.').ok_or(CursorError::Malformed)?;
        let bytes = b64url_decode(body).ok_or(CursorError::Malformed)?;
        let expected = u16::from_str_radix(tag, 16).map_err(|_| CursorError::Malformed)?;
        if checksum(&bytes) != expected {
            return Err(CursorError::Checksum);
        }
        parse_payload(&bytes)
    }
}

fn parse_payload(bytes: &[u8]) -> Result<CursorValue, CursorError> {
    let text = std::str::from_utf8(bytes).map_err(|_| CursorError::Malformed)?;
    let mut parts = text.split(':');
    let version: u8 = parts
        .next()
        .ok_or(CursorError::Malformed)?
        .parse()
        .map_err(|_| CursorError::Malformed)?;
    if version != CURSOR_VERSION {
        return Err(CursorError::UnsupportedVersion(version));
    }
    let kind_tag: u8 = parts
        .next()
        .ok_or(CursorError::Malformed)?
        .parse()
        .map_err(|_| CursorError::Malformed)?;
    let seq: u64 = parts
        .next()
        .ok_or(CursorError::Malformed)?
        .parse()
        .map_err(|_| CursorError::Malformed)?;
    if parts.next().is_some() {
        return Err(CursorError::Malformed);
    }
    let kind = CursorKind::from_tag(kind_tag).ok_or(CursorError::UnknownKind(kind_tag))?;
    Ok(CursorValue {
        kind,
        seq: Seq::new(seq),
    })
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 游标对日志没有价值，只有长度能说明"带没带"
        write!(f, "Cursor(<{} 字节>)", self.0.len())
    }
}

impl Serialize for Cursor {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Cursor {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        let cursor = Self(raw);
        // 边界处就校验：坏游标不该进到业务层才炸
        cursor.decode().map_err(serde::de::Error::custom)?;
        Ok(cursor)
    }
}

/// FNV-1a 的 32 位偏移基数。
const FNV_OFFSET_BASIS: u32 = 0x811c_9dc5;
/// FNV-1a 的 32 位质数。
const FNV_PRIME: u32 = 0x0100_0193;
/// 取高 16 位与低 16 位异或时的右移量。
const FNV_FOLD_SHIFT: u32 = 16;
/// 16 位掩码。
const U16_MASK: u32 = 0xffff;

/// FNV-1a 的 16 位截断：只查"有没有被改坏"，不防伪造（防伪造靠签名，那是
/// 服务端的事，客户端本来就不该相信自己的游标）。
fn checksum(bytes: &[u8]) -> u16 {
    let mut hash: u32 = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    (((hash >> FNV_FOLD_SHIFT) ^ hash) & U16_MASK) as u16
}

/// base64url 字母表（RFC 4648 §5，无填充）。
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
/// 6 位一组。
const B64_GROUP_BITS: u32 = 6;
/// 一组的掩码。
const B64_MASK: u32 = 0x3f;
/// 第 1 个字符取高 6 位。
const B64_SHIFT_1: u32 = 18;
/// 第 2 个字符。
const B64_SHIFT_2: u32 = 12;
/// 第 3 个字符。
const B64_SHIFT_3: u32 = 6;
/// 一个字节 8 位。
const BITS_PER_BYTE: u32 = 8;

fn b64url_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let packed = (u32::from(chunk[0]) << 16) | (b1 << 8) | b2;
        out.push(char::from(
            ALPHABET[(packed >> B64_SHIFT_1) as usize & B64_MASK as usize],
        ));
        out.push(char::from(
            ALPHABET[(packed >> B64_SHIFT_2) as usize & B64_MASK as usize],
        ));
        if chunk.len() > 1 {
            out.push(char::from(
                ALPHABET[(packed >> B64_SHIFT_3) as usize & B64_MASK as usize],
            ));
        }
        if chunk.len() > 2 {
            out.push(char::from(ALPHABET[packed as usize & B64_MASK as usize]));
        }
    }
    out
}

fn b64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        acc = (acc << B64_GROUP_BITS) | u32::from(value);
        bits += B64_GROUP_BITS;
        if bits >= BITS_PER_BYTE {
            bits -= BITS_PER_BYTE;
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_every_kind() {
        for kind in [CursorKind::Message, CursorKind::Event, CursorKind::Sync] {
            let cursor = Cursor::at(kind, Seq::new(42));
            let decoded = cursor.decode().expect("decode");
            assert_eq!(decoded.kind, kind);
            assert_eq!(decoded.seq, Seq::new(42));
        }
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        // RFC 4648 的 base64url 用例（去掉填充）
        assert_eq!(b64url_encode(b""), "");
        assert_eq!(b64url_encode(b"f"), "Zg");
        assert_eq!(b64url_encode(b"ab"), "YWI");
        assert_eq!(b64url_encode(b"foo"), "Zm9v");
        assert_eq!(b64url_decode("Zm9v"), Some(b"foo".to_vec()));
        assert!(b64url_decode("Zm9v!").is_none());
    }

    #[test]
    fn tampering_is_detected() {
        let cursor = Cursor::at(CursorKind::Event, Seq::new(7));
        let mut broken: Vec<char> = cursor.as_str().chars().collect();
        broken[0] = if broken[0] == 'A' { 'B' } else { 'A' };
        let broken: String = broken.into_iter().collect();
        let err = Cursor(broken).decode().expect_err("tampered");
        assert!(matches!(
            err,
            CursorError::Checksum | CursorError::Malformed | CursorError::UnsupportedVersion(_)
        ));
    }

    #[test]
    fn unknown_kind_is_reported_not_guessed() {
        let raw = format!("{CURSOR_VERSION}:9:1");
        let tag = checksum(raw.as_bytes());
        let cursor = Cursor(format!("{}.{tag:04x}", b64url_encode(raw.as_bytes())));
        assert_eq!(cursor.decode(), Err(CursorError::UnknownKind(9)));
    }

    #[test]
    fn deserialize_rejects_garbage() {
        assert!(serde_json::from_str::<Cursor>("\"nonsense\"").is_err());
        let ok = Cursor::at(CursorKind::Message, Seq::new(3));
        let text = serde_json::to_string(&ok).expect("ser");
        assert_eq!(serde_json::from_str::<Cursor>(&text).expect("de"), ok);
    }
}
