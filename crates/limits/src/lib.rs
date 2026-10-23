//! 跨 crate 的数值约定与**风控阈值配置**：按含义命名，不按值命名。
//!
//! 审计里最刺眼的一条是"同一个 `64` 有三种含义"：sha256 的十六进制长度、
//! 贴纸标签的字数上限、签名数组的长度。它们碰巧都等于 64，于是复制粘贴时
//! 谁也不会觉得不对——直到有人把 hash 换成 sha512，改了其中一处，另外两处
//! 跟着"变得看起来对"，而真正该改的那处没改。
//!
//! # 为什么单独成一个 crate
//!
//! 这些常量本该放在 `wxlike-core`。但 `core` 带着 `rusqlite`（bundled SQLite）
//! 与 `vodozemac`，而 `wxlike-client`（CLI / MCP 的底座）只需要两个分页默认值——
//! 为了两个数字把 SQLite 编进命令行工具，是拿构建时间换"看起来更内聚"。
//! 所以拆出一个**零依赖**的 crate，谁需要谁依赖，`core` 再 `pub use` 回去，
//! 老的 `wxlike_core::limits::...` 路径仍然可用。
//!
//! 这里只放**跨 crate 共用**的约定。只在一个模块里用到的上限，就留在那个模块
//! 里当 `const`——这个模块变成"什么数字都往里塞"的垃圾桶，比魔法数字更糟。

/// sha256 的十六进制表示长度（`media_id` / `video_id` / `sha256` 字段）。
///
/// 32 字节 → 64 个十六进制字符。这个值一旦变化，所有"id 长度必须等于 N"
/// 的校验都要跟着变，所以它只能有一个定义。
pub const SHA256_HEX_LEN: usize = 64;

/// 列表/历史的默认分页大小。
///
/// 50 是"一屏半"的经验值：太小要频繁翻页，太大一次序列化就变重。
pub const DEFAULT_PAGE_LIMIT: u64 = 50;

/// 搜索的默认条数。比分页小——搜索是"找到那一条"，不是"翻一页看看"。
pub const DEFAULT_SEARCH_LIMIT: u64 = 20;

/// 对称密钥/随机标识的字节数（32 字节 = 256 位）。
pub const KEY_BYTES: usize = 32;

// 这三条用**编译期断言**而不是测试：条件全是常量，写成 `#[test]` 只会在运行时
// 做一次恒真判断（clippy 1.99 会直接报 `assertions_on_constants`）。
// 放在模块级 `const _` 里，编译不过就是编译不过。
const _: () = assert!(SHA256_HEX_LEN == 32 * 2, "hex 长度 = 32 字节 × 2");
const _: () = assert!(KEY_BYTES == 32, "对称密钥 32 字节 = 256 位");
const _: () = assert!(
    DEFAULT_SEARCH_LIMIT < DEFAULT_PAGE_LIMIT,
    "搜索默认条数应当小于一页"
);

// ---------------------------------------------------------------------------
// 风控阈值配置
// ---------------------------------------------------------------------------

/// 风控阈值配置。默认值按"正常用户碰不到、脚本一定会撞上"取。
///
/// 这里只放**配置的形状与默认值**；滑动窗口/GCRA 与裁决逻辑在
/// `wxlike-risk`。`wxlike_risk::RiskConfig` 是它的 re-export，调用方
/// 不需要改 import 路径。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiskConfig {
    /// 同一来源 1 h 内注册次数达到此值 → Challenge。
    pub register_challenge_at: usize,
    /// 达到此值 → Deny（脚本批量注册）。
    pub register_deny_at: usize,
    /// 15 min 内登录失败达到此值 → Challenge。
    pub login_fail_challenge_at: usize,
    /// 达到此值 → Deny（撞库）。
    pub login_fail_deny_at: usize,
    /// 10 s 内发送达到此值 → Throttle。
    pub send_throttle_at: usize,
    /// 达到此值 → Deny（刷屏）。
    pub send_deny_at: usize,
    /// 1 h 内举报达到此值 → Throttle。
    pub report_throttle_at: usize,
    /// 达到此值 → Deny（举报轰炸）。
    pub report_deny_at: usize,
    /// 同一 (caller, target) 1 h 内认领预密钥达到此值 → Throttle。
    pub claim_keys_throttle_at: usize,
    /// 达到此值 → Deny（库存耗尽攻击）。真人远够用：一次会话重建只要一把。
    pub claim_keys_deny_at: usize,
    /// 1 h 内好友请求达到此值 → Throttle（AR-5）。
    pub friend_request_throttle_at: usize,
    /// 达到此值 → Deny（申请轰炸）。
    pub friend_request_deny_at: usize,
    /// 10 min 内媒体上传达到此值 → Throttle（AR-5）。
    pub media_upload_throttle_at: usize,
    /// 达到此值 → Deny（上传轰炸）。
    pub media_upload_deny_at: usize,
    /// 10 min 内朋友圈发布/点赞/评论达到此值 → Throttle（AR-5）。
    pub moment_create_throttle_at: usize,
    /// 达到此值 → Deny（刷屏）。
    pub moment_create_deny_at: usize,
    /// 10 min 内建群/拉人/踢人达到此值 → Throttle（AR-5 余量）。
    pub group_manage_throttle_at: usize,
    /// 达到此值 → Deny（建群/拉人轰炸）。
    pub group_manage_deny_at: usize,
    /// 10 min 内社区建社/建频道/发帖达到此值 → Throttle。
    pub community_post_throttle_at: usize,
    /// 达到此值 → Deny（刷帖/刷频道）。
    pub community_post_deny_at: usize,
    /// 1 h 内开播达到此值 → Throttle。
    pub live_start_throttle_at: usize,
    /// 达到此值 → Deny。
    pub live_start_deny_at: usize,
    /// 10 s 内通话信令达到此值 → Throttle（ICE trickle 是合法突发）。
    pub call_signal_throttle_at: usize,
    /// 达到此值 → Deny。
    pub call_signal_deny_at: usize,
}

/// 同来源一小时内注册多少次起要求人机校验。
///
/// 50 而不是 3：共享出口（公司网络 / 运营商 NAT / CI runner）上三个人注册
/// 就有人被要求 captcha 是误伤，比脚本注册更常见。deny 在 200。
const REGISTER_CHALLENGE_AT: usize = 50;
const REGISTER_DENY_AT: usize = 200;

// AR-2 / AR-5 新增动作的默认阈值。写接口（好友请求 / 朋友圈）共用一档；
// 媒体上传是重操作，单独放宽。
const CLAIM_KEYS_THROTTLE_AT: usize = 30;
const CLAIM_KEYS_DENY_AT: usize = 120;
const WRITE_THROTTLE_AT: usize = 30;
const WRITE_DENY_AT: usize = 100;
const MEDIA_UPLOAD_THROTTLE_AT: usize = 60;
const MEDIA_UPLOAD_DENY_AT: usize = 200;

// T-B2 四个写面的阈值。群/社区管理是低频操作，与好友请求同档；
// 开播一次要拉起媒体链路，正常主播不会一小时内开十次；
// 信令要容纳 ICE trickle 的合法突发（10 s 窗口）。
const GROUP_MANAGE_THROTTLE_AT: usize = 30;
const GROUP_MANAGE_DENY_AT: usize = 100;
const COMMUNITY_POST_THROTTLE_AT: usize = 30;
const COMMUNITY_POST_DENY_AT: usize = 100;
const LIVE_START_THROTTLE_AT: usize = 10;
const LIVE_START_DENY_AT: usize = 30;
const CALL_SIGNAL_THROTTLE_AT: usize = 100;
const CALL_SIGNAL_DENY_AT: usize = 300;

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            // 注册按**来源**计数（issue #19 之后来源是真实 peer IP）。
            //
            // 原来是 3：一个公司网络、一个运营商 NAT、一台 CI runner 上三个人
            // 注册就会有人被要求 captcha——"共享出口连坐"比"脚本注册"更常见。
            // 50 次/小时对正常出口绰绰有余，对脚本仍然是一道墙；deny 在 200。
            register_challenge_at: REGISTER_CHALLENGE_AT,
            register_deny_at: REGISTER_DENY_AT,
            login_fail_challenge_at: 5,
            login_fail_deny_at: 12,
            send_throttle_at: 20,
            send_deny_at: 60,
            report_throttle_at: 10,
            report_deny_at: 30,
            claim_keys_throttle_at: CLAIM_KEYS_THROTTLE_AT,
            claim_keys_deny_at: CLAIM_KEYS_DENY_AT,
            friend_request_throttle_at: WRITE_THROTTLE_AT,
            friend_request_deny_at: WRITE_DENY_AT,
            media_upload_throttle_at: MEDIA_UPLOAD_THROTTLE_AT,
            media_upload_deny_at: MEDIA_UPLOAD_DENY_AT,
            moment_create_throttle_at: WRITE_THROTTLE_AT,
            moment_create_deny_at: WRITE_DENY_AT,
            group_manage_throttle_at: GROUP_MANAGE_THROTTLE_AT,
            group_manage_deny_at: GROUP_MANAGE_DENY_AT,
            community_post_throttle_at: COMMUNITY_POST_THROTTLE_AT,
            community_post_deny_at: COMMUNITY_POST_DENY_AT,
            live_start_throttle_at: LIVE_START_THROTTLE_AT,
            live_start_deny_at: LIVE_START_DENY_AT,
            call_signal_throttle_at: CALL_SIGNAL_THROTTLE_AT,
            call_signal_deny_at: CALL_SIGNAL_DENY_AT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RiskConfig;

    /// 两档阈值必须严格递增：否则 `deny_at <= throttle_at` 时 Deny 永远
    /// 不可达，风控从"两档"退化成"一档"。
    #[test]
    fn default_thresholds_are_strictly_ordered() {
        let c = RiskConfig::default();
        let pairs = [
            (c.register_challenge_at, c.register_deny_at),
            (c.login_fail_challenge_at, c.login_fail_deny_at),
            (c.send_throttle_at, c.send_deny_at),
            (c.report_throttle_at, c.report_deny_at),
            (c.claim_keys_throttle_at, c.claim_keys_deny_at),
            (c.friend_request_throttle_at, c.friend_request_deny_at),
            (c.media_upload_throttle_at, c.media_upload_deny_at),
            (c.moment_create_throttle_at, c.moment_create_deny_at),
            (c.group_manage_throttle_at, c.group_manage_deny_at),
            (c.community_post_throttle_at, c.community_post_deny_at),
            (c.live_start_throttle_at, c.live_start_deny_at),
            (c.call_signal_throttle_at, c.call_signal_deny_at),
        ];
        for (throttle, deny) in pairs {
            assert!(throttle < deny, "throttle {throttle} must be < deny {deny}");
        }
    }
}
