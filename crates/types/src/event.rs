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
///
/// 反序列化**宽进**：`contracts/common/envelope.schema.json` 的兼容窗口规则
/// 是"新增字段必须可忽略、旧端不 panic"，所以缺 `contract` / `payload` /
/// `emitted_at_ms` / `seq` 甚至 `event_id`（线上信封那一形状没有它）都能解，
/// 只有**值本身是垃圾**（kind 不是字符串、seq 非数字）才拒绝。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// 事件 id：重放去重的唯一依据。缺省落到 [`EventId::default`]——
    /// 占位 id 意味着不可去重，调用方应按不可靠事件处理。
    #[serde(default)]
    pub event_id: EventId,
    /// 会话内的单调序列号。缺省 0；线上两种承载（十进制字符串 / 整数）都收。
    #[serde(default, deserialize_with = "de_seq_lenient")]
    pub seq: Seq,
    /// 相关会话；全局事件（presence）为 `None`。线上名字 `conv_id` 也认。
    #[serde(default, alias = "conv_id", skip_serializing_if = "Option::is_none")]
    pub conv: Option<ConvId>,
    /// 事件类型。缺 `kind` 是不符合 schema 的输入，拒绝。
    pub kind: EventKind,
    /// 各域自己的负载。缺省 `null`。
    #[serde(default)]
    pub payload: serde_json::Value,
    /// 服务端产生事件的时间（unix 毫秒）。缺省 0（=未知）。
    #[serde(default)]
    pub emitted_at_ms: u64,
    /// 产生该事件的服务端契约版本。缺省本端版本。
    #[serde(default)]
    pub contract: ContractVersion,
}

/// seq 的宽进反序列化：schema 允许"整数或十进制字符串"两种承载
/// （>2^53 在 HTTP 表面必须是字符串；QUIC/bincode 表面恒为原生 u64）。
///
/// 直接改 [`Seq`] 会破坏"JSON 上序列号一律走字符串"的三端约定
/// （`Seq` 自己的测试钉着数值必须拒绝），所以宽进只开在信封这一层。
fn de_seq_lenient<'de, D>(deserializer: D) -> Result<Seq, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = serde_json::Value::deserialize(deserializer)?;
    match raw {
        serde_json::Value::String(text) => text
            .parse::<u64>()
            .map(Seq::new)
            .map_err(|_| serde::de::Error::custom("seq 必须是十进制无符号整数")),
        serde_json::Value::Number(number) => number
            .as_u64()
            .map(Seq::new)
            .ok_or_else(|| serde::de::Error::custom("seq 必须是非负整数")),
        _ => Err(serde::de::Error::custom("seq 必须是数字或数字字符串")),
    }
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

    /// schema 兼容窗口（`contracts/common/envelope.schema.json`）：
    /// 缺字段不整体失败、未知字段忽略、未知枚举收敛为 Unknown。
    #[test]
    fn lenient_envelope_accepts_schema_conformant_wire_shapes() {
        // 线上信封形状：没有 event_id / payload / emitted_at_ms / contract，
        // seq 是整数，conv 的线上名字是 conv_id，且带未知字段
        let wire = serde_json::json!({
            "version": 1,
            "conv_id": "d-a~b",
            "sender": "a",
            "to": ["b"],
            "seq": 7,
            "kind": "Text",
            "sent_at_ms": 1_700_000_000_000_i64,
            "v9_shiny_field": {"nested": true}
        });
        let ev: EventEnvelope = serde_json::from_value(wire).expect("schema 合法信封必须可解");
        assert_eq!(ev.conv.as_ref().map(ConvId::as_str), Some("d-a~b"));
        assert_eq!(ev.seq.get(), 7, "整数 seq 也收");
        assert_eq!(ev.kind, EventKind::Unknown("Text".to_string()));
        assert_eq!(ev.payload, serde_json::Value::Null);
        assert_eq!(ev.emitted_at_ms, 0);
        assert_eq!(ev.contract, CONTRACT_VERSION);
        assert_eq!(ev.event_id.as_str(), "evt-unknown", "占位 id，不可去重");

        // 大整数走字符串承载：同样可解（fixture FX-bigint-seq 的形状）
        let big = serde_json::json!({"kind": "message.new", "seq": "9007199254740993"});
        let ev: EventEnvelope = serde_json::from_value(big).expect("字符串 seq 必须可解");
        assert_eq!(ev.seq.get(), 9_007_199_254_740_993);
    }

    /// 宽进不等于不设防：值本身是垃圾（seq 非数字、kind 缺失）必须拒绝，
    /// 否则脏数据会以"未知字段"的名义静默通过。
    #[test]
    fn lenient_envelope_still_rejects_garbage() {
        for garbage in [
            serde_json::json!({"kind": "message.new", "seq": "abc"}),
            serde_json::json!({"kind": "message.new", "seq": -3}),
            serde_json::json!({"kind": "message.new", "seq": [1]}),
            serde_json::json!({"seq": "1"}),
            serde_json::json!({"kind": 42, "seq": "1"}),
        ] {
            assert!(
                serde_json::from_value::<EventEnvelope>(garbage.clone()).is_err(),
                "{garbage} 是垃圾输入，必须拒绝"
            );
        }
    }
}
