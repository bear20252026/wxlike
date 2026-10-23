//! Redis 上的 GCRA（Generic Cell Rate Algorithm）风控后端。
//!
//! 每个 `(action, key)` 一个 Hash，里面按"窗口:阈值"存两格 TAT（理论到达
//! 时间）：`throttle` 与 `deny` 各一格。判定与推进由一段 Lua 脚本原子完成，
//! 所以多个服务进程看到的是**同一份**计数——这正是单进程滑动窗口做不到的。
//!
//! 语义取舍：
//! * **降速档是标准 GCRA**：窗口内允许 `throttle_at` 次，第 `throttle_at + 1`
//!   次降速；被拒的请求不推进 TAT（admission control，重试不惩罚自己）；
//! * **拒绝档是硬计数**：第 `deny_at` 次尝试即拒，与进程内实现的
//!   `n >= deny_at` 对齐；被降速的尝试**照样计入**拒绝档，否则
//!   `deny_at > throttle_at` 时永远到不了 Deny（降速先把路堵死了）；
//! * 已经被 Deny 的尝试不推进任何 TAT，让惩罚按窗口自然消退；
//! * 默认用调用方传入的 `now_ms`（引擎"时间由调用方注入"的可测性设计），
//!   [`Clock::Redis`] 改用 Redis 的 `TIME`，避免多机器时钟漂移导致误判。
//!
//! 与进程内实现的差别只在降速档的边界：内存版"第 N 次即标记"，GCRA 是
//! "前 N 次放行、第 N+1 次降速"——限流语义是"窗口内允许 N 次"，这是
//! GCRA 的标准口径。
//!
//! 需要 `redis` feature：`cargo test -p wxlike-risk --features redis`。

use std::sync::Mutex;

use redis::Connection;

use crate::store::{RiskStore, Rule, StoreVerdict};
use crate::{Action, Result, RiskError};

/// 时间来源。默认 `Caller`（测试可注入任意时钟）；多节点部署建议 `Redis`
/// 以消除机器间时钟漂移。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    Caller,
    Redis,
}

/// GCRA 后端。持有一条阻塞连接；方法内部串行化，Redis 端的脚本仍是原子的。
pub struct GcraStore {
    conn: Mutex<Connection>,
    clock: Clock,
    namespace: String,
}

impl GcraStore {
    /// 默认命名空间 `wxlike:risk`，默认使用调用方时间。
    pub fn connect(url: &str) -> Result<Self> {
        Self::connect_with_namespace(url, "wxlike:risk")
    }

    /// 测试用独立命名空间，避免并行用例互相污染。
    pub fn connect_with_namespace(url: &str, namespace: &str) -> Result<Self> {
        let client = redis::Client::open(url).map_err(store_err)?;
        let conn = client.get_connection().map_err(store_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
            clock: Clock::Caller,
            namespace: namespace.to_string(),
        })
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    fn key(&self, action: Action, key: &str) -> String {
        format!("{}:{}:{}", self.namespace, action.as_str(), key)
    }

    /// 返回 `(level, retry_after_ms, used)`；level 0=allow 1=throttle 2=deny。
    fn run(
        &self,
        mode: u64,
        action: Action,
        key: &str,
        now_ms: u64,
        rule: &Rule,
    ) -> Result<(i64, i64, i64)> {
        let mut conn = self.conn.lock().map_err(|_| RiskError::Poisoned)?;
        let now_arg: i64 = match self.clock {
            Clock::Caller => now_ms as i64,
            Clock::Redis => -1,
        };
        let script = redis::Script::new(GCRA_LUA);
        script
            .key(self.key(action, key))
            .arg(mode)
            .arg(rule.window_ms)
            .arg(rule.throttle_at as u64)
            .arg(rule.deny_at as u64)
            .arg(now_arg);
        script.invoke(&mut *conn).map_err(store_err)
    }
}

fn verdict(raw: (i64, i64, i64)) -> StoreVerdict {
    let (level, retry_after_ms, _) = raw;
    let retry_after_ms = u64::try_from(retry_after_ms).unwrap_or(0);
    match level {
        2 => StoreVerdict::Deny { retry_after_ms },
        1 => StoreVerdict::Throttle { retry_after_ms },
        _ => StoreVerdict::Allow,
    }
}

fn store_err(err: redis::RedisError) -> RiskError {
    RiskError::Store(err.to_string())
}

impl RiskStore for GcraStore {
    fn check(&self, action: Action, key: &str, now_ms: u64, rule: &Rule) -> Result<StoreVerdict> {
        let raw = self.run(0, action, key, now_ms, rule)?;
        Ok(verdict(raw))
    }

    fn record(&self, action: Action, key: &str, now_ms: u64, rule: &Rule) -> Result<StoreVerdict> {
        let raw = self.run(1, action, key, now_ms, rule)?;
        Ok(verdict(raw))
    }

    fn clear(&self, action: Action, key: &str) -> Result<()> {
        let mut conn = self.conn.lock().map_err(|_| RiskError::Poisoned)?;
        redis::cmd("DEL")
            .arg(self.key(action, key))
            .query::<i64>(&mut *conn)
            .map_err(store_err)?;
        Ok(())
    }

    fn count(&self, action: Action, key: &str, now_ms: u64, rule: &Rule) -> Result<usize> {
        let raw = self.run(2, action, key, now_ms, rule)?;
        Ok(usize::try_from(raw.2).unwrap_or(0))
    }
}

/// KEYS[1] = Hash 键；ARGV = mode, window_ms, throttle_at, deny_at, now_ms。
///
/// mode: 0=check（不写）、1=record（放行才写）、2=count（不写）。
/// now_ms < 0 时用 Redis `TIME`；否则用调用方注入的时间。
///
/// 时间单位统一为毫秒；TAT 用 `%.17g` 存，避免多档小数的累积误差。
const GCRA_LUA: &str = r#"
local key = KEYS[1]
local mode = tonumber(ARGV[1])
local window = tonumber(ARGV[2])
local throttle_at = tonumber(ARGV[3])
local deny_at = tonumber(ARGV[4])
local now = tonumber(ARGV[5])

if now < 0 then
    local t = redis.call('TIME')
    now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
end

local deny_field = 'deny:' .. window .. ':' .. deny_at
local throttle_field = 'throttle:' .. window .. ':' .. throttle_at

local deny_tat = tonumber(redis.call('HGET', key, deny_field)) or now
local throttle_tat = tonumber(redis.call('HGET', key, throttle_field)) or now
if deny_tat < now then deny_tat = now end
if throttle_tat < now then throttle_tat = now end

local new_deny = deny_tat + window / deny_at
local new_throttle = throttle_tat + window / throttle_at
local level = 0
local retry = 0

-- 拒绝档是硬计数（第 deny_at 次尝试即拒）；降速档是标准 GCRA
-- （窗口内允许 throttle_at 次，第 throttle_at+1 次降速）。
if new_deny >= now + window then
    level = 2
    -- 拒绝期还要等降速桶一起排空，否则 Enforce 解除后立刻又被降速。
    retry = math.max(new_deny - now - window, new_throttle - now - window)
elseif new_throttle > now + window then
    level = 1
    retry = new_throttle - now - window
end
if retry < 1 then retry = 1 end

if mode == 1 then
    if level == 2 then
        -- 拒绝的尝试不推进任何 TAT：惩罚按窗口自然消退。
    elseif level == 1 then
        -- 降速的尝试照样计入拒绝档，反复硬闯最终升级为 Deny。
        redis.call('HSET', key, deny_field, string.format('%.17g', new_deny))
        local ttl = math.max(new_deny, throttle_tat) - now
        if ttl > 0 then
            redis.call('PEXPIRE', key, math.ceil(ttl) + 1000)
        end
    else
        redis.call('HSET', key,
            deny_field, string.format('%.17g', new_deny),
            throttle_field, string.format('%.17g', new_throttle))
        local ttl = math.max(new_deny, new_throttle) - now
        if ttl > 0 then
            redis.call('PEXPIRE', key, math.ceil(ttl) + 1000)
        end
    end
end

-- 窗口内已消耗的尝试数（漏桶水平）：check/count 读已存状态，
-- record 时只有被 Deny 的尝试不算。
local used_tat = deny_tat
if mode == 1 and level ~= 2 then
    used_tat = new_deny
end
local used = math.floor((used_tat - now) * deny_at / window)
if used < 0 then used = 0 end
if used > deny_at then used = deny_at end

return {level, math.ceil(retry), used}
"#;
