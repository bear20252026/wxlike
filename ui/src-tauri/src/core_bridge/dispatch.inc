pub fn dispatch<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: CoreRequest,
) -> CoreReply {
    CoreReply::from_result(dispatch_inner(app, state, core, &req), &req.request_id)
}

/// 全部已登记 action 的总路由：按**域前缀**选子路由，这里不列举 action。
///
/// action 名单的唯一来源是各 `*_router` 的 match 臂。在总入口再抄一份正是 #296
/// 的漂移来源（`relations.group.kick` 同时挂在两处，能力目录里却没有它），
/// 所以 `tests/catalog_drift.rs` 把"总入口列举 action"直接判红。
fn dispatch_inner<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let Some(route) = route_for::<R>(&req.action_id) else {
        return Err(unrouted(req));
    };
    route(app, state, core, req)
}

/// 域前缀表：action 的第一段（identity 取到第二段）→ 子路由。
///
/// 表里只有前缀，没有完整 action_id。`tests/catalog_drift.rs` 按这份表逐条验证
/// "子路由的每条臂都能从这里走到、且走到的是它自己"，所以挂错域/漏一个域都会红。
fn route_for<R: tauri::Runtime>(action_id: &str) -> Option<Route<R>> {
    let mut segments = action_id.split('.');
    let head = segments.next().unwrap_or_default();
    let family = segments.next().unwrap_or_default();
    Some(match (head, family) {
        ("identity", "qr_login") => qr_router::<R>,
        ("identity", _) => session_router::<R>,
        ("security", _) => session_router::<R>,
        ("sync", _) => session_router::<R>,
        ("messaging", _) => messaging_router::<R>,
        ("media", _) => media_router::<R>,
        ("relations", _) => relations_router::<R>,
        ("wallet", _) => wallet_router::<R>,
        ("search", _) => search_router::<R>,
        ("preferences", _) => settings_router::<R>,
        ("secrets", _) => settings_router::<R>,
        ("storage", _) => settings_router::<R>,
        ("backup", _) => settings_router::<R>,
        ("app", _) => info_router::<R>,
        _ => return None,
    })
}

/// 两条"走不到处理器"的路共用同一句实话：总入口认不出这个域，
/// 或域里没有这条臂。错误文案只有一份，不会两边各写一套再各自漂移。
fn unrouted(req: &CoreRequest) -> BridgeError {
    BridgeError::NotImpl(format!(
        "core 桥未实现 {}（action 需先在 cmd catalog 登记）",
        req.action_id
    ))
}

fn session_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "identity.session.projection" => act_identity_session_projection(app, state, core, req),
        "identity.session.login" => act_identity_session_login(app, state, core, req),
        "identity.session.register" => act_identity_session_register(app, state, core, req),
        "identity.session.logout" => act_identity_session_logout(app, state, core, req),
        "identity.session.restore" => act_identity_session_restore(app, state, core, req),
        "security.lock" => act_security_lock(app, state, core, req),
        "security.unlock" => act_security_unlock(app, state, core, req),
        "sync.range.get" => act_sync_range_get(app, state, core, req),
        "sync.range.set" => act_sync_range_set(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn qr_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "identity.qr_login.create" => act_identity_qr_login_create(app, state, core, req),
        "identity.qr_login.cancel" => act_identity_qr_login_cancel(app, state, core, req),
        "identity.qr_login.switch_account" => {
            act_identity_qr_login_switch_account(app, state, core, req)
        }
        _ => Err(unrouted(req)),
    }
}

fn messaging_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "messaging.conversations.list" => act_messaging_conversations_list(app, state, core, req),
        "messaging.messages.page" => act_messaging_messages_page(app, state, core, req),
        "messaging.send" => act_messaging_send(app, state, core, req),
        "messaging.retry" => act_messaging_retry(app, state, core, req),
        "messaging.recall" => act_messaging_recall(app, state, core, req),
        "messaging.forward" => act_messaging_forward(app, state, core, req),
        "messaging.read" => act_messaging_read(app, state, core, req),
        "messaging.conversation.pin" => act_messaging_conversation_pin(app, state, core, req),
        "messaging.conversation.mute" => act_messaging_conversation_pin(app, state, core, req),
        "messaging.conversation.read_all" => {
            act_messaging_conversation_read_all(app, state, core, req)
        }
        "messaging.conversation.ensure" => act_messaging_conversation_ensure(app, state, core, req),
        "messaging.draft.get" => act_messaging_draft_get(app, state, core, req),
        "messaging.draft.save" => act_messaging_draft_save(app, state, core, req),
        "messaging.draft.clear" => act_messaging_draft_clear(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn media_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "media.upload.begin" => act_media_upload_begin(app, state, core, req),
        "media.upload.part" => act_media_upload_part(app, state, core, req),
        "media.upload.complete" => act_media_upload_complete(app, state, core, req),
        "media.upload.cancel" => act_media_upload_cancel(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn relations_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "relations.list" => act_relations_list(app, state, core, req),
        "relations.requests.incoming" => act_relations_requests_incoming(app, state, core, req),
        "relations.request.send" => act_relations_request_send(app, state, core, req),
        "relations.request.accept" => act_relations_request_accept(app, state, core, req),
        "relations.request.decline" => act_relations_request_accept(app, state, core, req),
        "relations.block" => act_relations_block(app, state, core, req),
        "relations.note.set" => act_relations_note_set(app, state, core, req),
        "relations.tag.set" => act_relations_tag_set(app, state, core, req),
        "relations.star.set" => act_relations_tag_set(app, state, core, req),
        "relations.profile" => act_relations_profile(app, state, core, req),
        "relations.common_groups" => act_relations_common_groups(app, state, core, req),
        "relations.group.members" => act_relations_group_members(app, state, core, req),
        "relations.group.kick" => act_relations_group_kick(app, state, core, req),
        "relations.group.settings.set" => act_relations_group_settings_set(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

/// 钱包域（#293）：钱的所有读写都从这里过，UI 侧不再自己发 HTTP。
///
/// 每条臂都走 [`authed_json`]——它是桥里唯一允许直连平台的出口，Bearer 由
/// `wxlike-client` 的 `Http` 统一挂（凭据只在 Rust 侧，没有任何 action 会把
/// token 回给 webview，见 `tests/bridge_wallet_auth.rs`）。
fn wallet_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    let _ = (app, core);
    match req.action_id.as_str() {
        "wallet.balance.get" => act_wallet_balance(app, state, core, req),
        "wallet.ledger.page" => act_wallet_ledger_page(app, state, core, req),
        "wallet.transfer" => act_wallet_transfer(app, state, core, req),
        "wallet.redpacket.create" => act_wallet_redpacket_create(app, state, core, req),
        "wallet.redpacket.get" => act_wallet_redpacket_get(app, state, core, req),
        "wallet.redpacket.claim" => act_wallet_redpacket_claim(app, state, core, req),
        "wallet.redpacket.list" => act_wallet_redpacket_list(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn search_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "search.query" => act_search_query(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn settings_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "preferences.get" => act_preferences_get(app, state, core, req),
        "preferences.set" => act_preferences_set(app, state, core, req),
        "secrets.status" => act_secrets_status(app, state, core, req),
        "secrets.lock" => act_secrets_lock(app, state, core, req),
        "secrets.migrate" => act_secrets_migrate(app, state, core, req),
        "storage.status" => act_storage_status(app, state, core, req),
        "storage.cache.clear" => act_storage_cache_clear(app, state, core, req),
        "backup.create" => act_backup_create(app, state, core, req),
        "backup.verify" => act_backup_verify(app, state, core, req),
        "backup.import" => act_backup_import(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

fn info_router<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    core: &CoreState,
    req: &CoreRequest,
) -> Result<Value, BridgeError> {
    match req.action_id.as_str() {
        "app.info" => act_app_info(app, state, core, req),
        _ => Err(unrouted(req)),
    }
}

/// capabilities：如实上报。动作清单 = 真实实现的那组（见 IMPLEMENTED_ACTIONS）。
pub fn capabilities() -> Value {
    json!({
        "contract_version": "1",
        "transport": "tauri",
        "actions": IMPLEMENTED_ACTIONS,
        "qr_login": true,
        "secrets_os_backed": false,
        "sync_range": true,
        "notes": [
            "G01-02：新 UI 经 core_dispatch 走与旧 UI 同一 Rust 后端（HTTP 控制面 + QUIC hub + 本地 SQLite）",
            "未列出的 action 一律返回 NOT_IMPLEMENTED，不伪造成功",
        ],
    })
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

