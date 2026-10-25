/// 订单/报价 id；使用 128 位随机数，避免可枚举的连续编号。
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{:032x}", rand::random::<u128>())
}
