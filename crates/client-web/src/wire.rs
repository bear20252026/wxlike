//! 浏览器侧的 bincode wire 镜像。
//!
//! `/ws` 只有一种线上格式：`bincode` 1.3 编码的 [`crate::wire::WireFrame`]
//! （`crates/net/src/wire.rs` / `crates/core/src/types.rs`）。浏览器不能复用
//! `wxlike-core`（它带 rusqlite/vodozemac），所以这里按字段位置逐项镜像；
//! 任何字段顺序、枚举序号、`u64` 长度前缀的漂移都由 `ci/jobs/contract/
//! check_wire_variants.py` 与 `wire_matches_server` 测试钉住。
//!
//! 这个模块**不实现业务规则**：解码出来的 `Envelope` 只做一层 UI 投影
//! （消息形状），排序/未读/权限仍在服务端。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use wxlike_types::contract::CONTRACT_VERSION;
use wxlike_types::{ConvId, EventEnvelope, EventId, EventKind, Seq};

/// `wxlike_core::types::PROTOCOL_VERSION`.
pub const ENVELOPE_VERSION: u8 = 2;
/// `/ws` 的 protocol v3（G03-04）。2 仍然被服务端接受；浏览器要以 v3 发
/// `ClientHello`，才能拿到 `Auth` 后发 `Subscribe/Resume`。
pub const PROTOCOL_V3: u8 = 3;
/// `proto::Capability::Resume`.
pub const CAP_RESUME: u32 = 2;

/// 帧上限：避免畸形长度前缀让浏览器先分配一个巨大数组。
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// 16 字节消息 id。
pub type MsgId = [u8; 16];

/// `MsgKind`：bincode 写的是声明序号。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgKind {
    /// 文本。
    Text,
    /// 二进制附件。
    Blob,
    /// 系统消息。
    System,
}

/// `MsgBody`：变体序号 0..=4，字段顺序必须与 `crates/core/src/types.rs` 一致。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum MsgBody {
    /// 文本。
    Text(String),
    /// 原始字节。
    Bytes(Vec<u8>),
    /// 提示。
    Notice(String),
    /// 撤回目标。
    Recall {
        /// 被撤回的消息 id。
        target: MsgId,
    },
    /// 结构化业务消息；payload 在 bincode 里是 JSON 文本（`json_bytes`）。
    Custom {
        /// 业务 kind，如 `msg.voice` / `wallet.redpacket`。
        kind: String,
        /// JSON 负载。
        #[serde(with = "json_text")]
        payload: Value,
    },
}

/// `Receipt`：Delivered=0 / Read=1。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Receipt {
    /// 已送达。
    Delivered,
    /// 已读到该消息。
    Read {
        /// 已读上界。
        up_to_msg_id: MsgId,
    },
}

/// `crates/core/src/types.rs` 的 Envelope 镜像。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Envelope {
    /// 信封版本（本端只发/收 2）。
    pub version: u8,
    /// 会话 id。
    pub conv_id: String,
    /// 发送者 user id。
    pub sender: String,
    /// 接收者；空 = 会话广播。
    pub to: Vec<String>,
    /// 会话内序号。
    pub seq: u64,
    /// 消息大类。
    pub kind: MsgKind,
    /// 消息体。
    pub body: MsgBody,
    /// 发送时间（unix ms）。
    pub sent_at_ms: u64,
    /// 客户端生成的消息 id。
    pub msg_id: MsgId,
    /// 引用消息。
    pub quote_of: Option<MsgId>,
    /// 送达/已读回执。
    pub receipt: Option<Receipt>,
}

/// `/ws` 控制帧。变体序号 **必须**与 `crates/net/src/wire.rs` 的声明顺序一致。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum WireFrame {
    /// v2 首帧：只带凭据。
    Hello {
        /// HTTP 登录拿到的 Bearer token。
        token: String,
    },
    /// 服务端握手应答。
    HelloAck {
        /// 服务端解析出的 user id。
        user_id: String,
        /// 本次连接绑定的 session id。
        session_id: String,
    },
    /// 服务端拒绝握手。
    HelloReject {
        /// 拒绝原因（不含 token）。
        reason: String,
    },
    /// 内容帧。
    Msg(Envelope),
    /// 保活。
    Ping,
    /// v3 首帧：设备先认领自己，身份仍由后续 `Hello` 的 token 解析。
    ClientHello {
        /// 设备 id。
        device_id: String,
        /// 声明支持的最高协议版本。
        protocol: u8,
        /// capability bitmask。
        caps: u32,
    },
    /// v3 认证应答。
    Auth {
        /// user id。
        user_id: String,
        /// device id。
        device_id: String,
        /// session id。
        session_id: String,
        /// 协商出的版本。
        version: u8,
    },
    /// 订阅范围。
    Subscribe {
        /// opaque scopes，如 `conv:<id>`。
        scopes: Vec<String>,
    },
    /// 服务端下发范围提示。
    Event {
        /// 会话 id。
        conv_id: String,
        /// 起始序号。
        from_seq: u64,
        /// 结束序号。
        to_seq: u64,
        /// 是否缺口过大。
        gap: bool,
    },
    /// 客户端确认已落库。
    Ack {
        /// 会话 id。
        conv_id: String,
        /// 已落库游标。
        stored_cursor: u64,
    },
    /// 客户端断线续传。
    Resume {
        /// 会话 id。
        conv_id: String,
        /// 本地游标。
        cursor: u64,
    },
    /// hub 间转发帧；浏览器永远不会发送或使用，镜像只为枚举序号一致。
    RoutedMsg {
        /// 源 hub id。
        source_hub: String,
        /// 原始内容信封。
        envelope: Envelope,
    },
    /// hub 间投递回执；浏览器永远不会发送或使用。
    DeliveryReceipt {
        /// 源 outbox 的消息键。
        msg_key: String,
        /// 收件人。
        recipient: String,
    },
}

impl WireFrame {
    /// bincode 编码。
    ///
    /// # Errors
    /// 任何字段超过 bincode 限制时返回文本错误。
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        bincode::serialize(self).map_err(|err| format!("bincode encode: {err}"))
    }

    /// bincode 解码，并挡掉超大帧。
    ///
    /// # Errors
    /// 超限、截断、尾部多余字节或字段类型不符。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(format!("frame {} bytes over cap", bytes.len()));
        }
        bincode::deserialize(bytes).map_err(|err| format!("bincode decode: {err}"))
    }
}

/// `json_bytes`：`serde_json::Value` 在 bincode 里以 JSON 文本承载。
mod json_text {
    use serde::{de, Deserialize, Deserializer, Serializer};
    use serde_json::Value;

    pub fn serialize<S>(value: &Value, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        serde_json::from_str(&text).map_err(de::Error::custom)
    }
}

/// 把线上信封映射成核心/UI 共用的规范事件。
///
/// `self_user` 来自 `HelloAck`，用于把 UI 的消息方向标成 `out`/`in`。映射规则
/// 与 `crates/client-native/src/realtime.rs` 一致，避免两个客户端各写一套。
#[must_use]
pub fn envelope_to_event(env: &Envelope, self_user: Option<&str>) -> Option<EventEnvelope> {
    // typing 是瞬态控制帧，不进状态机。
    if matches!(&env.body, MsgBody::Custom { kind, .. } if kind == "typing") {
        return None;
    }
    let event_id = event_id_for(env);
    let conv = ConvId::parse(env.conv_id.clone()).ok();
    let payload = ui_message(env, self_user);
    Some(EventEnvelope {
        event_id,
        seq: Seq::new(env.seq),
        conv,
        kind: event_kind_for(env),
        payload,
        emitted_at_ms: env.sent_at_ms,
        contract: CONTRACT_VERSION,
    })
}

fn event_id_for(env: &Envelope) -> EventId {
    let raw = if env.msg_id == [0u8; 16] {
        format!("{}:{}", env.conv_id, env.seq)
    } else {
        hex(&env.msg_id)
    };
    EventId::parse(raw).unwrap_or_default()
}

fn event_kind_for(env: &Envelope) -> EventKind {
    if matches!(env.receipt, Some(Receipt::Read { .. })) {
        return EventKind::ReadReceipt;
    }
    match &env.body {
        MsgBody::Recall { .. } => EventKind::MessageRecalled,
        MsgBody::Custom { kind, .. } => match kind.as_str() {
            k if k.starts_with("call.") => EventKind::CallSignal,
            "presence" => EventKind::Presence,
            "member.changed" => EventKind::MemberChanged,
            _ => EventKind::MessageNew,
        },
        _ => EventKind::MessageNew,
    }
}

/// 把线上信封投影成桌面 UI 的 `ChatMessage` 形状（`core-contract.ts`）。
fn ui_message(env: &Envelope, self_user: Option<&str>) -> Value {
    let body = ui_body(env);
    let direction = if self_user.is_some_and(|me| me == env.sender) {
        "out"
    } else {
        "in"
    };
    json!({
        "msg_id": hex(&env.msg_id),
        "conv_id": env.conv_id,
        "seq": env.seq.to_string(),
        "sender": { "user_id": env.sender, "name": env.sender },
        "direction": direction,
        "body": body,
        "state": "SERVER_ACCEPTED",
        "created_at_ms": iso_ms(env.sent_at_ms),
    })
}

fn ui_body(env: &Envelope) -> Value {
    match &env.body {
        MsgBody::Text(text) => json!({ "kind": "text", "text": text }),
        MsgBody::Bytes(_) => json!({ "kind": "text", "text": "[二进制消息]" }),
        MsgBody::Notice(text) => json!({ "kind": "system", "text": text }),
        MsgBody::Recall { .. } => json!({ "kind": "system", "text": "消息已撤回" }),
        MsgBody::Custom { kind, payload } => ui_custom_body(kind, payload),
    }
}

fn ui_custom_body(kind: &str, payload: &Value) -> Value {
    let text = payload.get("text").and_then(Value::as_str).unwrap_or("");
    match kind {
        "e2ee.dm" => json!({ "kind": "text", "text": "[加密消息]" }),
        "wallet.redpacket" => json!({ "kind": "system", "text": "[红包]" }),
        "wallet.transfer" => json!({ "kind": "system", "text": "[转账]" }),
        "msg.voice" => json!({
            "kind": "voice",
            "media_id": payload.get("media_id").and_then(Value::as_str).unwrap_or(""),
            "duration_ms": payload.get("duration_ms").cloned().unwrap_or(json!(0)),
        }),
        "msg.file" => json!({
            "kind": "file",
            "media_id": payload.get("media_id").and_then(Value::as_str).unwrap_or(""),
            "name": payload.get("name").and_then(Value::as_str).unwrap_or(""),
            "size": payload.get("size").cloned().unwrap_or(json!(0)),
        }),
        "msg.sticker" => json!({
            "kind": "sticker",
            "sticker": payload.get("id").and_then(Value::as_str).unwrap_or(""),
            "media_id": payload.get("media_id").and_then(Value::as_str).unwrap_or(""),
        }),
        "msg.video" => json!({
            "kind": "link",
            "media_id": payload.get("media_id").and_then(Value::as_str).unwrap_or(""),
            "text": payload.get("caption").and_then(Value::as_str).unwrap_or(text),
        }),
        "msg.forward" | "msg.merge" => json!({ "kind": "system", "text": "[转发]" }),
        other => json!({ "kind": "system", "text": format!("[{other}]") }),
    }
}

/// 小写 hex（16 字节 → 32 字符）。
fn hex(bytes: &MsgId) -> String {
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

/// unix ms → ISO-8601 UTC。纯函数，原生测试与 wasm 都可用。
pub fn iso_ms(ms: u64) -> String {
    const DAYS_TO_CIVIL_EPOCH: i64 = 719_468;
    const ERA_DAYS: i64 = 146_097;
    const ERA_YEAR_DAYS: i64 = 1_460;
    const CENTURY_DAYS: i64 = 36_524;
    const ERA_CENTURY_SHIFT: i64 = 146_096;
    const YEARS_PER_ERA: i64 = 400;
    const MONTHS_PER_ERA_DENOM: i64 = 153;

    let millis = ms as i64;
    let secs = millis.div_euclid(1000);
    let rem = millis.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let z = days + DAYS_TO_CIVIL_EPOCH;
    let era = z.div_euclid(ERA_DAYS);
    let doe = z.rem_euclid(ERA_DAYS);
    let yoe = (doe - doe / ERA_YEAR_DAYS + doe / CENTURY_DAYS - doe / ERA_CENTURY_SHIFT) / 365;
    let y = yoe + era * YEARS_PER_ERA;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / MONTHS_PER_ERA_DENOM;
    let d = doy - (MONTHS_PER_ERA_DENOM * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}.{rem:03}Z")
}

/// 规范 `EventEnvelope` → 桌面 UI 的 `EventEnvelope` 形状（JSON 文本）。
///
/// Rust 侧的字段是 `kind/conv/emitted_at_ms`，TS 侧是
/// `event_type/aggregate_id/server_time_ms`；映射只发生在 WASM 出口。
#[must_use]
pub fn ui_event_json(event: &EventEnvelope) -> String {
    let conv_id = event.conv.as_ref().map(|conv| conv.as_str()).unwrap_or("");
    let seq = event.seq.get();
    serde_json::json!({
        "event_id": event.event_id.as_str(),
        "aggregate_id": conv_id,
        "aggregate_version": "1",
        "event_type": ui_event_type(&event.kind),
        "server_time_ms": iso_ms(event.emitted_at_ms),
        "cursor": { "stream_id": "event", "cursor": seq.to_string() },
        "payload": event.payload,
    })
    .to_string()
}

/// 桌面 UI 的事件类型（`core-contract.ts` 的 `event_type`）。
#[must_use]
pub fn ui_event_type(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::MessageRecalled => "message.updated",
        EventKind::ReadReceipt => "read.receipt",
        EventKind::Typing => "typing",
        EventKind::Presence => "presence",
        EventKind::CallSignal => "call.signal",
        EventKind::MemberChanged => "member.changed",
        EventKind::CursorAdvanced => "cursor.advanced",
        _ => "message.created",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_env() -> Envelope {
        Envelope {
            version: ENVELOPE_VERSION,
            conv_id: "d-alice~bob".into(),
            sender: "u-alice".into(),
            to: vec![],
            seq: 42,
            kind: MsgKind::Text,
            body: MsgBody::Text("hello".into()),
            sent_at_ms: 1_700_000_000_000,
            msg_id: [0xab; 16],
            quote_of: None,
            receipt: None,
        }
    }

    #[test]
    fn frames_roundtrip() {
        for frame in [
            WireFrame::ClientHello {
                device_id: "web-1".into(),
                protocol: PROTOCOL_V3,
                caps: CAP_RESUME,
            },
            WireFrame::Hello {
                token: "tok".into(),
            },
            WireFrame::Subscribe {
                scopes: vec!["conv:*".into(), "conv:g-1".into()],
            },
            WireFrame::Resume {
                conv_id: "g-1".into(),
                cursor: 41,
            },
            WireFrame::Msg(text_env()),
        ] {
            let bytes = frame.to_bytes().expect("encode");
            let back = WireFrame::from_bytes(&bytes).expect("decode");
            assert_eq!(back, frame);
        }
    }

    #[test]
    fn text_maps_to_message_event() {
        let event = envelope_to_event(&text_env(), Some("u-alice")).expect("event");
        assert_eq!(event.kind, EventKind::MessageNew);
        assert_eq!(event.seq, Seq::new(42));
        assert_eq!(event.payload["conv_id"], "d-alice~bob");
        assert_eq!(event.payload["direction"], "out");
        assert_eq!(event.payload["body"]["kind"], "text");
        assert_eq!(event.payload["body"]["text"], "hello");
    }

    #[test]
    fn ui_event_json_uses_the_desktop_contract_shape() {
        let event = envelope_to_event(&text_env(), Some("u-alice")).expect("event");
        let json: Value = serde_json::from_str(&ui_event_json(&event)).expect("json");
        assert_eq!(json["event_type"], "message.created");
        assert_eq!(json["aggregate_id"], "d-alice~bob");
        assert_eq!(json["payload"]["msg_id"], hex(&[0xab; 16]));
        assert_eq!(json["payload"]["body"]["text"], "hello");
    }

    #[test]
    fn typing_is_dropped_before_the_ui() {
        let mut env = text_env();
        env.body = MsgBody::Custom {
            kind: "typing".into(),
            payload: json!({}),
        };
        assert!(envelope_to_event(&env, None).is_none());
    }

    #[test]
    fn zero_msg_id_uses_deterministic_conv_seq() {
        let mut env = text_env();
        env.msg_id = [0u8; 16];
        let event = envelope_to_event(&env, None).expect("event");
        assert_eq!(event.event_id.as_str(), "d-alice~bob:42");
    }
}
