//! 共享密钥的比较方式。
//!
//! 单独一个模块是因为它被两处用：E1 的名单 token、E2 的推流凭据。写在任何一边的
//! 私有函数里，另一处就只能复制一份——两份常数时间比较必然有一份会退化。
//! 形状照搬 `crates/platform/src/live.rs` 的同名私有函数（同仓库）。

/// 长度不同也走完全程的比较（不按字节提前返回）。
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u8;
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}
