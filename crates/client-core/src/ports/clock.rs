//! 时间端口。
//!
//! 核心需要"现在几点"和"睡一会"两件事，两者都必须可注入：
//! 退避重试的测试要是真等 30 秒，就没人会写它了；而用真实时间又会让
//! "杀进程"这类用例变得不可复现。

use super::MaybeSend;

/// 时间与睡眠。
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait Clock: MaybeSend {
    /// 当前 unix 毫秒。
    fn now_ms(&self) -> i64;

    /// 睡 `ms` 毫秒。实现要能被运行时丢弃（不阻塞退出）。
    async fn sleep(&self, ms: u64);
}
