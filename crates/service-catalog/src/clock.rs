use std::time::{SystemTime, UNIX_EPOCH};

/// 订单与报价的时间源；生产只通过该 trait 读时间，测试可注入固定时钟。
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as i64)
    }
}
