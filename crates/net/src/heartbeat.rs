//! 自适应保活：按网络质量调心跳间隔，按网络指纹持久化。
//!
//! 参照 `refs/mars/mars/stn/src/smart_heartbeat.{h,cc}` + `stn/config.h`（腾讯 Mars，MIT）。
//! 它解决的问题是：**固定心跳在移动网络上是错的**——WiFi 下 30 秒一次是白耗电，
//! 弱网下 3 分钟一次又会让"对方掉线"发现得太晚。
//!
//! # 与 Mars 的偏离（重要）
//!
//! Mars 的心跳区间是 **3.5–10 分钟**，因为它是 TCP 长连 + 应用层心跳：TCP 有自己的
//! keepalive 选项，长间隔只影响"多久发现掉线"。QUIC 没有等价选项，保活完全靠 PING
//! 帧，而 `max_idle_timeout` 才是判定死亡的依据。所以照抄 10 分钟会得到：
//! 手机放兜里 10 分钟，服务端才知道它掉线——消息推送全堵在这条链路上。
//!
//! 我们改成在 **15–60 秒**内自适应，`idle = 3 × interval`（夹在 45–180 秒）：
//! 既能让稳定网络省电（60 秒一次，约 1 包/分钟），又能在弱网下 15 秒探一次。
//! 区间形状（最短/最长/步长/收敛条件/失败回落）照搬 Mars，数值按 QUIC 重定。
//!
//! # 为什么要"按网络指纹"存
//!
//! 同一个 WiFi 下的稳定值，换到 4G 上完全不适用。Mars 按 `net_detail_`（SSID /
//! 运营商）各存一份，回到旧网络直接复用上次的收敛结果，不用重新试探。这一条是
//! 它最值钱的设计，也是最容易被漏掉的——见 [`HeartbeatStore`]。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 最短间隔：弱网下 15 秒探一次。
pub const MIN_INTERVAL_MS: u64 = 15_000;
/// 最长间隔：稳定网络 60 秒一次，约 1 包/分钟。
pub const MAX_INTERVAL_MS: u64 = 60_000;
/// 稳定后每次往上加多少（Mars 的 `HeartStep` 是 60 秒，我们是 5 秒）。
pub const STEP_MS: u64 = 5_000;
/// 连续多少次成功算"这个间隔稳了"（Mars 的 `BaseSuccCount` 是 5）。
pub const STABLE_SUCCESSES: u32 = 4;
/// 连续多少次失败回落到最短（Mars 的 `MaxHeartFailCount` 是 2）。
pub const MAX_FAILURES: u32 = 2;
/// RTT 超过这个值就算"弱网"：不加长间隔（Mars 用最小速率 12/4 KB/s 判断，
/// 我们用更直接的往返时延）。
pub const WEAK_RTT_MS: u64 = 800;

/// 网络类型。移动端由平台 API 提供（Android `ConnectivityManager` /
/// iOS `NWPathMonitor`），桌面端目前只能给 `Unknown`——见 [`NetProfile::from_env`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetKind {
    Wifi,
    Cellular,
    Ethernet,
    Unknown,
}

impl NetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Cellular => "cellular",
            Self::Ethernet => "ethernet",
            Self::Unknown => "unknown",
        }
    }

    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "wifi" | "wlan" => Self::Wifi,
            "cellular" | "4g" | "5g" | "mobile" => Self::Cellular,
            "ethernet" | "lan" | "wired" => Self::Ethernet,
            _ => Self::Unknown,
        }
    }

    /// 初始间隔：弱网先按最短试，稳定网络可以从中间起步。
    fn initial_interval_ms(self) -> u64 {
        match self {
            // 蜂窝网络先保守：NAT 超时短、切换频繁
            Self::Cellular => MIN_INTERVAL_MS,
            Self::Wifi | Self::Ethernet => MIN_INTERVAL_MS + STEP_MS * 2,
            Self::Unknown => MIN_INTERVAL_MS,
        }
    }
}

/// 网络指纹：类型 + 一个更细的标识（SSID / 运营商 / 接口名）。
///
/// 指纹只用来**分组存取**收敛结果，不参与任何鉴权，所以不需要脱敏。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetProfile {
    pub kind: NetKind,
    /// 细分标识。拿不到就留空——同一类型的所有网络共用一份收敛结果。
    pub detail: String,
}

impl NetProfile {
    pub fn new(kind: NetKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    pub fn unknown() -> Self {
        Self::new(NetKind::Unknown, "")
    }

    /// 持久化用的键：`wifi:home`、`cellular:`。
    pub fn fingerprint(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.detail.trim())
    }

    /// 从环境变量读（`WXLIKE_NET_KIND` / `WXLIKE_NET_DETAIL`）。
    ///
    /// 桌面端没有可移植的"当前是 WiFi 还是 4G"API（要读路由表/NetworkManager，
    /// 每个平台一套）。所以这里只给一个**显式入口**：部署脚本或移动端绑定可以设，
    /// 不设就是 `unknown`（按最保守的策略跑）。宁可保守，也不要猜错——
    /// 猜成 WiFi 会让蜂窝网络上的心跳变长，直接表现为"掉线发现慢"。
    pub fn from_env() -> Self {
        let kind = std::env::var("WXLIKE_NET_KIND")
            .map(|v| NetKind::parse(&v))
            .unwrap_or(NetKind::Unknown);
        let detail = std::env::var("WXLIKE_NET_DETAIL").unwrap_or_default();
        Self { kind, detail }
    }
}

/// 一条心跳的观测结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartSample {
    /// 这一轮是否成功（PING 有回应 / 连接仍活着）
    pub ok: bool,
    /// 往返时延；拿不到就传 0
    pub rtt_ms: u64,
    /// 是不是**超时**导致的失败。Mars 只在超时时清零成功计数——
    /// 主动断开（切网、用户退出）不该被当成"这个间隔不行"。
    pub timeout: bool,
}

impl HeartSample {
    pub fn ok(rtt_ms: u64) -> Self {
        Self {
            ok: true,
            rtt_ms,
            timeout: false,
        }
    }

    pub fn failed(timeout: bool) -> Self {
        Self {
            ok: false,
            rtt_ms: 0,
            timeout,
        }
    }
}

/// 自适应心跳状态机。纯逻辑，没有 IO——所有时间都由调用方喂进来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatPolicy {
    kind: NetKind,
    interval_ms: u64,
    /// 当前间隔是否已收敛（不再试探）
    stable: bool,
    succ_streak: u32,
    fail_streak: u32,
}

impl HeartbeatPolicy {
    pub fn new(kind: NetKind) -> Self {
        Self {
            kind,
            interval_ms: kind.initial_interval_ms(),
            stable: false,
            succ_streak: 0,
            fail_streak: 0,
        }
    }

    /// 从持久化的值恢复（上次在这个网络上的收敛结果）。
    pub fn restored(kind: NetKind, interval_ms: u64, stable: bool) -> Self {
        Self {
            kind,
            interval_ms: interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS),
            stable,
            succ_streak: 0,
            fail_streak: 0,
        }
    }

    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }

    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms)
    }

    pub fn is_stable(&self) -> bool {
        self.stable
    }

    /// 空闲判定上限。**必须大于间隔**，否则连接会在两次心跳之间被判死。
    ///
    /// 3 倍是给"单个 PING 丢了"留的余量；上限 180 秒是为了不让"对方真掉线"
    /// 的发现时间拖过三分钟。
    pub fn idle_timeout(&self) -> Duration {
        Duration::from_millis((self.interval_ms * 3).clamp(45_000, 180_000))
    }

    /// 喂一条结果，返回下一次该用的间隔。
    pub fn on_sample(&mut self, sample: HeartSample) -> u64 {
        if sample.ok {
            self.fail_streak = 0;
            self.succ_streak += 1;
            // 弱网（RTT 高）不加长：这时候"省电"是次要的，及时性才是
            let weak = sample.rtt_ms >= WEAK_RTT_MS;
            if self.succ_streak >= STABLE_SUCCESSES && !weak {
                self.succ_streak = 0;
                self.stable = true;
                // 收敛后仍可以再往上探一档，直到上限
                self.interval_ms = (self.interval_ms + STEP_MS).min(MAX_INTERVAL_MS);
            }
            return self.interval_ms;
        }

        // 失败：成功计数清零；只有**超时**才把"稳定"打回 false
        self.succ_streak = 0;
        self.fail_streak += 1;
        if sample.timeout {
            self.stable = false;
        }
        if self.fail_streak >= MAX_FAILURES {
            self.fail_streak = 0;
            self.stable = false;
            self.interval_ms = MIN_INTERVAL_MS;
        } else {
            // 单次失败先降一档，别等到连续两次才反应
            self.interval_ms = self
                .interval_ms
                .saturating_sub(STEP_MS)
                .max(MIN_INTERVAL_MS);
        }
        self.interval_ms
    }
}

/// 一个网络的收敛结果（持久化单元）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedHeartbeat {
    pub interval_ms: u64,
    pub stable: bool,
    pub updated_at_ms: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HeartbeatFile {
    /// 指纹 → 收敛结果
    profiles: std::collections::BTreeMap<String, SavedHeartbeat>,
}

/// 按网络指纹存取收敛结果。
///
/// 写盘时机由调用方决定：只在"间隔变了"时写，不是每次心跳都写。
/// 文件很小（每个网络一行），Mars 用 INI 且专门限了大小，原因相同。
pub struct HeartbeatStore {
    path: Option<PathBuf>,
}

impl HeartbeatStore {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }

    /// 内存版（测试用）。
    pub fn memory() -> Self {
        Self { path: None }
    }

    pub fn load(&self, profile: &NetProfile) -> Option<SavedHeartbeat> {
        let file = self.read();
        file.profiles.get(&profile.fingerprint()).copied()
    }

    pub fn save(&self, profile: &NetProfile, interval_ms: u64, stable: bool, now_ms: i64) {
        let Some(path) = &self.path else {
            return;
        };
        let mut file = self.read();
        file.profiles.insert(
            profile.fingerprint(),
            SavedHeartbeat {
                interval_ms,
                stable,
                updated_at_ms: now_ms,
            },
        );
        // 坏文件不该让进程起不来：写失败只记日志
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_vec_pretty(&file) {
            Ok(bytes) => {
                if let Err(err) = std::fs::write(path, bytes) {
                    tracing::warn!("写心跳收敛结果失败 {}: {err}", path.display());
                }
            }
            Err(err) => tracing::warn!("序列化心跳收敛结果失败: {err}"),
        }
    }

    fn read(&self) -> HeartbeatFile {
        let Some(path) = &self.path else {
            return HeartbeatFile::default();
        };
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
}

/// 进程级心跳状态：一个进程只有一条网络，所以是单例。
///
/// Mars 的 `SmartHeartbeat` 也是单例（`Singleton` 宏）。这里用 `OnceLock`
/// 明确表达"进程内唯一"，并且**允许在连接前初始化**：
///
/// ```ignore
/// wxlike_net::heartbeat::init(NetProfile::from_env(), HeartbeatStore::new(Some(dir)));
/// ```
pub struct Heartbeat {
    profile: NetProfile,
    store: HeartbeatStore,
    policy: Mutex<HeartbeatPolicy>,
}

static GLOBAL: OnceLock<Heartbeat> = OnceLock::new();

impl Heartbeat {
    /// 初始化（重复调用只生效第一次）。返回是否**本次**初始化成功。
    pub fn init(profile: NetProfile, store: HeartbeatStore) -> bool {
        GLOBAL
            .set(Self {
                policy: Mutex::new(match store.load(&profile) {
                    Some(saved) => {
                        HeartbeatPolicy::restored(profile.kind, saved.interval_ms, saved.stable)
                    }
                    None => HeartbeatPolicy::new(profile.kind),
                }),
                profile,
                store,
            })
            .is_ok()
    }

    /// 没初始化过就用 `unknown` 策略（安全默认：最短间隔起步）。
    pub fn current() -> &'static Self {
        GLOBAL.get_or_init(|| Self {
            profile: NetProfile::unknown(),
            store: HeartbeatStore::memory(),
            policy: Mutex::new(HeartbeatPolicy::new(NetKind::Unknown)),
        })
    }

    pub fn interval(&self) -> Duration {
        self.lock().interval()
    }

    pub fn idle_timeout(&self) -> Duration {
        self.lock().idle_timeout()
    }

    pub fn profile(&self) -> &NetProfile {
        &self.profile
    }

    /// 喂一条观测；间隔变化时落盘。
    pub fn record(&self, sample: HeartSample) -> Duration {
        let (before, after) = {
            let mut policy = self.lock();
            let before = policy.interval_ms();
            policy.on_sample(sample);
            (before, policy.interval_ms())
        };
        if before != after {
            let policy = self.lock();
            self.store.save(
                &self.profile,
                after,
                policy.is_stable(),
                wxlike_core::types::now_ms() as i64,
            );
        }
        Duration::from_millis(after)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HeartbeatPolicy> {
        self.policy.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 便捷入口：喂一条观测给进程级策略。
pub fn record(sample: HeartSample) -> Duration {
    Heartbeat::current().record(sample)
}

/// 便捷入口：给 `transport_config` 用。
pub fn keepalive() -> (Duration, Duration) {
    let hb = Heartbeat::current();
    (hb.interval(), hb.idle_timeout())
}

/// 默认落盘位置：`<dir>/heartbeat.json`。传 `None` 就只在内存里。
pub fn store_in(dir: Option<&Path>) -> HeartbeatStore {
    match dir {
        Some(d) => HeartbeatStore::new(Some(d.join("heartbeat.json"))),
        None => HeartbeatStore::memory(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(rtt: u64) -> HeartSample {
        HeartSample::ok(rtt)
    }

    #[test]
    fn stable_network_grows_then_caps() {
        let mut p = HeartbeatPolicy::new(NetKind::Wifi);
        let start = p.interval_ms();
        // 每 STABLE_SUCCESSES 次成功长一档
        for _ in 0..STABLE_SUCCESSES {
            p.on_sample(ok(30));
        }
        assert_eq!(p.interval_ms(), start + STEP_MS);
        assert!(p.is_stable());

        for _ in 0..200 {
            p.on_sample(ok(30));
        }
        assert_eq!(p.interval_ms(), MAX_INTERVAL_MS, "封顶 60 秒");
    }

    #[test]
    fn high_rtt_does_not_grow_the_interval() {
        let mut p = HeartbeatPolicy::new(NetKind::Cellular);
        let start = p.interval_ms();
        for _ in 0..20 {
            p.on_sample(ok(WEAK_RTT_MS + 1));
        }
        assert_eq!(
            p.interval_ms(),
            start,
            "弱网（RTT ≥ {}ms）不该为了省电把心跳拉长",
            WEAK_RTT_MS
        );
        assert!(!p.is_stable());
    }

    #[test]
    fn two_failures_fall_back_to_the_floor() {
        let mut p = HeartbeatPolicy::new(NetKind::Wifi);
        for _ in 0..50 {
            p.on_sample(ok(30));
        }
        assert_eq!(p.interval_ms(), MAX_INTERVAL_MS);

        p.on_sample(HeartSample::failed(true));
        assert!(p.interval_ms() < MAX_INTERVAL_MS, "单次失败先降一档");
        p.on_sample(HeartSample::failed(true));
        assert_eq!(p.interval_ms(), MIN_INTERVAL_MS, "连续两次失败回最短");
        assert!(!p.is_stable());
    }

    /// Mars 的细节：只有**超时**才把"稳定"打回 false。
    /// 主动断开（切网、用户退出）不该被当成"这个间隔不行"。
    #[test]
    fn a_non_timeout_failure_keeps_the_stable_flag() {
        let mut p = HeartbeatPolicy::new(NetKind::Wifi);
        for _ in 0..STABLE_SUCCESSES {
            p.on_sample(ok(30));
        }
        assert!(p.is_stable());
        p.on_sample(HeartSample::failed(false));
        assert!(p.is_stable(), "非超时失败不清 stable");
        assert!(p.interval_ms() >= MIN_INTERVAL_MS);
    }

    #[test]
    fn idle_timeout_always_exceeds_the_interval() {
        for kind in [NetKind::Wifi, NetKind::Cellular, NetKind::Unknown] {
            let mut p = HeartbeatPolicy::new(kind);
            for _ in 0..100 {
                p.on_sample(ok(20));
                assert!(
                    p.idle_timeout() > p.interval(),
                    "idle 必须大于间隔，否则两次心跳之间就被判死"
                );
                assert!(p.idle_timeout() <= Duration::from_secs(180));
            }
        }
    }

    #[test]
    fn restored_values_are_clamped_to_the_supported_range() {
        let p = HeartbeatPolicy::restored(NetKind::Wifi, 9_999_999, true);
        assert_eq!(
            p.interval_ms(),
            MAX_INTERVAL_MS,
            "坏值不能把心跳拉到 2 小时"
        );
        let p = HeartbeatPolicy::restored(NetKind::Wifi, 1, true);
        assert_eq!(p.interval_ms(), MIN_INTERVAL_MS);
    }

    /// 指纹隔离：WiFi 的收敛结果不能套到 4G 上。
    #[test]
    fn profiles_do_not_share_their_converged_value() {
        let dir = std::env::temp_dir().join(format!("wxlike-hb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = HeartbeatStore::new(Some(dir.join("heartbeat.json")));

        let wifi = NetProfile::new(NetKind::Wifi, "home");
        let cell = NetProfile::new(NetKind::Cellular, "");
        store.save(&wifi, 45_000, true, 1);

        assert_eq!(store.load(&wifi).map(|s| s.interval_ms), Some(45_000));
        assert_eq!(store.load(&cell), None, "换网络必须重新试探");

        // 重新打开（模拟重启）后仍然读得到
        let reopened = HeartbeatStore::new(Some(dir.join("heartbeat.json")));
        assert_eq!(reopened.load(&wifi).map(|s| s.stable), Some(true));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_parses_the_kind_and_keeps_unknown_as_the_safe_default() {
        // 不直接改进程环境（测试并行会互相干扰），只验解析
        assert_eq!(NetKind::parse("WiFi"), NetKind::Wifi);
        assert_eq!(NetKind::parse("4g"), NetKind::Cellular);
        assert_eq!(NetKind::parse("ethernet"), NetKind::Ethernet);
        assert_eq!(NetKind::parse("something-else"), NetKind::Unknown);
        assert_eq!(NetProfile::unknown().fingerprint(), "unknown:");
    }
}
