// support 碎片 2/6：事件发射与行渲染（emit_core、forward_inbound、
// ui_event_type、row_to_message、iso_ms 等）。由 support.inc 按序 include。

// ---------------------------------------------------------------------------
// 事件
// ---------------------------------------------------------------------------

/// 以新契约的 EventEnvelope 形状发 `core://event`，并把信封登记进补发日志。
///
/// 信封里带 `cursor`（`EventCursor` 形状）：调用方每收到一条就把它记下来，
/// 重订阅时原样带回，[`subscribe`] 据此补发漏掉的事件。
fn emit_core<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    event_type: &str,
    aggregate_id: &str,
    payload: Value,
) {
    use tauri::Emitter as _;
    let core = app.state::<CoreState>();
    let seq = core.event_seq.fetch_add(1, Ordering::Relaxed);
    let now = wxlike_core::types::now_ms();
    let envelope = json!({
        // epoch 参与 id：序号会在重启后从 1 重新数，只有 epoch 能保证 id 全局唯一。
        "event_id": format!("ev-{}-{seq}", core.epoch),
        "aggregate_id": aggregate_id,
        "aggregate_version": "1",
        "event_type": event_type,
        "server_time_ms": iso_ms(now as i64),
        "cursor": { "stream_id": EVENT_STREAM_ID, "cursor": core.cursor_at(seq) },
        "payload": payload,
    });
    core.record(seq, envelope.clone());
    let _ = app.emit("core://event", envelope);
}


/// 收包泵的转发钩子：一条入站消息 → `message.created`/`message.updated` +
/// `conversation.upsert`。`me` 用于判定方向；None 时全部按入站处理。
///
/// R2（第二轮审计）：分类用**原始线上信封** `env`，渲染行 `payload` 只做 UI
/// 投影。此前 S5a 试图把渲染行 serde 重建成 Envelope——形状根本不同（渲染层
/// 拍平了 body、丢了 version/to、msg_id 转了 hex），重建必然失败、永远落
/// fallback，是死代码（探针实证）。现在直接拿收包泵手里的 env 调适配器，
/// recall→message.updated 真实生效。
///
/// 对 crate 外可见：补发/游标测试要驱动**真实生产者**（收包泵就是调它），
/// 而不是自己拼一份事件形状。
pub fn forward_inbound<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    env: &wxlike_core::Envelope,
    payload: &Value,
    me: Option<&str>,
) {
    let conv_id = payload
        .get("conv_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if conv_id.is_empty() || payload.get("kind").and_then(Value::as_str) == Some("typing") {
        return; // 控制帧与坏事件不进新 UI 的状态机
    }
    let event_type = ui_event_type(env);
    let message = row_to_message(payload, me.unwrap_or(""));
    emit_core(app, event_type, conv_id, message.clone());
    // 会话卡片：标题先用缓存/conv_id，列表 action 会补全真实标题。
    let conversation = json!({
        "conv_id": conv_id,
        "kind": conv_kind(conv_id),
        "title": conv_id,
        "unread": 1,
        "pinned": false,
        "muted": false,
        "last_preview": preview_of(payload),
        "last_at_ms": message["created_at_ms"].clone(),
        "draft": "",
    });
    emit_core(app, "conversation.upsert", conv_id, conversation);
}

/// R2：把**原始线上信封**经 S4 适配器派生成 UI 投影事件类型。
///
/// 复用 `wxlike_client_native::realtime::envelope_to_event`（wire→canonical 的
/// 唯一映射点），不在这里手写第二套 kind 判断。规范 EventKind → UI 投影类型：
/// message.recalled→message.updated（撤回是消息变更），其余→message.created。
/// UI 投影类型与规范事件类型是两层概念，这里只做名称映射，不改 payload 形状。
fn ui_event_type(env: &wxlike_core::Envelope) -> &'static str {
    use wxlike_client_native::realtime::envelope_to_event;
    use wxlike_types::EventKind;
    match envelope_to_event(env).kind {
        EventKind::MessageRecalled => "message.updated",
        _ => "message.created",
    }
}

fn conv_kind(conv_id: &str) -> &'static str {
    if conv_id.starts_with("d-") {
        "direct"
    } else {
        "group"
    }
}

/// 直聊会话的对端（conv 形如 `d-{a}~{b}`，另一端是自己时取空）。
fn peer_of(conv_id: &str, me: &str) -> Option<String> {
    let rest = conv_id.strip_prefix("d-")?;
    rest.split('~').find(|p| *p != me).map(str::to_string)
}

fn preview_of(payload: &Value) -> String {
    match payload
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "text" | "e2ee" => payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(40)
            .collect(),
        "image" => "[图片]".into(),
        "msg.voice" => "[语音]".into(),
        "msg.file" => "[文件]".into(),
        "msg.sticker" => "[表情]".into(),
        "msg.video" => "[视频]".into(),
        "wallet.redpacket" => "[红包]".into(),
        "system" | "notice" => payload
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(40)
            .collect(),
        other => format!("[{other}]"),
    }
}

/// 本地库/服务端历史的行（render.rs payload 形状）→ UI 的 ChatMessage。
fn row_to_message(row: &Value, me: &str) -> Value {
    let sender = row
        .get("sender")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let direction = if sender == me { "out" } else { "in" };
    let kind = row.get("kind").and_then(Value::as_str).unwrap_or("text");
    let body = match kind {
        "text" | "e2ee" => {
            json!({ "kind": "text", "text": row.get("text").and_then(Value::as_str).unwrap_or("") })
        }
        "image" => {
            json!({ "kind": "image", "media_id": row.get("media_id").and_then(Value::as_str).unwrap_or("") })
        }
        "system" | "notice" | "recall" => {
            json!({ "kind": "system", "text": row.get("text").and_then(Value::as_str).unwrap_or("") })
        }
        custom => custom_body(custom, &row.get("payload").cloned().unwrap_or(json!({}))),
    };
    json!({
        "msg_id": row.get("msg_id").and_then(Value::as_str).unwrap_or(""),
        "conv_id": row.get("conv_id").and_then(Value::as_str).unwrap_or(""),
        "seq": row.get("seq").map(|v| v.to_string()).unwrap_or_else(|| "0".into()),
        "sender": { "user_id": sender, "name": sender },
        "direction": direction,
        "body": body,
        "state": "SERVER_ACCEPTED",
        "created_at_ms": iso_ms(row.get("sent_at_ms").and_then(Value::as_i64).unwrap_or_default()),
    })
}

/// 毫秒时间戳 → ISO-8601（UI 里 created_at_ms / last_at_ms 是字符串）。
/// 自定义消息体（msg.voice/file/sticker/video 等）→ UI 的 MessageBody。
fn custom_body(custom: &str, p: &Value) -> Value {
    match custom {
        "msg.voice" => json!({
            "kind": "voice",
            "media_id": p.get("media_id").and_then(Value::as_str).unwrap_or(""),
            "duration_ms": p.get("duration_ms").cloned().unwrap_or(json!(0)),
        }),
        "msg.file" => json!({
            "kind": "file",
            "media_id": p.get("media_id").and_then(Value::as_str).unwrap_or(""),
            "name": p.get("name").and_then(Value::as_str).unwrap_or(""),
            "size": p.get("size").cloned().unwrap_or(json!(0)),
        }),
        "msg.sticker" => json!({
            "kind": "sticker",
            "sticker": p.get("id").and_then(Value::as_str).unwrap_or(""),
            "media_id": p.get("media_id").and_then(Value::as_str).unwrap_or(""),
        }),
        "msg.video" => json!({
            "kind": "link",
            "media_id": p.get("media_id").and_then(Value::as_str).unwrap_or(""),
            "text": p.get("caption").and_then(Value::as_str).unwrap_or(""),
        }),
        _ => json!({ "kind": "system", "text": "[ unsupported message ]" }),
    }
}

/// iso_ms 的 civil-date 算法常量（Howard Hinnant `date_algorithms`）。
const DAYS_TO_CIVIL_EPOCH: i64 = 719_468;
const ERA_DAYS: i64 = 146_097;
const ERA_YEAR_DAYS: i64 = 1_460;
const CENTURY_DAYS: i64 = 36_524;
const ERA_CENTURY_SHIFT: i64 = 146_096;
const YEARS_PER_ERA: i64 = 400;
const MONTHS_PER_ERA_DENOM: i64 = 153;

fn iso_ms(ms: i64) -> String {
    // 没有 chrono：手工换算（UTC）。够用——只用于展示与排序。
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // 1970-01-01 起的 civil date（Howard Hinnant 的算法，无依赖版）
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
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

fn cursor_usize(params: &Value) -> usize {
    match params.get("cursor") {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as usize,
        _ => 0,
    }
}

fn cursor_i64(params: &Value) -> i64 {
    match params.get("cursor") {
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        _ => 0,
    }
}

