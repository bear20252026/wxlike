//! P3 投递策略（合同不变式 9 / §3.2 拉黑 / §3.3 禁言的投递面落地）。
//!
//! hub 在内容帧入站后、持久化与扇出前咨询此策略；裁决权在调用方注入的
//! 实现（server 用 `Platform` 的关系/禁言表作答，测试用静态表）。三条裁决
//! 全部为**帧级丢弃**：被拦的帧不落库、不暂存、不扇出——被拉黑者的消息连
//! 痕迹都不留（与 WP-03 伪造撤回同一处理哲学）。
//!
//! 豁免范围（刻意收窄）：
//! * **控制帧**（回执/撤回）不咨询策略——撤回自己已发出的消息不受后续拉黑
//!   影响，回执路由同理；
//! * **系统帧**（`Notice` body，好友/群事件）不咨询——管理与关系事件必须
//!   到达（否则拉黑后连"取消拉黑"都收不到对方的任何信号）。
//!
//! 失败语义：实现方在裁决查询出错时**放行**（fail-open）并自行记日志——
//! 拦截增强不能变成可用性单点。

/// 发送闸门允许的最长退避。超过它的 `Sleep` 不再等待，按 `Reject` 丢弃。
///
/// 退避发生在该连接的读循环里：无上限的 sleep 等于把连接任务交给策略方
/// 无限挂起（慢速 DoS）。1 秒够吸收一次突发，又不会让一条连接停摆太久。
pub const MAX_SEND_GATE_SLEEP_MS: u64 = 1_000;

/// P4 发送闸门结果（风控 + 内容审核）。
///
/// 与 `blocked`/`muted` 的区别：那两条是**关系裁决**（谁可以给谁发），
/// 这条是**行为裁决**（这个人此刻还能不能发、这条内容能不能投递）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendGate {
    Allow,
    /// 降速：帧丢弃，调用方应提示"发送过快"。
    Throttle {
        retry_after_ms: u64,
    },
    /// 软限流（Synapse `should_sleep` 同型）：等 `delay_ms` 后继续投递。
    ///
    /// 超过 [`MAX_SEND_GATE_SLEEP_MS`] 的请求由 hub 按拒绝处理。
    Sleep {
        delay_ms: u64,
    },
    /// 拒绝：硬违规或封禁。
    Reject {
        reason: String,
    },
}

#[async_trait::async_trait]
pub trait DeliveryPolicy: Send + Sync + 'static {
    /// 收件人 `to` 已拉黑发送者 `from`：该帧对此收件人不存在。
    async fn blocked(&self, from: &str, to: &str) -> bool;

    /// `user` 在 `group` 当前被禁言（全员或单人、未到解除时刻）。
    async fn muted(&self, group: &str, user: &str) -> bool;

    /// `from` 可直发 `to`（好友，或对方接受过其陌生人请求——不变式 9）。
    async fn direct_allowed(&self, from: &str, to: &str) -> bool;

    /// P4 发送闸门：风控限流 + 内容审核（ADR-20 前两层）。
    ///
    /// 默认放行——保持 P3 的既有实现与测试不动；只有注入真实平台的部署
    /// 才覆盖它。`Throttle`/`Reject` 时 hub 丢弃该帧（不落库、不暂存、
    /// 不扇出）；`Sleep` 时先退避再走正常投递。
    async fn send_gate(&self, _from: &str, _conv: &str, _text: &str) -> SendGate {
        SendGate::Allow
    }

    /// 发送闸门在限流指标里的名字（Synapse `rate_limiter_name` 标签）。
    ///
    /// 多个限流器共用一个 hub 时靠它区分序列；注入真实风控的部署可以
    /// 覆盖成 `risk_send` 之类，默认值代表"策略层统一闸门"。
    fn rate_limiter_name(&self) -> &'static str {
        "send_gate"
    }

    /// AR-6（第四轮审计 #30）：秘密会话的消息 TTL 兜底。
    ///
    /// 返回该会话启用的消息存活时长（毫秒）；`None` = 非秘密会话 / 未启用
    /// / 查询失败（fail-open，与 `blocked` 同语义）。hub 在**补发离线帧**
    /// 时咨询它：`sent_at_ms + ttl < now` 的内容帧直接 ack 丢弃——秘密
    /// 消息"到期即不可再取"从只覆盖 HTTP 检索/搜索面，扩展到 QUIC/WS
    /// 两条实时链路的最后一跳（issue #15 的"端到端闭环"缺口）。
    /// 回执/撤回/系统帧豁免：撤回与回执恰恰要在过期后仍能到达。
    async fn secret_ttl_ms(&self, _conv_id: &str) -> Option<u64> {
        None
    }
}
