// support 碎片 3/6：游标解析与订阅补发（Resume/Replay、subscribe、
// resume_at/take_replay）。由 support.inc 按序 include。

/// 游标解析结果。
enum Resume {
    /// 没带游标 = 冷订阅：只收此后的事件，不补发。
    Fresh,
    /// 带了一个当前不认识的形状（非本流 id、解析不出序号）。
    Invalid,
    /// 正常游标：`{epoch, seq}`，epoch 可能来自上一个进程。
    At {
        epoch: String,
        seq: u64,
        token: String,
    },
}

/// 游标只带了一个裸序号（`3` / `"3"`）：没有 epoch 可依，按"当前这条流"解释。
fn at_head(core: &CoreState, seq: u64) -> Resume {
    Resume::At {
        epoch: core.epoch.clone(),
        seq,
        token: core.cursor_at(seq),
    }
}

/// 裸序号写法：十进制字符串才认（负数、小数、非数字都是 None）。
fn parse_seq(text: &str) -> Option<u64> {
    text.parse::<u64>().ok()
}

/// JSON 值里的裸序号：数字或十进制字符串。
fn bare_seq(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(parse_seq))
}

/// `"<epoch>:<seq>"`；切不出 `:` 就当裸序号（`"3"`）。
fn parse_text_cursor(core: &CoreState, text: &str) -> Resume {
    // epoch 里有 '-' 但没有 ':'，所以按最后一个 ':' 切。
    let Some((epoch, seq)) = text.rsplit_once(':') else {
        return parse_seq(text).map_or(Resume::Invalid, |seq| at_head(core, seq));
    };
    let Some(seq) = parse_seq(seq) else {
        return Resume::Invalid;
    };
    Resume::At {
        epoch: epoch.to_string(),
        seq,
        token: text.to_string(),
    }
}

/// 对象写法：契约的 `EventCursor`（`{stream_id, cursor}`）或 `{epoch, seq}`。
fn parse_map_cursor(core: &CoreState, map: &serde_json::Map<String, Value>) -> Resume {
    // 不是本桥那条流的游标，补不了也不能假装补。
    if map
        .get("stream_id")
        .and_then(Value::as_str)
        .is_some_and(|stream| stream != EVENT_STREAM_ID)
    {
        return Resume::Invalid;
    }
    if let Some(inner) = map.get("cursor") {
        return parse_cursor(core, inner);
    }
    let Some(seq) = map.get("seq").and_then(bare_seq) else {
        return Resume::Invalid;
    };
    let epoch = map
        .get("epoch")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| core.epoch.clone());
    Resume::At {
        token: format!("{epoch}:{seq}"),
        epoch,
        seq,
    }
}

/// 解析调用方带回的游标。兼容这些写法（都不带也合法）：
/// `null` / `3` / `"3"` / `"<epoch>:<seq>"` /
/// `{ "stream_id": "core", "cursor": "<epoch>:<seq>" }`（契约 `EventCursor`）/
/// `{ "epoch": ..., "seq": ... }`。
fn parse_cursor(core: &CoreState, raw: &Value) -> Resume {
    match raw {
        // 没带游标 = 冷订阅。
        Value::Null => Resume::Fresh,
        Value::Number(_) => bare_seq(raw).map_or(Resume::Invalid, |seq| at_head(core, seq)),
        Value::String(text) => parse_text_cursor(core, text),
        Value::Object(map) => parse_map_cursor(core, map),
        _ => Resume::Invalid,
    }
}

/// `core_subscribe` 的实现：按游标补发（resume）。
///
/// 契约：入参是调用方上次记下的游标（可为 None，兼容写法见 [`parse_cursor`]），
/// 返回订阅回执 JSON。补发是**真的重新 `emit` 一遍 `core://event`**，所以现有
/// 前端一行都不用改（它的 `applyEvent` 按 id 幂等合并，重复投影会收敛）。
///
/// 为什么不是"全量重放"——#296 的诚实边界，写在这里：
/// * 事件只活在 [`EventJournal`] 这块内存环形日志里。本地 SQLite 存的是
///   消息/会话这些**投影**，不是事件流；进程重启后日志归零，没有历史事件可补。
///   硬凑"重放"只能把投影 diff 成事件，那是第二套业务逻辑，正是本文件开头禁止的事。
/// * 所以跨重启如实报 `gap: true, reason: "stream_reset"` 并附上要重拉的 action
///   列表，由调用方走 `messaging.conversations.list` / `messaging.messages.page`
///   全量刷新（也正是冷启动本来就走的路径）。同进程内日志被挤掉的窗口同理报
///   `journal_truncated`，能补的那段照样补。
/// * 不丢：同进程内的卸载重订（webview 刷新、断线重连）漏掉的事件全部补回。
/// * 不重：补发只取 `seq > 游标`，且同一游标再订阅只补"上次补发之后"的新事件
///   （[`EventJournal::resumed`] 去重表）；`event_id` 带 epoch，重启后也不会与
///   上一个进程发过的撞号。
pub fn subscribe<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    core: &CoreState,
    cursor: Option<Value>,
) -> Value {
    let head = core.head();
    let incoming = cursor.unwrap_or(Value::Null);

    // 冷订阅：没有游标就无从判断漏了什么，把当前 head 交给调用方当基线。
    let replay = match parse_cursor(core, &incoming) {
        Resume::Fresh => Replay::nothing(head),
        Resume::Invalid => Replay::missing(head, "invalid_cursor"),
        Resume::At { epoch, seq, token } => resume_at(app, core, &epoch, seq, &token, head),
    };

    subscribe_reply(core, head, replay)
}

/// 一次续订的补发结果（喂给 [`subscribe_reply`]）。
#[derive(Default)]
struct Replay {
    /// 补不出的原因；None = 没有洞。
    gap: Option<&'static str>,
    /// 这次真的重发了几条。
    count: u64,
    /// 补发窗口的左右边界（没补发时 = 基线 head）。
    from: u64,
    to: u64,
}

impl Replay {
    /// 什么都不补发（冷订阅，或认不出/用不了的游标）。
    fn nothing(head: u64) -> Self {
        Self {
            gap: None,
            count: 0,
            from: head,
            to: head,
        }
    }

    /// 承认有洞：一条也补不出，但要告诉调用方从哪里重拉。
    fn missing(head: u64, reason: &'static str) -> Self {
        Self {
            gap: Some(reason),
            ..Self::nothing(head)
        }
    }
}

/// 游标合法（同一条流、没超前）时的补发。
///
/// 缺口用 `&'static str` 而不是 bool——调用方要按原因决定重拉哪几份投影。
fn resume_at<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    core: &CoreState,
    epoch: &str,
    seq: u64,
    token: &str,
    head: u64,
) -> Replay {
    use tauri::Emitter as _;
    if epoch != core.epoch {
        // 游标来自上一个进程：环形日志已随之消失，只能让调用方重拉。
        return Replay::missing(head, "stream_reset");
    }
    if seq > head {
        // 调用方声称收到过还没发出去的事件：状态不可信，重拉。
        return Replay::missing(head, "cursor_ahead");
    }
    let (pending, base, truncated) = take_replay(core, seq, token, head);
    let mut replay = Replay {
        gap: truncated,
        count: pending.len() as u64,
        from: pending.first().map(|(s, _)| *s).unwrap_or(base),
        to: head,
    };
    // 锁已在 [`take_replay`] 返回前释放：emit 会同步回调监听器，监听器里可能再次订阅。
    for (event_seq, envelope) in &pending {
        let _ = app.emit("core://event", envelope.clone());
        replay.to = *event_seq;
    }
    record_resumed(core, token.to_string(), base.max(replay.to));
    replay
}

/// 这次续订该补发哪些事件：返回 `(待补发, 补发基线 seq, 缺口原因)`。
///
/// 锁在返回前释放，`subscribe` 才能在持锁之外做 emit。
fn take_replay(
    core: &CoreState,
    seq: u64,
    token: &str,
    head: u64,
) -> (Vec<(u64, Value)>, u64, Option<&'static str>) {
    let journal = core.journal();
    let gap = if seq < journal.dropped_through {
        Some("journal_truncated")
    } else {
        None
    };
    // 去重：这个游标此前已补发到 resumed[token]，只补那之后的新事件。
    let base = seq.max(journal.resumed.get(token).copied().unwrap_or(seq));
    let pending = journal
        .entries
        .iter()
        .filter(|(s, _)| *s > base && *s <= head)
        .cloned()
        .collect();
    drop(journal);
    (pending, base, gap)
}

/// 记下这次补发到的水位；表太长就整表清空（它是去重加速，不是真相源）。
fn record_resumed(core: &CoreState, token: String, upto: u64) {
    let mut journal = core.journal();
    if journal.resumed.len() >= RESUME_DEDUPE_CAP {
        journal.resumed.clear();
    }
    journal.resumed.insert(token, upto);
}

/// 续订回执。`replay.gap` 为真时带上要重拉的投影清单，绝不让调用方以为补全了。
fn subscribe_reply(core: &CoreState, head: u64, replay: Replay) -> Value {
    let available_from = core
        .journal()
        .entries
        .front()
        .map(|(s, _)| s.saturating_sub(1))
        .unwrap_or(head);
    let gap = replay.gap;
    json!({
        "subscribed": true,
        "stream_id": EVENT_STREAM_ID,
        // 调用方把它原样带回 core_subscribe 即可续订。
        "cursor": { "stream_id": EVENT_STREAM_ID, "cursor": core.cursor_at(head) },
        "head": head,
        "replayed": replay.count,
        "replayed_from": replay.from,
        "replayed_to": replay.to,
        "gap": gap.is_some(),
        "reason": gap,
        // gap 时调用方要重拉的投影（不补事件，改拉全量）。
        "refetch": if gap.is_some() {
            json!(["messaging.conversations.list", "messaging.messages.page", "relations.list", "preferences.get"])
        } else {
            json!([])
        },
        "journal_capacity": JOURNAL_CAP,
        "journal_available_from": available_from,
    })
}

