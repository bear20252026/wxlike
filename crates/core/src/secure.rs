//! 常量时间比较：密码学与令牌校验的唯一实现。
//!
//! 仓库里曾有 9 份同名函数，算法分两种：一种遇到长度不同立即返回，另一种
//! 跑满较长的一侧。前者对长度差异的耗时是可观察的，后者用 `u8` 存长度差，
//! 长度相差 256 的倍数时会被截成 0。这里取后者的形状、把累加器改成 `usize`，
//! 既不在长度上提前返回，也不会因截断把"差 256 字节"误判成相等。

/// 长度不同也走完全程；返回值只在所有字节与长度都一致时为 `true`。
#[must_use]
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut diff = left.len() ^ right.len();
    let n = left.len().max(right.len());
    for i in 0..n {
        let x = left.get(i).copied().unwrap_or(0);
        let y = right.get(i).copied().unwrap_or(0);
        diff |= usize::from(x ^ y);
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn equal_and_unequal() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn length_difference_of_256_is_not_masked() {
        let zeros = [0u8; 256];
        assert!(!constant_time_eq(&[], &zeros));
        assert!(!constant_time_eq(b"", &zeros));
    }
}
