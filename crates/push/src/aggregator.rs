//! P3 通知聚合 + 多级免打扰（合同 §3.13，ADR-19 决策层，goim 分层推送设计）。
//!
//! 决策/传输分离（不变式 5）：本模块是**决策层**——离线事件进来，先过多级
//! 免打扰规则，再进聚合窗口；窗口到期才把折叠后的摘要交给传输层
//! （[`crate::DispatchHook`] → provider）。`push-gateway` 永远只收"已决策"
//! 的最终通知。
//!
//! 多级免打扰（按会话/频道粒度 + 关键词）：
//! * `All` —— 正常推送（默认）；
//! * `MentionOnly` —— 仅 @我 才推（群/频道高频消息的解药）；
//! * `None` —— 完全静音；
//! * `keywords` 黑名单：正文预览命中任一关键词即不推。
//!
//! 聚合（goim"活跃实时/非活跃聚合"的窗口化落法）：同一会话窗口内的多条
//! 消息折叠为一条摘要任务，`count` 与 `last_from` 供客户端渲染"群X：张三
//! 等 5 人发来 8 条消息"。

use std::collections::HashMap;

/// 会话级免打扰级别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MuteLevel {
    /// 正常推送。
    #[default]
    All,
    /// 仅 @我 才推。
    MentionOnly,
    /// 完全静音。
    None,
}

impl MuteLevel {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "all" => Some(Self::All),
            "mention" => Some(Self::MentionOnly),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::MentionOnly => "mention",
            Self::None => "none",
        }
    }
}

/// 一条会话的免打扰规则（级别 + 关键词黑名单）。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MuteRule {
    pub level: MuteLevel,
    pub keywords: Vec<String>,
}

impl MuteRule {
    pub fn new(level: MuteLevel, keywords: &[String]) -> Self {
        Self {
            level,
            keywords: keywords.to_vec(),
        }
    }
}

/// 一次待决通知的最小事实（不含正文——与 crate 铁律一致）。
#[derive(Clone, Debug, PartialEq)]
pub struct PendingPush {
    pub conv_id: String,
    pub from_user: String,
    /// 是否点名了收件人（@我 / 直聊天然算点名）。
    pub mentioned: bool,
    /// 正文预览仅供关键词过滤，不进入摘要 payload。
    pub preview: String,
    pub at_ms: u64,
}

/// 折叠后的摘要推送任务（传输层拿到的最终形态）。
#[derive(Clone, Debug, PartialEq)]
pub struct AggregatedPush {
    pub conv_id: String,
    pub count: u64,
    /// 最近一位发送者（"群X：张三 等 N 人"）。
    pub last_from: String,
    /// 是否包含 @收件人 的消息（客户端可据此渲染强提醒）。
    pub mentioned: bool,
    pub first_at_ms: u64,
    pub last_at_ms: u64,
}

/// 免打扰 + 聚合的纯决策函数：这条消息要不要进入推送管道？
///
/// 直聊（`direct = true`）天然视为点名，`MentionOnly` 也放行；群/频道帧按
/// 级别与关键词裁决。`rules` 缺省 = `All`（不过滤）。
pub fn should_notify(rule: Option<&MuteRule>, direct: bool, pending: &PendingPush) -> bool {
    let Some(rule) = rule else {
        return true;
    };
    // 关键词黑名单：命中即不推（级别再宽松也不推）
    for kw in &rule.keywords {
        if !kw.is_empty() && pending.preview.contains(kw.as_str()) {
            return false;
        }
    }
    if direct {
        return rule.level != MuteLevel::None;
    }
    match rule.level {
        MuteLevel::All => true,
        MuteLevel::MentionOnly => pending.mentioned,
        MuteLevel::None => false,
    }
}

/// 聚合器：按 (user, conv) 缓冲窗口内的离线事件；`flush` 把到期的缓冲折叠
/// 成每会话一条摘要任务。
///
/// 纯内存、无锁快照由调用方持锁；线程化（每用户 flush 定时器）是 server
/// 装配的事——决策逻辑保持可单测。
#[derive(Default)]
pub struct Aggregator {
    /// user -> conv -> 窗口内待决条目（保留最早/最晚时间与 mentioned 并集）。
    buffer: HashMap<String, HashMap<String, Buffered>>,
}

#[derive(Clone, Debug)]
struct Buffered {
    count: u64,
    mentioned: bool,
    last_from: String,
    first_at_ms: u64,
    last_at_ms: u64,
}

/// 聚合窗口：同会话此窗口内的事件折叠为一条（goim 非活跃聚合的窗口参数）。
pub const AGGREGATION_WINDOW_MS: u64 = 30_000;

impl Aggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// 一条离线事件进缓冲。返回 `Some(count)` = 该会话窗口内的当前条数
    /// （首条时为 1——调用方可立即推送首条，其余等窗口，即"首条实时、
    /// 后续聚合"的 goim 两段式）。
    pub fn push(&mut self, user: &str, pending: PendingPush) -> u64 {
        let convs = self.buffer.entry(user.to_string()).or_default();
        let entry = convs.entry(pending.conv_id.clone()).or_insert(Buffered {
            count: 0,
            mentioned: false,
            last_from: String::new(),
            first_at_ms: pending.at_ms,
            last_at_ms: pending.at_ms,
        });
        entry.count += 1;
        entry.mentioned |= pending.mentioned;
        entry.last_from = pending.from_user;
        entry.last_at_ms = entry.last_at_ms.max(pending.at_ms);
        entry.count
    }

    /// 冲刷某用户全部到期的缓冲（`now_ms` 起算窗口）。返回折叠后的摘要任务
    /// 并清空对应缓冲；未到窗口的会话保留。
    pub fn flush(&mut self, user: &str, now_ms: u64) -> Vec<AggregatedPush> {
        let Some(convs) = self.buffer.get_mut(user) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let convs_snapshot: Vec<String> = convs.keys().cloned().collect();
        for conv in convs_snapshot {
            let entry = &convs[&conv];
            // 到期判定：首条事件起算超窗（goim 窗口化聚合的窗口参数）
            if now_ms.saturating_sub(entry.first_at_ms) >= AGGREGATION_WINDOW_MS {
                out.push(AggregatedPush {
                    conv_id: conv.clone(),
                    count: entry.count,
                    last_from: entry.last_from.clone(),
                    mentioned: entry.mentioned,
                    first_at_ms: entry.first_at_ms,
                    last_at_ms: entry.last_at_ms,
                });
                convs.remove(&conv);
            }
        }
        if convs.is_empty() {
            self.buffer.remove(user);
        }
        out
    }

    /// 某用户缓冲中的会话数（观测面）。
    pub fn buffered_convs(&self, user: &str) -> usize {
        self.buffer.get(user).map(|c| c.len()).unwrap_or(0)
    }
}

impl Aggregator {
    /// 冲刷**全部用户**的到期缓冲（后台冲刷循环节拍用）。
    pub fn flush_all(&mut self, now_ms: u64) -> Vec<(String, AggregatedPush)> {
        let users: Vec<String> = self.buffer.keys().cloned().collect();
        let mut out = Vec::new();
        for user in users {
            for task in self.flush(&user, now_ms) {
                out.push((user.clone(), task));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// AggregatingDispatchHook：离线事件 → 免打扰裁决 → 首条实时/后续聚合
// → 窗口到期发摘要。不变式 5：传输层（provider）只收已决策的最终通知。
// ---------------------------------------------------------------------------

use crate::{DispatchHook, PushPayload};
use wxlike_core::Envelope;

/// 免打扰规则查询（server 用平台 notification_settings 表实现）。
/// `None` = 该会话无规则（默认 All 放行）。
#[async_trait::async_trait]
pub trait MuteRuleDirectory: Send + Sync + 'static {
    async fn rule_of(&self, user: &str, conv: &str) -> Option<MuteRule>;
}

/// 聚合式离线推送钩子，实现 [`wxlike_net::OfflinePush`]。
///
/// 两段式（goim）：会话首条**实时**发出（最快感知），窗口内的后续消息只进
/// 缓冲；到期由后台冲刷循环发一条 `count>1` 的摘要。免打扰在进缓冲前裁决
/// ——被静音的消息从不进入推送管道。收件人设备查询沿用内层 [`DispatchHook`]
/// 的目录与 provider。
pub struct AggregatingDispatchHook {
    inner: std::sync::Arc<DispatchHook>,
    rules: std::sync::Arc<dyn MuteRuleDirectory>,
    agg: std::sync::Mutex<Aggregator>,
}

impl AggregatingDispatchHook {
    /// 构造并拉起后台冲刷循环（每 `window_ms/4` 检查一次到期缓冲）。
    /// 必须在 tokio 运行时上下文中调用（server 的 start_stack 满足）。
    pub fn spawn(
        inner: std::sync::Arc<DispatchHook>,
        rules: std::sync::Arc<dyn MuteRuleDirectory>,
        window_ms: u64,
    ) -> std::sync::Arc<Self> {
        let hook = std::sync::Arc::new(Self {
            inner,
            rules,
            agg: std::sync::Mutex::new(Aggregator::new()),
        });
        // 冲刷循环：window/4 节拍
        let flusher = hook.clone();
        let interval = (window_ms / 4).max(50);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(interval));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let now = wxlike_core::types::now_ms();
                let due = {
                    let mut agg = flusher
                        .agg
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    agg.flush_all(now)
                };
                for (user, task) in due {
                    let payload = PushPayload {
                        from_user: task.last_from,
                        conv_id: task.conv_id,
                        count: Some(task.count),
                    };
                    if let Err(err) = flusher.inner.dispatch_devices(&user, &payload).await {
                        eprintln!("[push] aggregated send to {user} failed: {err:#}");
                    }
                }
            }
        });
        hook
    }
}

#[async_trait::async_trait]
impl wxlike_net::OfflinePush for AggregatingDispatchHook {
    async fn user_offline(&self, user_id: &str, env: &Envelope) {
        let pending = PendingPush {
            conv_id: env.conv_id.clone(),
            from_user: env.sender.clone(),
            // 直聊天然点名；群/频道的 @我 信息在 mention 帧 payload 里，
            // 决策层此处以直聊/群帧形状近似（mention 帧的 user_ids 由
            // hub @改写路径保证只发给被点名者）。
            mentioned: env.to.len() == 1 && env.to[0] == user_id && !env.conv_id.starts_with("g-"),
            preview: String::new(),
            at_ms: wxlike_core::types::now_ms(),
        };
        let rule = self.rules.rule_of(user_id, &pending.conv_id).await;
        if !should_notify(rule.as_ref(), pending.conv_id.starts_with("d-"), &pending) {
            return; // 免打扰：从不进管道
        }

        let immediate = {
            let mut agg = self
                .agg
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let count = agg.push(user_id, pending);
            count == 1
        };
        if immediate {
            // 首条实时（goim 两段式的前段）
            if let Err(err) = self
                .inner
                .dispatch_devices(user_id, &crate::PushPayload::of(env))
                .await
            {
                eprintln!("[push] immediate send to {user_id} failed: {err:#}");
            }
        }
        // 后续消息等冲刷循环的摘要
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(conv: &str, from: &str, mentioned: bool, preview: &str, at: u64) -> PendingPush {
        PendingPush {
            conv_id: conv.into(),
            from_user: from.into(),
            mentioned,
            preview: preview.into(),
            at_ms: at,
        }
    }

    #[test]
    fn mute_levels_gate_group_but_direct_is_special() {
        let mention_rule = MuteRule::new(MuteLevel::MentionOnly, &[]);
        // 群帧：MentionOnly 只放行 @我
        assert!(!should_notify(
            Some(&mention_rule),
            false,
            &pending("g-1", "a", false, "hello", 1)
        ));
        assert!(should_notify(
            Some(&mention_rule),
            false,
            &pending("g-1", "a", true, "@bob look", 1)
        ));
        // 直聊天然点名：MentionOnly 也推
        assert!(should_notify(
            Some(&mention_rule),
            true,
            &pending("d-a~b", "a", false, "hi", 1)
        ));
        // None 级别：直聊也静音
        let none_rule = MuteRule::new(MuteLevel::None, &[]);
        assert!(!should_notify(
            Some(&none_rule),
            true,
            &pending("d-a~b", "a", true, "hi", 1)
        ));
        // 无规则 = 全推
        assert!(should_notify(
            None,
            false,
            &pending("g-1", "a", false, "x", 1)
        ));
    }

    #[test]
    fn keyword_blocklist_overrides_level() {
        let rule = MuteRule::new(MuteLevel::All, &["广告".into(), "加微".into()]);
        assert!(!should_notify(
            Some(&rule),
            true,
            &pending("d-a~b", "x", true, "最新广告不要错过", 1)
        ));
        assert!(!should_notify(
            Some(&rule),
            true,
            &pending("d-a~b", "x", true, "加我加微 xxx", 1)
        ));
        assert!(should_notify(
            Some(&rule),
            true,
            &pending("d-a~b", "x", true, "正常聊天", 1)
        ));
    }

    #[test]
    fn aggregator_collapses_burst_into_one_summary() {
        let mut agg = Aggregator::new();
        let user = "bob";
        // 窗口内同会话 3 条：首条计数 1，后续 2、3
        assert_eq!(agg.push(user, pending("g-big", "a", false, "m1", 1_000)), 1);
        assert_eq!(
            agg.push(user, pending("g-big", "b", true, "m2 @bob", 2_000)),
            2
        );
        assert_eq!(agg.push(user, pending("g-big", "a", false, "m3", 3_000)), 3);
        assert_eq!(agg.buffered_convs(user), 1);

        // 窗口未到（首条起 5s < 30s）不冲刷
        assert!(agg.flush(user, 6_000).is_empty());

        // 到期冲刷：折叠为一条，mentioned 并集保留、last_from 为最新
        let due = agg.flush(user, 31_000);
        assert_eq!(due.len(), 1);
        let task = &due[0];
        assert_eq!(task.conv_id, "g-big");
        assert_eq!(task.count, 3);
        assert!(task.mentioned, "m2 点名了 bob");
        assert_eq!(task.last_from, "a");
        assert_eq!(task.first_at_ms, 1_000);
        assert_eq!(task.last_at_ms, 3_000);

        // 冲刷后缓冲清空
        assert_eq!(agg.buffered_convs(user), 0);
        assert!(agg.flush(user, 40_000).is_empty());
    }

    #[test]
    fn aggregator_keeps_conversations_separate() {
        let mut agg = Aggregator::new();
        agg.push("u", pending("g-1", "a", false, "x", 1_000));
        agg.push("u", pending("g-2", "b", false, "y", 1_100));
        agg.push("v", pending("g-1", "c", false, "z", 1_200));
        assert_eq!(agg.buffered_convs("u"), 2);
        assert_eq!(agg.buffered_convs("v"), 1);

        // g-1@u 先到期（窗口按各自首条起算）；g-2 晚 100ms 属下个窗口
        let due = agg.flush("u", 31_000);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].conv_id, "g-1");
        assert_eq!(agg.buffered_convs("u"), 1, "g-2 窗口未到");
        assert_eq!(agg.buffered_convs("v"), 1, "v 互不影响");

        // g-2 到期冲刷；v 到期冲刷
        let due = agg.flush("u", 31_200);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].conv_id, "g-2");
        let _ = agg.flush("v", 31_200);
        assert_eq!(agg.buffered_convs("u"), 0);
    }

    #[test]
    fn filter_and_aggregator_compose() {
        // 组合语义：被免打扰拦下的消息不进聚合缓冲
        let rule = MuteRule::new(MuteLevel::None, &[]);
        let mut agg = Aggregator::new();
        let events = [
            pending("g-x", "a", false, "one", 1_000),
            pending("g-x", "b", false, "two", 2_000),
        ];
        for e in &events {
            if should_notify(Some(&rule), false, e) {
                agg.push("bob", e.clone());
            }
        }
        assert_eq!(agg.buffered_convs("bob"), 0, "全部被 None 级别拦下");
    }
}
