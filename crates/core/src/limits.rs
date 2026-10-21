//! 跨 crate 的数值约定：**按含义命名，不按值命名**。
//!
//! 审计里最刺眼的一条是"同一个 `64` 有三种含义"：sha256 的十六进制长度、
//! 贴纸标签的字数上限、签名数组的长度。它们碰巧都等于 64，于是复制粘贴时
//! 谁也不会觉得不对——直到有人把 hash 换成 sha512，改了其中一处，另外两处
//! 跟着"变得看起来对"，而真正该改的那处没改。
//!
//! 这里只放**跨 crate 共用**的约定。只在一个模块里用到的上限，就留在那个模块
//! 里当 `const`，不要搬进来——这个模块变成"什么数字都往里塞"的垃圾桶，
//! 比魔法数字更糟。

/// sha256 的十六进制表示长度（`media_id` / `video_id` / `sha256` 字段）。
///
/// 32 字节 → 64 个十六进制字符。这个值一旦变化，所有"id 长度必须等于 N"
/// 的校验都要跟着变，所以它只能有一个定义。
pub const SHA256_HEX_LEN: usize = 64;

/// 列表/历史的默认分页大小。
///
/// 50 是"一屏半"的经验值：太小要频繁翻页，太大一次序列化就变重。
pub const DEFAULT_PAGE_LIMIT: usize = 50;

/// 搜索的默认条数。比分页小——搜索是"找到那一条"，不是"翻一页看看"。
pub const DEFAULT_SEARCH_LIMIT: usize = 20;

/// 对称密钥/随机标识的字节数（32 字节 = 256 位）。
pub const KEY_BYTES: usize = 32;

#[cfg(test)]
mod tests {
    use super::*;

    /// 这条测试不是凑数：它把"64 是 sha256 的 hex 长度"钉在真实计算上。
    /// 有人把常量改成 32（误以为是字节数）时，这里会红。
    #[test]
    fn sha256_hex_len_matches_the_real_digest() {
        use sha2::Digest as _;
        let digest = sha2::Sha256::digest(b"wxlike");
        assert_eq!(digest.len() * 2, SHA256_HEX_LEN);
    }

    #[test]
    fn key_bytes_is_a_full_256_bit_key() {
        assert_eq!(KEY_BYTES * 8, 256);
    }
}
