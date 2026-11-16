//! S4 / ADR-29：实时事件适配器——把线上 [`wxlike_core::Envelope`] 映射成规范
//! [`wxlike_types::EventEnvelope`]，让共享核心能消费宿主无关的实时流。
//!
//! 背景（2026-11-16 审计根因 2）：实时事件面此前在每个宿主里各写一遍
//! （桌面 Tauri 的 `start_recv_pump`、Flutter 的 MethodChannel），Rust 核心只
//! 共享请求/响应、不共享实时。`wxlike_net::Client::next_envelope()` 是宿主无关
//! 的 QUIC 客户端，但它产的是 `core::Envelope`（无 event_id/contract），不是
//! 规范 `EventEnvelope`。本模块是两者之间的**唯一映射点**——桌面与移动端都
//! 复用它，不再各写第二套（桌面 `forward_inbound` 在 S5 切到这里）。
//!
//! 映射规则：
//! * `event_id`：`msg_id` 非零 → hex（系统唯一，重放去重依据）；`msg_id` 为零
//!   （旧 v1 消息）→ 确定性 `conv:seq` 兜底（同一消息重放仍得同一 id）。
//! * `kind`：Recall→message.recalled；Read 回执→read.receipt；Custom 按内层
//!   kind 分流（call.*→call.signal 等）；其余→message.new。
//! * `payload`：整个 Envelope 的 JSON（保留 sender/body/receipt 等原文）。
//! * `contract`：本端 [`CONTRACT_VERSION`]（线上信封不带，由本端补）。

use wxlike_core::{Envelope, MsgBody, MsgKind, Receipt};
use wxlike_types::contract::CONTRACT_VERSION;
use wxlike_types::{ConvId, EventEnvelope, EventId, EventKind, Seq};

// 注意分层（dependency_tree 守卫）：本 crate 属客户端闭包，**不许**链接
// wxlike-net——net 含 axum（hub 服务端），链接它等于把 Web 框架塞进 APK。
// 所以这里只放纯映射（Envelope→EventEnvelope，仅需 core+types）；把 QUIC
// 客户端包装成 EventSource 的 QuicEventSource 放在**宿主装配层**
// （ui/src-tauri，本就链接 net），由宿主负责建连/重连。

/// 把一条线上信封映射成规范事件信封。
///
/// 纯函数、无副作用：相同的输入永远得到相同的输出（重放去重的前提）。
#[must_use]
pub fn envelope_to_event(env: &Envelope) -> EventEnvelope {
    let event_id = event_id_for(env);
    let kind = event_kind_for(env);
    let conv = ConvId::parse(env.conv_id.clone()).ok();
    let payload = serde_json::to_value(env).unwrap_or(serde_json::Value::Null);
    EventEnvelope {
        event_id,
        seq: Seq::new(env.seq),
        conv,
        kind,
        payload,
        emitted_at_ms: env.sent_at_ms,
        contract: CONTRACT_VERSION,
    }
}

/// 事件 id：msg_id 非零用 hex，零值用确定性 `conv:seq` 兜底。
fn event_id_for(env: &Envelope) -> EventId {
    let raw = if env.msg_id == [0u8; 16] {
        // 旧 v1 消息没有 msg_id：用 conv:seq 合成确定性 id（重放仍一致）。
        format!("{}:{}", env.conv_id, env.seq)
    } else {
        hex(&env.msg_id)
    };
    // hex 与 conv:seq 的字符都在 opaque_id 允许集内；兜底 parse 失败落占位。
    EventId::parse(raw).unwrap_or_default()
}

/// 事件类型映射。
fn event_kind_for(env: &Envelope) -> EventKind {
    // 已读回执优先（receipt 携带 Read）。
    if matches!(env.receipt, Some(Receipt::Read { .. })) {
        return EventKind::ReadReceipt;
    }
    match &env.body {
        MsgBody::Recall { .. } => EventKind::MessageRecalled,
        MsgBody::Custom { kind, .. } => match kind.as_str() {
            k if k.starts_with("call.") => EventKind::CallSignal,
            "typing" => EventKind::Typing,
            "presence" => EventKind::Presence,
            "member.changed" => EventKind::MemberChanged,
            _ => EventKind::MessageNew,
        },
        // Text / Bytes / Notice 都是新消息。
        _ => EventKind::MessageNew,
    }
}

/// 小写 hex（msg_id 16 字节 → 32 字符）。
fn hex(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap());
        out.push(char::from_digit((byte & 0xf) as u32, 16).unwrap());
    }
    out
}

/// 这条信封是不是新消息（供宿主决定要不要落库/通知）。
#[must_use]
pub fn is_message_kind(kind: &MsgKind) -> bool {
    matches!(kind, MsgKind::Text | MsgKind::Blob)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxlike_core::MsgBody;

    fn envelope(conv: &str, seq: u64, msg_id: [u8; 16], body: MsgBody) -> Envelope {
        Envelope {
            version: 2,
            conv_id: conv.to_string(),
            sender: "u-sender".to_string(),
            to: vec![],
            seq,
            kind: MsgKind::Text,
            body,
            sent_at_ms: 1_700_000_000_000,
            msg_id,
            quote_of: None,
            receipt: None,
        }
    }

    #[test]
    fn maps_text_to_message_new_with_hex_id() {
        let mut id = [0u8; 16];
        id[0] = 0xab;
        id[15] = 0xcd;
        let env = envelope("d-alice~bob", 42, id, MsgBody::Text("hi".into()));
        let event = envelope_to_event(&env);
        assert_eq!(event.kind, EventKind::MessageNew);
        assert_eq!(event.seq, Seq::new(42));
        assert_eq!(event.emitted_at_ms, 1_700_000_000_000);
        assert_eq!(event.contract, CONTRACT_VERSION);
        assert!(
            event.event_id.as_str().starts_with("ab"),
            "{}",
            event.event_id
        );
        assert!(event.event_id.as_str().ends_with("cd"));
        assert_eq!(event.conv.unwrap().as_str(), "d-alice~bob");
        // payload 保留原文。
        assert_eq!(event.payload["sender"], "u-sender");
    }

    #[test]
    fn zero_msg_id_falls_back_to_deterministic_conv_seq() {
        let env = envelope("d-alice~bob", 7, [0u8; 16], MsgBody::Text("legacy".into()));
        let a = envelope_to_event(&env);
        let b = envelope_to_event(&env);
        // 重放同一消息得到同一 id（去重前提）。
        assert_eq!(a.event_id, b.event_id);
        assert_eq!(a.event_id.as_str(), "d-alice~bob:7");
    }

    #[test]
    fn recall_maps_to_message_recalled() {
        let env = envelope("c-1", 1, [1u8; 16], MsgBody::Recall { target: [2u8; 16] });
        assert_eq!(envelope_to_event(&env).kind, EventKind::MessageRecalled);
    }

    #[test]
    fn read_receipt_maps_to_read_receipt() {
        let mut env = envelope("c-1", 1, [1u8; 16], MsgBody::Text("x".into()));
        env.receipt = Some(Receipt::Read {
            up_to_msg_id: [3u8; 16],
        });
        assert_eq!(envelope_to_event(&env).kind, EventKind::ReadReceipt);
    }

    #[test]
    fn custom_call_maps_to_call_signal() {
        let env = envelope(
            "c-1",
            1,
            [1u8; 16],
            MsgBody::Custom {
                kind: "call.invite".into(),
                payload: serde_json::json!({"sdp": "..."}),
            },
        );
        assert_eq!(envelope_to_event(&env).kind, EventKind::CallSignal);
    }

    #[test]
    fn distinct_messages_get_distinct_ids() {
        let a = envelope_to_event(&envelope("c-1", 1, [1u8; 16], MsgBody::Text("a".into())));
        let b = envelope_to_event(&envelope("c-1", 2, [2u8; 16], MsgBody::Text("b".into())));
        assert_ne!(a.event_id, b.event_id);
    }
}
