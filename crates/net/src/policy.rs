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

#[async_trait::async_trait]
pub trait DeliveryPolicy: Send + Sync + 'static {
    /// 收件人 `to` 已拉黑发送者 `from`：该帧对此收件人不存在。
    async fn blocked(&self, from: &str, to: &str) -> bool;

    /// `user` 在 `group` 当前被禁言（全员或单人、未到解除时刻）。
    async fn muted(&self, group: &str, user: &str) -> bool;

    /// `from` 可直发 `to`（好友，或对方接受过其陌生人请求——不变式 9）。
    async fn direct_allowed(&self, from: &str, to: &str) -> bool;
}
