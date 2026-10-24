//! Tauri 命令面：前端 invoke 的全部入口（G00-02 自 lib.rs 内联模块转文件模块）。
//! 原为嵌套模块以规避根级命令名与文本定义的 E0255 冲突；转文件模块后
//! `crate::commands` 路径不变，外部调用方零改动。

use super::*;


use super::*;

// -- P4/P5: 通话房间控制面 + 设备验证（前端不碰 HTTP，全部经此桥） --------

/// 房间快照：`live=false` 表示当前没有通话。
/// P3 未读数：服务端信箱是唯一真相（`MessageStore::unread`）。
/// 走 `wxlike-client` 的领域方法，而不是在这里拼 URL——移动端要复用的正是这一层。
#[tauri::command]
pub fn conv_unread(
    state: tauri::State<'_, AppState>,
    conv: String,
) -> Result<serde_json::Value, String> {
    state.http().conv_unread(&conv)
}

/// 把已读游标推到最新（空 body = 读到最新）。返回推送后的快照。
#[tauri::command]
pub fn conv_mark_read(
    state: tauri::State<'_, AppState>,
    conv: String,
) -> Result<serde_json::Value, String> {
    state.http().mark_conv_read(&conv)
}

#[tauri::command]
pub fn call_state(
    state: tauri::State<'_, AppState>,
    conv: String,
) -> Result<serde_json::Value, String> {
    state
        .http()
        .expect_json("GET", &format!("/api/call/{conv}"), None)
}

/// 统一入口：`action` ∈ start/join/leave/mute/mute-all/kick/lock/record/
/// hand/transfer/egress。参数按动作取用，其余忽略。
#[tauri::command]
pub fn call_action(
    state: tauri::State<'_, AppState>,
    conv: String,
    action: String,
    target: Option<String>,
    track: Option<String>,
    value: Option<bool>,
    capacity: Option<usize>,
) -> Result<serde_json::Value, String> {
    let path = format!("/api/call/{conv}/{action}");
    let mut body = serde_json::Map::new();
    match action.as_str() {
        "mute" | "mute-all" => {
            body.insert(
                "track".into(),
                serde_json::json!(track.unwrap_or_else(|| "audio".into())),
            );
            body.insert("muted".into(), serde_json::json!(value.unwrap_or(true)));
            if let Some(t) = target {
                body.insert("target".into(), serde_json::json!(t));
            }
        }
        "kick" | "transfer" => {
            let t = target.ok_or_else(|| "target is required".to_string())?;
            body.insert("target".into(), serde_json::json!(t));
        }
        "lock" | "record" | "hand" => {
            body.insert("value".into(), serde_json::json!(value.unwrap_or(true)));
        }
        "start" => {
            body.insert("capacity".into(), serde_json::json!(capacity));
        }
        _ => {}
    }
    if action == "egress" {
        return state.http().expect_json("GET", &path, None);
    }
    // 控制类端点一律 204：必须走 expect_empty——`into_json` 会把空 body
    // 当成 decode 错误（"EOF"），这里踩过一次。
    let payload = if body.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(body))
    };
    state.http().expect_empty("POST", &path, payload.as_ref())?;
    Ok(serde_json::json!({ "ok": true }))
}

/// 投递一条通话信令（offer/answer/ICE）。`from` 由服务端改写，别信客户端。
#[tauri::command]
pub fn call_signal_post(
    state: tauri::State<'_, AppState>,
    conv: String,
    signal: serde_json::Value,
) -> Result<serde_json::Value, String> {
    state
        .http()
        .expect_json("POST", &format!("/api/call/{conv}/signal"), Some(&signal))
}

/// 取走发给我的信令（读即清，服务端队列有界）。
#[tauri::command]
pub fn call_signal_take(
    state: tauri::State<'_, AppState>,
    conv: String,
) -> Result<serde_json::Value, String> {
    state
        .http()
        .expect_json("GET", &format!("/api/call/{conv}/signal"), None)
}

/// 安全码（12×5 数字）+ 对端身份指纹 + 我是否已标记验证。
#[tauri::command]
pub fn safety_number(
    state: tauri::State<'_, AppState>,
    peer: String,
) -> Result<serde_json::Value, String> {
    state
        .http()
        .expect_json("GET", &format!("/api/secret/{peer}/safety-number"), None)
}

/// 口头/扫码比对通过后落一条验证记录。
#[tauri::command]
pub fn verify_peer(
    state: tauri::State<'_, AppState>,
    peer: String,
    peer_identity: String,
) -> Result<(), String> {
    state.http().expect_empty(
        "PUT",
        &format!("/api/secret/{peer}/verify"),
        Some(&serde_json::json!({ "peer_identity": peer_identity })),
    )
}

#[tauri::command]
pub fn set_server(state: tauri::State<'_, AppState>, base_url: String) -> Result<(), String> {
    state.set_server(&base_url);
    Ok(())
}

#[tauri::command]
pub fn register(
    state: tauri::State<'_, AppState>,
    name: String,
    password: String,
) -> Result<serde_json::Value, String> {
    state.register(&name, &password)
}

#[tauri::command]
pub fn login(
    state: tauri::State<'_, AppState>,
    name: String,
    password: String,
) -> Result<serde_json::Value, String> {
    state.login(&name, &password)
}

#[tauri::command]
pub fn logout(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.logout()
}

#[tauri::command]
pub fn me(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    state.me()
}

#[tauri::command]
pub fn update_profile(
    state: tauri::State<'_, AppState>,
    name: Option<String>,
    avatar: Option<String>,
    signature: Option<String>,
) -> Result<serde_json::Value, String> {
    state.update_profile(name.as_deref(), avatar.as_deref(), signature.as_deref())
}

#[tauri::command]
pub fn friends(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    state.friends()
}

#[tauri::command]
pub fn friend_request(
    state: tauri::State<'_, AppState>,
    to_user_id: String,
    message: String,
) -> Result<(), String> {
    state.friend_request(&to_user_id, &message)
}

#[tauri::command]
pub fn friend_requests_incoming(
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    state.friend_requests_incoming()
}

#[tauri::command]
pub fn friend_requests_outgoing(
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    state.friend_requests_outgoing()
}

#[tauri::command]
pub fn friend_accept(state: tauri::State<'_, AppState>, req_id: String) -> Result<(), String> {
    state.friend_accept(&req_id)
}

#[tauri::command]
pub fn friend_decline(state: tauri::State<'_, AppState>, req_id: String) -> Result<(), String> {
    state.friend_decline(&req_id)
}

#[tauri::command]
pub fn friend_delete(state: tauri::State<'_, AppState>, user_id: String) -> Result<(), String> {
    state.friend_delete(&user_id)
}

#[tauri::command]
pub fn groups_create(
    state: tauri::State<'_, AppState>,
    name: String,
    member_ids: Vec<String>,
) -> Result<String, String> {
    state.groups_create(&name, &member_ids)
}

#[tauri::command]
pub fn groups_mine(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    state.groups_mine()
}

#[tauri::command]
pub fn group_info(
    state: tauri::State<'_, AppState>,
    group_id: String,
) -> Result<serde_json::Value, String> {
    state.group_info(&group_id)
}

#[tauri::command]
pub fn wallet_balance(state: tauri::State<'_, AppState>) -> Result<i64, String> {
    state.wallet_balance()
}

#[tauri::command]
pub fn wallet_transfer(
    state: tauri::State<'_, AppState>,
    to_uid: String,
    amount: i64,
    memo: String,
) -> Result<String, String> {
    state.wallet_transfer(&to_uid, amount, &memo)
}

#[tauri::command]
pub fn wallet_redpacket_cmd(
    state: tauri::State<'_, AppState>,
    amount: i64,
    count: i64,
    memo: String,
) -> Result<String, String> {
    state.wallet_redpacket(amount, count, &memo)
}

#[tauri::command]
pub fn wallet_claim(
    state: tauri::State<'_, AppState>,
    redpacket_id: String,
) -> Result<i64, String> {
    state.wallet_claim(&redpacket_id)
}

#[tauri::command]
pub fn upload_media(
    state: tauri::State<'_, AppState>,
    bytes: Vec<u8>,
    content_type: String,
    sha256: Option<String>,
    conv_id: Option<String>,
    public: Option<bool>,
) -> Result<serde_json::Value, String> {
    state.upload_media(
        bytes,
        &content_type,
        sha256.as_deref(),
        conv_id.as_deref(),
        public.unwrap_or(false),
    )
}

#[tauri::command]
pub fn download_media(
    state: tauri::State<'_, AppState>,
    media_id: String,
    thumb: bool,
) -> Result<MediaBlob, String> {
    state.download_media(&media_id, thumb)
}

#[tauri::command]
pub fn conversation_id(
    state: tauri::State<'_, AppState>,
    peer: String,
) -> Result<String, String> {
    let me = state.user_id()?;
    Ok(direct_conv_id(&me, &peer))
}

#[tauri::command]
pub fn connect_hub(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    addr: String,
) -> Result<String, String> {
    // 连 hub 用**凭据**，不是 user_id（issue #3）
    let token = state
        .auth_token()
        .ok_or_else(|| "未登录：没有可用的凭据".to_string())?;
    let pins = app
        .path()
        .app_data_dir()
        .map(|dir| dir.join("tofu-pins.json"))
        .unwrap_or_else(|_| std::env::temp_dir().join("wxlike-tofu-pins.json"));
    let out = state.connect_hub(&addr, &token, &pins)?;
    // 连上才起泵：没连接时它只会每秒空转一次，无害但没必要
    state.start_outbox_pump(app.clone());
    Ok(out)
}

/// 发送队列状态：还有几条没发出去、哪几条彻底失败（UI 显示"重发/删除"）。
#[tauri::command]
pub fn outbox_status(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let Some(ob) = state.outbox() else {
        return Ok(serde_json::json!({ "pending": 0, "failed": [] }));
    };
    let pending = ob.pending_count().map_err(|e| e.to_string())?;
    let failed = ob.failed(50).map_err(|e| e.to_string())?;
    let failed: Vec<serde_json::Value> = failed
        .iter()
        .map(|t| {
            serde_json::json!({
                "task_id": t.task_id,
                "conv_id": t.conv_id,
                "attempts": t.attempts,
                "error": t.last_error,
                "payload": t.payload,
            })
        })
        .collect();
    Ok(serde_json::json!({ "pending": pending, "failed": failed }))
}

/// 用户点"重发"：清零计数，交给后台泵。
#[tauri::command]
pub fn outbox_retry(state: tauri::State<'_, AppState>, task_id: String) -> Result<(), String> {
    let ob = state.outbox().ok_or("本地库未打开")?;
    ob.requeue(&task_id, wxlike_core::types::now_ms() as i64)
        .map_err(|e| e.to_string())
}

/// 网络诊断：DNS → TCP → HTTP → QUIC，四步顺序短路。
///
/// 目标从当前连接推导（控制面 base + hub 地址），所以用户不需要填任何东西。
/// 没连过 hub 也能跑——DNS/TCP/HTTP 三步照样能定位"为什么连不上"。
#[tauri::command]
pub fn net_diag(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let base = state.http_base();
    let rest = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .unwrap_or(&base)
        .trim_end_matches('/');
    let (host, http_port) = split_host_port(rest, 80);
    let quic_port = state
        .hub_addr()
        .and_then(|a| a.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok()))
        .unwrap_or(9443);

    let pins = app
        .path()
        .app_data_dir()
        .map(|dir| dir.join("tofu-pins.json"))
        .unwrap_or_else(|_| std::env::temp_dir().join("wxlike-tofu-pins.json"));
    let mut opts = wxlike_net::diag::DiagOptions::new(pins);
    // 诊断走的是同一个 `Hello`：要**凭据**，不是 user_id（issue #3）
    opts.token = state.auth_token();
    let target = wxlike_net::diag::DiagTarget::new(host, http_port, quic_port);

    let report = wait(wxlike_net::diag::run(&target, &opts));
    serde_json::to_value(&report).map_err(|e| e.to_string())
}

/// 用户点"删除"：放弃这条消息。
#[tauri::command]
pub fn outbox_drop(state: tauri::State<'_, AppState>, task_id: String) -> Result<(), String> {
    let ob = state.outbox().ok_or("本地库未打开")?;
    ob.remove(&task_id).map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn send_text(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    to: Vec<String>,
    text: String,
) -> Result<(), String> {
    state.send_text(&conv_id, to, &text)
}

#[tauri::command]
pub fn send_image(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    to: Vec<String>,
    media_id: String,
) -> Result<(), String> {
    state.send_image(&conv_id, to, &media_id)
}

#[tauri::command]
pub fn send_redpacket_msg(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    to: Vec<String>,
    redpacket_id: String,
    amount: i64,
    count: i64,
    memo: String,
) -> Result<(), String> {
    state.send_redpacket(&conv_id, to, &redpacket_id, amount, count, &memo)
}

#[tauri::command]
pub fn start_recv_pump(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    state.start_recv_pump(app)
}

// ---- P1 commands: five message operations + history + secret chat ------

#[tauri::command]
pub fn send_typing(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    to: Vec<String>,
) -> Result<(), String> {
    state.send_typing(&conv_id, to)
}

#[tauri::command]
pub fn forward_message(
    state: tauri::State<'_, AppState>,
    to_conv: String,
    to: Vec<String>,
    item: serde_json::Value,
) -> Result<(), String> {
    state.forward_message(&to_conv, to, &item)
}

#[tauri::command]
pub fn recall_msg(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    target: String,
) -> Result<(), String> {
    state.recall_msg(&conv_id, &target)
}

#[tauri::command]
pub fn send_read_receipt(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    up_to: String,
) -> Result<(), String> {
    state.send_read_receipt(&conv_id, &up_to)
}

#[tauri::command]
pub fn merge_forward(
    state: tauri::State<'_, AppState>,
    to_conv: String,
    to: Vec<String>,
    items: Vec<serde_json::Value>,
) -> Result<(), String> {
    state.merge_forward(&to_conv, to, &items)
}

#[tauri::command]
pub fn load_history(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    before_seq: i64,
    limit: usize,
) -> Result<serde_json::Value, String> {
    state.load_history(&conv_id, before_seq, limit)
}

#[tauri::command]
pub fn search_messages(
    state: tauri::State<'_, AppState>,
    q: String,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    state.search_messages(&q, limit.unwrap_or(20))
}

#[tauri::command]
pub fn favorite_add(
    state: tauri::State<'_, AppState>,
    item: serde_json::Value,
) -> Result<serde_json::Value, String> {
    state.favorite_add(&item)
}

#[tauri::command]
pub fn sessions_list(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    state.sessions_list()
}

#[tauri::command]
pub fn session_revoke(state: tauri::State<'_, AppState>, sid: String) -> Result<(), String> {
    state.session_revoke(&sid)
}

#[tauri::command]
pub fn sessions_revoke_others(
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    state.sessions_revoke_others()
}

#[tauri::command]
pub fn favorites_list(
    state: tauri::State<'_, AppState>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    state.favorites_list(limit.unwrap_or(20))
}

#[tauri::command]
pub fn favorite_remove(state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
    state.favorite_remove(&id)
}

#[tauri::command]
pub fn secret_enable(state: tauri::State<'_, AppState>) -> Result<String, String> {
    state.secret_enable()
}

#[tauri::command]
pub fn secret_enabled(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    Ok(state.secret_enabled())
}

#[tauri::command]
pub fn secret_configure(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    enabled: bool,
    ttl_ms: Option<u64>,
) -> Result<serde_json::Value, String> {
    state.secret_configure(&conv_id, enabled, ttl_ms.unwrap_or(0))
}

#[tauri::command]
pub fn secret_state(
    state: tauri::State<'_, AppState>,
    conv_id: String,
) -> Result<serde_json::Value, String> {
    state.secret_state(&conv_id)
}

#[tauri::command]
pub fn send_secret_text(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    to: Vec<String>,
    text: String,
) -> Result<(), String> {
    state.send_secret_text(&conv_id, to, &text)
}

// ---- P2 commands: rich messages + video + avatar + local library ------

#[tauri::command]
pub fn send_voice(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    to: Vec<String>,
    media_id: String,
    duration_ms: u32,
    waveform: Vec<u8>,
) -> Result<(), String> {
    state.send_voice(&conv_id, to, &media_id, duration_ms, waveform)
}

#[tauri::command]
pub fn send_file(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    to: Vec<String>,
    media_id: String,
    name: String,
    size: u64,
    sha256: String,
) -> Result<(), String> {
    state.send_file(&conv_id, to, &media_id, &name, size, &sha256)
}

#[tauri::command]
pub fn send_sticker(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    to: Vec<String>,
    media_id: String,
    pack: String,
    id: String,
) -> Result<(), String> {
    state.send_sticker(&conv_id, to, &media_id, &pack, &id)
}

#[tauri::command]
pub fn upload_video(
    state: tauri::State<'_, AppState>,
    bytes: Vec<u8>,
) -> Result<serde_json::Value, String> {
    state.upload_video(bytes)
}

#[tauri::command]
pub fn send_video(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    to: Vec<String>,
    payload: serde_json::Value,
) -> Result<(), String> {
    state.send_video(&conv_id, to, &payload)
}

#[tauri::command]
pub fn download_video(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<MediaBlob, String> {
    state.download_video(&path)
}

#[tauri::command]
pub fn upload_avatar(
    state: tauri::State<'_, AppState>,
    bytes: Vec<u8>,
    content_type: String,
) -> Result<String, String> {
    state.upload_avatar(bytes, &content_type)
}

/// 打开本地消息库。**只允许应用数据目录内的路径**。
///
/// webview 传什么进来都不该直接交给 SQLite：一旦前端被 XSS，`../../` 就能
/// 在磁盘任意位置建库/读库。这里按"归一化后必须落在 app_data_dir 内"判断，
/// 而不是只查前缀字符串（`/data2` 不是 `/data` 的子目录）。
#[tauri::command]
pub fn local_open(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let base = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("wxlike-ui"));
    let base = normalize_path(&base);
    let want = normalize_path(std::path::Path::new(&path));
    if !want.starts_with(&base) {
        return Err(format!(
            "local_open 只允许应用数据目录内的库（拒绝 {path}）"
        ));
    }
    state.local_open(&want)
}

/// Opens the local message library at the app-data default; called once
/// at webview boot (tests open an explicit path instead).
#[tauri::command]
pub fn local_autopen(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let dir = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("wxlike-ui"));
    state.local_open(&dir.join("messages.db"))
}

#[tauri::command]
pub fn local_ready(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    Ok(state.local_ready())
}

#[tauri::command]
pub fn local_history(
    state: tauri::State<'_, AppState>,
    conv_id: String,
    before_seq: i64,
    limit: usize,
) -> Result<serde_json::Value, String> {
    state.local_history(&conv_id, before_seq, limit)
}

// -- P5: 本地通话引擎（信令中继 + 真 UDP 媒体） --------------------------

/// 发起通话：开媒体端口、把 offer 投进中继，返回本地 call_id。
/// 房间控制面（`call_action` start）仍由调用方先做——两者是不同层次：
/// 前者管"谁在房里"，这里管"声音怎么过去"。
#[tauri::command]
pub fn call_rtc_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, callrtc::CallEngine>,
    conv: String,
    peers: Vec<String>,
    video: Option<bool>,
) -> Result<Vec<String>, String> {
    let me = state.user_id()?;
    let (base, token) = {
        let http = state.http();
        let token = http.token.clone().ok_or("未登录")?;
        (http.base.clone(), token)
    };
    engine.start(
        app,
        callrtc::RelayCfg { base, token },
        me,
        conv,
        peers,
        video.unwrap_or(false),
    )
}

/// 接听当前响铃中的来电。
#[tauri::command]
pub fn call_rtc_accept(engine: tauri::State<'_, callrtc::CallEngine>) -> Result<(), String> {
    engine.accept();
    Ok(())
}

/// 挂断并停掉引擎线程。
#[tauri::command]
pub fn call_rtc_hangup(engine: tauri::State<'_, callrtc::CallEngine>) -> Result<(), String> {
    engine.hangup();
    engine.shutdown();
    Ok(())
}

/// 引擎状态快照（UI 首屏拿一次，之后靠 `call://rtc` 事件）。
#[tauri::command]
pub fn call_rtc_status(
    engine: tauri::State<'_, callrtc::CallEngine>,
) -> Result<callrtc::EngineStatus, String> {
    Ok(engine.status())
}
