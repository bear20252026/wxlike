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
//! 存储是进程内滑动窗口（每 `(action, key)` 一条 `VecDeque<u64>`）。多节点
//! 部署时换成 Redis ZSET 即可——本 crate 不持有连接，`RiskEngine` 的接口
//! 就是将来 `RiskStore` trait 的形状。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// 受风控约束的四类动作（合同 §3.14 三层 + 举报防刷）。
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
            Self::Register | Self::Report | Self::ClaimKeys | Self::FriendRequest => {
                60 * 60 * 1000
            }
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

/// 阈值配置。默认值按"正常用户碰不到、脚本一定会撞上"取。
#[derive(Clone, Debug)]
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
}

/// 同来源一小时内注册多少次起要求人机校验。
///
/// 50 而不是 3：共享出口（公司网络 / 运营商 NAT / CI runner）上三个人注册
/// 就有人被要求 captcha 是误伤，比脚本注册更常见。deny 在 200。
const REGISTER_CHALLENGE_AT: usize = 50;
const REGISTER_DENY_AT: usize = 200;

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
            claim_keys_throttle_at: 30,
            claim_keys_deny_at: 120,
            friend_request_throttle_at: 30,
            friend_request_deny_at: 100,
            media_upload_throttle_at: 60,
            media_upload_deny_at: 200,
            moment_create_throttle_at: 30,
            moment_create_deny_at: 100,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RiskError {
    #[error("risk engine lock poisoned")]
    Poisoned,
}

pub type Result<T> = std::result::Result<T, RiskError>;

/// 进程内滑动窗口风控引擎。
pub struct RiskEngine {
    cfg: RiskConfig,
    /// `(action, key) -> 窗口内事件时间戳（升序）`。
    windows: Mutex<HashMap<(Action, String), VecDeque<u64>>>,
}

impl RiskEngine {
    pub fn new(cfg: RiskConfig) -> Self {
        Self {
            cfg,
            windows: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(RiskConfig::default())
    }

    pub fn config(&self) -> &RiskConfig {
        &self.cfg
    }

    /// 只读裁决：不改变窗口状态。`key` 由调用方决定（注册用来源 IP /
    /// 设备标识，登录用账号名，发送用 user_id）。
    pub fn check(&self, action: Action, key: &str, now_ms: u64) -> Result<Verdict> {
        let mut guard = self.windows.lock().map_err(|_| RiskError::Poisoned)?;
        let window = action.window_ms();
        let events = guard.entry((action, key.to_string())).or_default();
        evict(events, now_ms, window);
        // +1：本次尝试也要计入，否则"第 N 次触发阈值"会晚一次才生效
        Ok(self.decide(action, events, now_ms, window, 1))
    }

    /// 记一次事件（发送、举报、注册尝试、登录失败都算）。
    pub fn record(&self, action: Action, key: &str, now_ms: u64) -> Result<Verdict> {
        let mut guard = self.windows.lock().map_err(|_| RiskError::Poisoned)?;
        let window = action.window_ms();
        let events = guard.entry((action, key.to_string())).or_default();
        evict(events, now_ms, window);
        events.push_back(now_ms);
        Ok(self.decide(action, events, now_ms, window, 0))
    }

    /// 一次成功动作清空失败计数（登录成功即解除 Challenge）。
    pub fn clear(&self, action: Action, key: &str) -> Result<()> {
        let mut guard = self.windows.lock().map_err(|_| RiskError::Poisoned)?;
        guard.remove(&(action, key.to_string()));
        Ok(())
    }

    /// 当前窗口内计数（运维面板/测试用）。
    pub fn count(&self, action: Action, key: &str, now_ms: u64) -> Result<usize> {
        let mut guard = self.windows.lock().map_err(|_| RiskError::Poisoned)?;
        let window = action.window_ms();
        let events = guard.entry((action, key.to_string())).or_default();
        evict(events, now_ms, window);
        Ok(events.len())
    }

    /// 无 captcha 兑现面动作的公共裁决：throttle → deny → allow。
    fn throttle_or_deny(
        &self,
        n: usize,
        retry_after: u64,
        throttle_at: usize,
        deny_at: usize,
        reason: &'static str,
    ) -> Verdict {
        if n >= deny_at {
            Verdict::Deny {
                reason: reason.into(),
            }
        } else if n >= throttle_at {
            Verdict::Throttle {
                retry_after_ms: retry_after,
            }
        } else {
            Verdict::Allow
        }
    }

    fn decide(
        &self,
        action: Action,
        events: &VecDeque<u64>,
        now_ms: u64,
        window: u64,
        extra: usize,
    ) -> Verdict {
        let n = events.len() + extra;
        let retry_after = events
            .front()
            .map(|&oldest| window.saturating_sub(now_ms.saturating_sub(oldest)))
            .unwrap_or(0);
        match action {
            Action::Register => {
                if n >= self.cfg.register_deny_at {
                    Verdict::Deny {
                        reason: "register_rate".into(),
                    }
                } else if n >= self.cfg.register_challenge_at {
                    Verdict::Challenge {
                        reason: "register_rate".into(),
                    }
                } else {
                    Verdict::Allow
                }
            }
            Action::Login => {
                if n >= self.cfg.login_fail_deny_at {
                    Verdict::Deny {
                        reason: "login_failures".into(),
                    }
                } else if n >= self.cfg.login_fail_challenge_at {
                    Verdict::Challenge {
                        reason: "login_failures".into(),
                    }
                } else {
                    Verdict::Allow
                }
            }
            Action::Send => {
                if n >= self.cfg.send_deny_at {
                    Verdict::Deny {
                        reason: "send_rate".into(),
                    }
                } else if n >= self.cfg.send_throttle_at {
                    Verdict::Throttle {
                        retry_after_ms: retry_after,
                    }
                } else {
                    Verdict::Allow
                }
            }
            // 认领没有 captcha 兑现面，Challenge 无处兑现——只 Throttle/Deny。
            Action::ClaimKeys => {
                if n >= self.cfg.claim_keys_deny_at {
                    Verdict::Deny {
                        reason: "claim_keys_rate".into(),
                    }
                } else if n >= self.cfg.claim_keys_throttle_at {
                    Verdict::Throttle {
                        retry_after_ms: retry_after,
                    }
                } else {
                    Verdict::Allow
                }
            }
            // 中间件闸门的三个动作同样没有 captcha 兑现面（写接口 429 即可）
            Action::FriendRequest => self.throttle_or_deny(
                n,
                retry_after,
                self.cfg.friend_request_throttle_at,
                self.cfg.friend_request_deny_at,
                "friend_request_rate",
            ),
            Action::MediaUpload => self.throttle_or_deny(
                n,
                retry_after,
                self.cfg.media_upload_throttle_at,
                self.cfg.media_upload_deny_at,
                "media_upload_rate",
            ),
            Action::MomentCreate => self.throttle_or_deny(
                n,
                retry_after,
                self.cfg.moment_create_throttle_at,
                self.cfg.moment_create_deny_at,
                "moment_create_rate",
            ),
            Action::Report => {
                if n >= self.cfg.report_deny_at {
                    Verdict::Deny {
                        reason: "report_rate".into(),
                    }
                } else if n >= self.cfg.report_throttle_at {
                    Verdict::Throttle {
                        retry_after_ms: retry_after,
                    }
                } else {
                    Verdict::Allow
                }
            }
        }
    }
}

impl Default for RiskEngine {
    fn default() -> Self {
        Self::with_defaults()
    }
}

fn evict(events: &mut VecDeque<u64>, now_ms: u64, window_ms: u64) {
    while let Some(&oldest) = events.front() {
        if now_ms.saturating_sub(oldest) >= window_ms {
            events.pop_front();
        } else {
            break;
        }
    }
}
