//! 事件信封：核心 → UI 的唯一推送形状。
//!
//! 三条设计取舍：
//!
//! 1. **事件带自己的 `seq` 与 `event_id`**。UI 不需要（也不允许）自己去猜
//!    "这条是不是已经渲染过"——重连后重放、双端同时在线都会重复投递，
//!    去重的依据只有 `event_id`。
//! 2. **`kind` 未知时保留原文**。服务端先上线一个新事件类型，旧客户端必须
//!    能解出 [`EventKind::Unknown`] 并跳过，而不是整个 `EventEnvelope`
//!    解析失败、连带后面的正常事件一起丢。
//! 3. **`payload` 不做强类型**。它属于各域（G02/G03/...），强类型化会把
//!    契约层变成所有域的交汇点；各域自己解，解不动就跳过。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::contract::{ContractVersion, CONTRACT_VERSION};
use crate::cursor::Cursor;
use crate::ids::{ConvId, EventId};
use crate::seq::Seq;

/// 事件类型。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EventKind {
    /// 新消息。
    MessageNew,
    /// 消息被编辑。
    MessageUpdated,
    /// 消息被撤回。
    MessageRecalled,
    /// 已读回执。
    ReadReceipt,
    /// 正在输入。
    Typing,
    /// 在线状态变化。
    Presence,
    /// 通话信令。
    CallSignal,
    /// 成员变化（进群/退群/改角色）。
    MemberChanged,
    /// 游标推进（服务端告诉你"到 N 了，没有别的新东西"）。
    CursorAdvanced,
    /// 本端不认识的事件：保留原文，跳过。
    Unknown(String),
}

impl EventKind {
    /// 线上取值。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            EventKind::MessageNew => "message.new",
            EventKind::MessageUpdated => "message.updated",
            EventKind::MessageRecalled => "message.recalled",
            EventKind::ReadReceipt => "read.receipt",
            EventKind::Typing => "typing",
            EventKind::Presence => "presence",
            EventKind::CallSignal => "call.signal",
            EventKind::MemberChanged => "member.changed",
            EventKind::CursorAdvanced => "cursor.advanced",
            EventKind::Unknown(raw) => raw,
        }
    }

    /// 解析；不认识的落到 [`EventKind::Unknown`]，**不报错**。
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "message.new" => EventKind::MessageNew,
            "message.updated" => EventKind::MessageUpdated,
            "message.recalled" => EventKind::MessageRecalled,
            "read.receipt" => EventKind::ReadReceipt,
            "typing" => EventKind::Typing,
            "presence" => EventKind::Presence,
            "call.signal" => EventKind::CallSignal,
            "member.changed" => EventKind::MemberChanged,
            "cursor.advanced" => EventKind::CursorAdvanced,
            other => EventKind::Unknown(other.to_string()),
        }
    }

    /// 本端认不认识。
    #[must_use]
    pub fn is_known(&self) -> bool {
        !matches!(self, EventKind::Unknown(_))
    }
}

impl Serialize for EventKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for EventKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Ok(Self::parse(&raw))
    }
}

/// 一条事件。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// 事件 id：重放去重的唯一依据。
    pub event_id: EventId,
    /// 会话内的单调序列号。
    pub seq: Seq,
    /// 相关会话；全局事件（presence）为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conv: Option<ConvId>,
    /// 事件类型。
    pub kind: EventKind,
    /// 各域自己的负载。
    pub payload: serde_json::Value,
    /// 服务端产生事件的时间（unix 毫秒）。
    pub emitted_at_ms: u64,
    /// 产生该事件的服务端契约版本。
    pub contract: ContractVersion,
}

impl EventEnvelope {
    /// 构造（服务端与测试夹具用）。
    #[must_use]
    pub fn new(
        event_id: EventId,
        seq: Seq,
        kind: EventKind,
        payload: serde_json::Value,
        emitted_at_ms: u64,
    ) -> Self {
        Self {
            event_id,
            seq,
            conv: None,
            kind,
            payload,
            emitted_at_ms,
            contract: CONTRACT_VERSION,
        }
    }

    /// 挂上会话。
    #[must_use]
    pub fn with_conv(mut self, conv: ConvId) -> Self {
        self.conv = Some(conv);
        self
    }
}

/// 订阅请求。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubscribeRequest {
    /// 从哪个游标之后开始；`None` = 从最新开始。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
    /// 只关心哪些类型；空 = 全部（`Unknown` 不参与过滤）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<EventKind>,
    /// 只关心哪些会话；空 = 全部。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub convs: Vec<ConvId>,
}

impl SubscribeRequest {
    /// 从最新开始，全部类型、全部会话。
    #[must_use]
    pub fn all() -> Self {
        Self {
            cursor: None,
            kinds: Vec::new(),
            convs: Vec::new(),
        }
    }

    /// 这条事件要不要投给调用方。
    #[must_use]
    pub fn accepts(&self, event: &EventEnvelope) -> bool {
        let kind_ok = self.kinds.is_empty() || self.kinds.contains(&event.kind);
        let conv_ok = match (&event.conv, self.convs.is_empty()) {
            (_, true) => true,
            (Some(conv), false) => self.convs.contains(conv),
            (None, false) => false,
        };
        kind_ok && conv_ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str) -> EventEnvelope {
        EventEnvelope::new(
            EventId::parse("evt-1").expect("event id"),
            Seq::new(3),
            EventKind::parse(kind),
            serde_json::json!({"text": "hi"}),
            1_700_000_000_000,
        )
    }

    #[test]
    fn unknown_kind_does_not_break_the_envelope() {
        let json = serde_json::to_value(event("brand.new.thing")).expect("ser");
        let back: EventEnvelope = serde_json::from_value(json).expect("de");
        assert_eq!(back.kind, EventKind::Unknown("brand.new.thing".to_string()));
        assert!(!back.kind.is_known());
        // 原样带回，服务端日志里能对上
        assert_eq!(back.kind.as_str(), "brand.new.thing");
    }

    #[test]
    fn envelope_round_trips_with_big_seq() {
        let mut ev = event("message.new");
        ev.seq = Seq::new(9_007_199_254_740_993);
        let json = serde_json::to_value(&ev).expect("ser");
        assert_eq!(json["seq"], "9007199254740993");
        assert_eq!(
            serde_json::from_value::<EventEnvelope>(json).expect("de"),
            ev
        );
    }

    #[test]
    fn filters_are_explicit() {
        let conv = ConvId::parse("d-a~b").expect("conv");
        let ev = event("message.new").with_conv(conv.clone());
        let mut req = SubscribeRequest::all();
        assert!(req.accepts(&ev));
        req.kinds = vec![EventKind::Typing];
        assert!(!req.accepts(&ev));
        req.kinds.clear();
        req.convs = vec![ConvId::parse("d-c~d").expect("conv")];
        assert!(!req.accepts(&ev));
        // 全局事件在"只订阅某些会话"时不该投
        assert!(!req.accepts(&event("presence")));
    }
}
