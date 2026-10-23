//! `wxlike-risk` — P4 风控前置层（ADR-20 第一层：risk-service）。
//!
//! 职责边界（合同 §3.14 / 附件二 ADR-20）：**毫秒级实时裁决，不调用审核
//! 服务**。审核是"这条内容是什么"（`wxlike_moderation`），风控是"这次
//! 请求该不该放行"——两者数据面不同、超时预算不同，合并会让一次内容判定
//! 的抖动拖垮登录路径。
//!
//! 裁决四态（合同原文）：放行 / 限流 / 挑战 / 拒绝。
//!
//! * **Allow** — 正常放行；
//! * **Throttle** — 放行但要求退避（发送/举报这类可降速的入口）；
//! * **Challenge** — 必须过人机校验才能继续（注册/登录这类一次性入口，
//!   由 platform 的 captcha 兑现）；
//! * **Deny** — 直接拒绝，附机器可读原因。
//!
//! 时间由调用方传入（`now_ms`），引擎内部不读时钟：风控规则的时间窗口
//! 必须能在测试里被精确推进，否则只能靠 sleep 写出脆弱的用例。
//!
//! 状态存储抽成 [`RiskStore`]：默认 [`MemoryStore`] 是进程内滑动窗口
//! （每 `(action, key)` 一条 `VecDeque<u64>`）；多节点部署用 `redis`
//! feature 的 [`gcra::GcraStore`]（共享 TAT，全局阈值不会乘上节点数）。
//! 阈值配置的形状与默认值在 `wxlike-limits`，这里只做算法与裁决。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use wxlike_limits::RiskConfig;

pub mod store;

pub use store::{MemoryStore, RiskStore, Rule, StoreVerdict};

#[cfg(feature = "redis")]
pub mod gcra;

/// 受风控约束的动作（合同 §3.14 三层 + 举报防刷）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Register,
    Login,
    Send,
    Report,
    /// AR-2/H-4（第四轮审计）：预密钥认领。一次性密钥库存是有限资源，
    /// 任何登录用户都能循环认领他人库存 = 廉价资源耗尽 DoS，所以认领
    /// 也要进闸门（键形如 `caller:target`）。
    ClaimKeys,
    /// AR-5（第四轮审计 #29）：写接口中间件闸门的三个动作。此前风控只
    /// 在登录/注册/举报/认领四处手工调用，好友请求/媒体上传/朋友圈刷写
    /// 全裸——漏掉是"每个 handler 手工调用"这一模式的结构性必然。
    FriendRequest,
    MediaUpload,
    MomentCreate,
}

impl Action {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::Login => "login",
            Self::Send => "send",
            Self::Report => "report",
            Self::ClaimKeys => "claim_keys",
            Self::FriendRequest => "friend_request",
            Self::MediaUpload => "media_upload",
            Self::MomentCreate => "moment_create",
        }
    }

    /// 窗口长度：注册 1 h、登录 15 min、发送 10 s、举报 1 h。
    fn window_ms(&self) -> u64 {
        match self {
            Self::Register | Self::Report | Self::ClaimKeys | Self::FriendRequest => 60 * 60 * 1000,
            Self::Login => 15 * 60 * 1000,
            Self::Send => 10 * 1000,
            // 上传/发动态是重操作，10 秒窗口太短（一次九宫格就 9 条），给 10 分钟
            Self::MediaUpload | Self::MomentCreate => 10 * 60 * 1000,
        }
    }
}

/// 裁决结果。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    Allow,
    /// 退避后再试；`retry_after_ms` 是窗口内最早一条记录过期的时间。
    Throttle {
        retry_after_ms: u64,
    },
    /// 必须过人机校验（captcha）才能继续。
    Challenge {
        reason: String,
    },
    /// 直接拒绝。
    Deny {
        reason: String,
    },
}

impl Verdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }

    pub fn needs_captcha(&self) -> bool {
        matches!(self, Self::Challenge { .. })
    }
}

/// 单个动作的阈值与裁决形状：captcha 动作给 Challenge，其余给 Throttle。
struct Limits {
    throttle_at: usize,
    deny_at: usize,
    reason: &'static str,
    captcha: bool,
}

fn limits(cfg: &RiskConfig, action: Action) -> Limits {
    match action {
        Action::Register => Limits {
            throttle_at: cfg.register_challenge_at,
            deny_at: cfg.register_deny_at,
            reason: "register_rate",
            captcha: true,
        },
        Action::Login => Limits {
            throttle_at: cfg.login_fail_challenge_at,
            deny_at: cfg.login_fail_deny_at,
            reason: "login_failures",
            captcha: true,
        },
        Action::Send => Limits {
            throttle_at: cfg.send_throttle_at,
            deny_at: cfg.send_deny_at,
            reason: "send_rate",
            captcha: false,
        },
        Action::ClaimKeys => Limits {
            throttle_at: cfg.claim_keys_throttle_at,
            deny_at: cfg.claim_keys_deny_at,
            reason: "claim_keys_rate",
            captcha: false,
        },
        Action::FriendRequest => Limits {
            throttle_at: cfg.friend_request_throttle_at,
            deny_at: cfg.friend_request_deny_at,
            reason: "friend_request_rate",
            captcha: false,
        },
        Action::MediaUpload => Limits {
            throttle_at: cfg.media_upload_throttle_at,
            deny_at: cfg.media_upload_deny_at,
            reason: "media_upload_rate",
            captcha: false,
        },
        Action::MomentCreate => Limits {
            throttle_at: cfg.moment_create_throttle_at,
            deny_at: cfg.moment_create_deny_at,
            reason: "moment_create_rate",
            captcha: false,
        },
        Action::Report => Limits {
            throttle_at: cfg.report_throttle_at,
            deny_at: cfg.report_deny_at,
            reason: "report_rate",
            captcha: false,
        },
    }
}

fn rule_for(cfg: &RiskConfig, action: Action) -> Rule {
    let limits = limits(cfg, action);
    Rule {
        window_ms: action.window_ms(),
        throttle_at: limits.throttle_at,
        deny_at: limits.deny_at,
    }
}

fn map_verdict(cfg: &RiskConfig, action: Action, stored: StoreVerdict) -> Verdict {
    let limits = limits(cfg, action);
    match stored {
        StoreVerdict::Allow => Verdict::Allow,
        StoreVerdict::Deny { .. } => Verdict::Deny {
            reason: limits.reason.into(),
        },
        StoreVerdict::Throttle { retry_after_ms } => {
            if limits.captcha {
                Verdict::Challenge {
                    reason: limits.reason.into(),
                }
            } else {
                Verdict::Throttle { retry_after_ms }
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RiskError {
    #[error("risk engine lock poisoned")]
    Poisoned,
    /// 存储后端不可用（Redis 连接/脚本错误等）。平台把它映射成 500，
    /// 不伪装成客户端错误。
    #[error("risk store error: {0}")]
    Store(String),
}

pub type Result<T> = std::result::Result<T, RiskError>;

/// 风控引擎：配置 + 存储后端。进程内滑动窗口是默认实现。
pub struct RiskEngine {
    cfg: RiskConfig,
    store: Arc<dyn RiskStore>,
}

impl RiskEngine {
    pub fn new(cfg: RiskConfig) -> Self {
        Self::with_store(cfg, Arc::new(MemoryStore::new()))
    }

    pub fn with_store(cfg: RiskConfig, store: Arc<dyn RiskStore>) -> Self {
        Self { cfg, store }
    }

    pub fn with_defaults() -> Self {
        Self::new(RiskConfig::default())
    }

    /// 启动期配置：设置 `WXLIKE_RISK_REDIS_URL`（或 `REDIS_URL`）时走 Redis
    /// GCRA，否则用进程内实现。**配了 URL 却连不上会返回错误**——多节点
    /// 部署静默退化成本地计数，比启动失败更危险。
    ///
    /// 未带 `redis` feature 时，配了 URL 也会返回错误而不是悄悄退化。
    pub fn from_env() -> Result<Self> {
        match std::env::var("WXLIKE_RISK_REDIS_URL").or_else(|_| std::env::var("REDIS_URL")) {
            Ok(url) if !url.trim().is_empty() => Self::from_redis_url(&url),
            _ => Ok(Self::with_defaults()),
        }
    }

    pub fn from_redis_url(url: &str) -> Result<Self> {
        #[cfg(feature = "redis")]
        {
            let store = gcra::GcraStore::connect(url)?;
            Ok(Self::with_store(RiskConfig::default(), Arc::new(store)))
        }
        #[cfg(not(feature = "redis"))]
        {
            let _ = url;
            Err(RiskError::Store(
                "wxlike-risk built without the `redis` feature".into(),
            ))
        }
    }

    pub fn config(&self) -> &RiskConfig {
        &self.cfg
    }

    /// 只读裁决：不改变存储状态。`key` 由调用方决定（注册用来源 IP /
    /// 设备标识，登录用账号名，发送用 user_id）。
    pub fn check(&self, action: Action, key: &str, now_ms: u64) -> Result<Verdict> {
        let rule = rule_for(&self.cfg, action);
        let stored = self.store.check(action, key, now_ms, &rule)?;
        Ok(map_verdict(&self.cfg, action, stored))
    }

    /// 记一次事件（发送、举报、注册尝试、登录失败都算）。
    pub fn record(&self, action: Action, key: &str, now_ms: u64) -> Result<Verdict> {
        let rule = rule_for(&self.cfg, action);
        let stored = self.store.record(action, key, now_ms, &rule)?;
        Ok(map_verdict(&self.cfg, action, stored))
    }

    /// 一次成功动作清空失败计数（登录成功即解除 Challenge）。
    pub fn clear(&self, action: Action, key: &str) -> Result<()> {
        self.store.clear(action, key)
    }

    /// 当前窗口内计数（运维面板/测试用）。
    pub fn count(&self, action: Action, key: &str, now_ms: u64) -> Result<usize> {
        let rule = rule_for(&self.cfg, action);
        self.store.count(action, key, now_ms, &rule)
    }
}

impl Default for RiskEngine {
    fn default() -> Self {
        Self::with_defaults()
    }
}
