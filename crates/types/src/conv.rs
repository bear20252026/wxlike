//! 会话 id 的构造与解析。
//!
//! 直聊会话 id 是 `d-<小>~<大>`：两个人无论谁发起，都必须落到**同一个**
//! 会话上。算法以前只在 `crates/client` 里有一份，移动端照着抄了一遍，
//! 桌面端又抄了一遍——抄错就是"同一个人两个会话"。现在这里是唯一一份，
//! `crates/types/tests/parity_with_legacy.rs` 钉住它与旧实现逐字一致。

use crate::ids::{ConvId, IdError, UserId};

/// 会话种类。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConvKind {
    /// 两人直聊。
    Direct,
    /// 群 / 频道 / 社区主题。
    Group,
}

/// 解析后的会话。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ParsedConv {
    /// 直聊：两个参与者，按 id 升序。
    Direct {
        /// 较小的用户 id。
        left: UserId,
        /// 较大的用户 id。
        right: UserId,
    },
    /// 群：原样带回 id。
    Group {
        /// 群 id。
        id: ConvId,
    },
}

impl ParsedConv {
    /// 种类。
    #[must_use]
    pub fn kind(&self) -> ConvKind {
        match self {
            ParsedConv::Direct { .. } => ConvKind::Direct,
            ParsedConv::Group { .. } => ConvKind::Group,
        }
    }
}

/// 直聊会话 id 的算法（**唯一实现**）。
///
/// 排序是为了让 `(a, b)` 与 `(b, a)` 得到同一个 id；`d-` 前缀让它不可能与
/// 服务端生成的群 id 撞车。
#[must_use]
pub fn direct_conv_id(a: &str, b: &str) -> String {
    let (mut x, mut y) = (a.to_string(), b.to_string());
    if x > y {
        std::mem::swap(&mut x, &mut y);
    }
    format!("d-{x}~{y}")
}

/// 类型化的直聊 id 构造。
///
/// # Errors
/// 拼出来的 id 不合规时返回 [`IdError`]（例如两侧用户 id 过长）。
pub fn direct_conv(a: &UserId, b: &UserId) -> Result<ConvId, IdError> {
    ConvId::parse(direct_conv_id(a.as_str(), b.as_str()))
}

/// 解析会话 id。
///
/// `d-` 前缀 + 恰好一个 `~` 才算直聊；其余一律当群。
///
/// # Errors
/// 直聊形态但两侧用户 id 不合规时返回 [`IdError`]。
pub fn parse_conv(conv: &ConvId) -> Result<ParsedConv, IdError> {
    let raw = conv.as_str();
    let Some(rest) = raw.strip_prefix("d-") else {
        return Ok(ParsedConv::Group { id: conv.clone() });
    };
    let Some((left, right)) = rest.split_once('~') else {
        return Ok(ParsedConv::Group { id: conv.clone() });
    };
    if right.contains('~') {
        // 多个分隔符：不是直聊 id，按群处理（服务端不该生成这种，但别崩）
        return Ok(ParsedConv::Group { id: conv.clone() });
    }
    Ok(ParsedConv::Direct {
        left: UserId::parse(left)?,
        right: UserId::parse(right)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_does_not_matter() {
        assert_eq!(direct_conv_id("alice", "bob"), "d-alice~bob");
        assert_eq!(direct_conv_id("bob", "alice"), "d-alice~bob");
    }

    #[test]
    fn parse_round_trips() {
        let conv = ConvId::parse(direct_conv_id("alice", "bob")).expect("conv");
        let parsed = parse_conv(&conv).expect("parse");
        assert_eq!(parsed.kind(), ConvKind::Direct);
        match parsed {
            ParsedConv::Direct { left, right } => {
                assert_eq!(left.as_str(), "alice");
                assert_eq!(right.as_str(), "bob");
            }
            ParsedConv::Group { .. } => panic!("应当是直聊"),
        }
    }

    #[test]
    fn group_ids_stay_groups() {
        let conv = ConvId::parse("0c0ffee01").expect("conv");
        assert_eq!(parse_conv(&conv).expect("parse").kind(), ConvKind::Group);
        // `d-` 开头但没有分隔符：仍是群，不报错
        let odd = ConvId::parse("d-nope").expect("conv");
        assert_eq!(parse_conv(&odd).expect("parse").kind(), ConvKind::Group);
    }
}
