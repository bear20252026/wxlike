//! 浏览器 `/ws` 事件源：bincode + protocol v3 握手。
//!
//! 服务端只有一种线上格式：一帧一条 binary `WireFrame`（`crates/net/src/conn.rs::ws`）。
//! 浏览器不能复用 `wxlike-net`（含 axum/quinn），所以协议镜像在
//! [`crate::wire`]，本模块只负责：
//!
//! * 建连后发 `ClientHello{device_id, protocol:3, caps:Resume}` + `Hello{token}`；
//! * 收到 `Auth` 后发 `Subscribe{scopes}`，再按本地游标逐会话发 `Resume`；
//! * `Msg` → 规范 `EventEnvelope`，`HelloReject` 立刻失败，`Event` 范围提示
//!   留给 HTTP 补齐（不伪造消息）；
//! * 断线后指数退避重连，保留每个会话的 `seq`，重连时用 `Resume` 续传；
//! * 每条事件入队，`next()` 轮询交付；取消/Drop 时摘回调并关 socket。
//!
//! 浏览器 `WebSocket` 是回调式 API，没有 async 迭代器；这里用“回调塞队列 +
//! `next()` 轮询”接成 [`EventSource`]，轮询间隔 20ms。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{BinaryType, Event, MessageEvent, WebSocket};

use wxlike_client_core::ports::{Clock, EventSource, TransportError};
use wxlike_types::{Cursor, EventEnvelope, SubscribeRequest};

use crate::clock::WebClock;
use crate::wire::{envelope_to_event, WireFrame, CAP_RESUME, PROTOCOL_V3};

/// 轮询间隔（毫秒）。
const POLL_MS: u64 = 20;
/// 首次重连等待；之后翻倍。
const RECONNECT_MIN_MS: u64 = 1_000;
/// 重连等待上限。
const RECONNECT_MAX_MS: u64 = 15_000;
/// keepalive：服务端没有 Pong，只要求连接别被中间层判空闲。
const PING_INTERVAL_MS: f64 = 25_000.0;

/// 一条订阅的连接参数。
#[derive(Clone)]
struct Config {
    url: String,
    token: String,
    device_id: String,
    scopes: Vec<String>,
}

/// 多个 JS 回调共享的状态。
struct Shared {
    queue: RefCell<VecDeque<EventEnvelope>>,
    /// socket 已 open（不等于已认证）。
    online: Cell<bool>,
    /// socket 已关闭/尚未 open；`next()` 据此决定是否重连。
    socket_closed: Cell<bool>,
    /// 调用方主动退订/Drop。
    stopped: Cell<bool>,
    /// 握手被拒：不再重试，直接把原因交给上层。
    fatal: RefCell<Option<String>>,
    /// 最近一次传输错误。
    error: RefCell<Option<String>>,
    /// 服务端在 `HelloAck/Auth` 里认定的 user id。
    self_user: RefCell<Option<String>>,
    /// 每个会话最后一条 `Msg` 的 seq；重连时作为 `Resume` 游标。
    last_seq: RefCell<HashMap<String, u64>>,
    /// 连接代次：旧 socket 的 onclose/onmessage 不能影响新连接。
    generation: Cell<u64>,
    /// 重连次数（onopen 归零）。
    attempts: Cell<u32>,
    /// 上次 Ping 时间（`Date.now()`）。
    last_ping_ms: Cell<f64>,
}

/// `/ws` 事件源。
pub struct WebSocketEvents {
    shared: Rc<Shared>,
    config: Config,
    socket: Option<WebSocket>,
    on_open: Option<Closure<dyn FnMut(Event)>>,
    on_message: Option<Closure<dyn FnMut(MessageEvent)>>,
    on_close: Option<Closure<dyn FnMut(Event)>>,
    on_error: Option<Closure<dyn FnMut(Event)>>,
}

/// 浏览器设备 id：首次生成后写 localStorage，同一浏览器多 tab 共用一个。
#[must_use]
pub fn device_id() -> String {
    let key = "wxlike-web-device";
    let storage = web_sys::window().and_then(|window| window.local_storage().ok().flatten());
    if let Some(storage) = storage.as_ref() {
        if let Ok(Some(existing)) = storage.get_item(key) {
            if !existing.trim().is_empty() {
                return existing;
            }
        }
    }
    let id = format!(
        "web-{:.0}-{:x}",
        js_sys::Date::now(),
        (js_sys::Math::random() * 1e16) as u64
    );
    if let Some(storage) = storage.as_ref() {
        let _ = storage.set_item(key, &id);
    }
    id
}

impl WebSocketEvents {
    /// 建连。`url` 必须是 `ws://` 或 `wss://` 全量地址。
    ///
    /// `request` 里的 `convs` 会转成 `conv:<id>` scopes；`cursor` 作为各会话
    /// 的初始 `Resume` 游标（`SubscribeRequest` 目前只有一个事件游标，逐会话
    /// 游标由连接自己收到的 `Msg` seq 继续推进）。
    ///
    /// # Errors
    /// 未登录、浏览器拒绝建连（混合内容、非法 URL）。
    pub fn open(
        url: &str,
        token: &str,
        device_id: &str,
        request: &SubscribeRequest,
    ) -> Result<Self, TransportError> {
        if token.trim().is_empty() {
            return Err(TransportError::Protocol("未登录，不能订阅实时事件".into()));
        }
        let scopes = scopes_for(request);
        let base_cursor = request
            .cursor
            .as_ref()
            .and_then(|cursor| cursor.decode().ok())
            .map(|value| value.seq.get());
        let mut initial = HashMap::new();
        if let Some(cursor) = base_cursor {
            for conv in &request.convs {
                initial.insert(conv.as_str().to_string(), cursor);
            }
        }
        let shared = Rc::new(Shared {
            queue: RefCell::new(VecDeque::new()),
            online: Cell::new(false),
            socket_closed: Cell::new(true),
            stopped: Cell::new(false),
            fatal: RefCell::new(None),
            error: RefCell::new(None),
            self_user: RefCell::new(None),
            last_seq: RefCell::new(initial),
            generation: Cell::new(0),
            attempts: Cell::new(0),
            last_ping_ms: Cell::new(0.0),
        });
        let config = Config {
            url: url.to_string(),
            token: token.to_string(),
            device_id: device_id.to_string(),
            scopes,
        };
        let mut events = Self {
            shared,
            config,
            socket: None,
            on_open: None,
            on_message: None,
            on_close: None,
            on_error: None,
        };
        events.connect()?;
        Ok(events)
    }

    /// 把 `http(s)://host` 换成 `ws(s)://host` 并拼上路径。
    #[must_use]
    pub fn url_for(base: &str, path: &str) -> String {
        let scheme_swapped = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            base.to_string()
        };
        format!("{}{}", scheme_swapped.trim_end_matches('/'), path)
    }

    fn connect(&mut self) -> Result<(), TransportError> {
        self.detach();
        let socket = WebSocket::new(&self.config.url)
            .map_err(|err| TransportError::Protocol(format!("WebSocket 建连失败：{err:?}")))?;
        socket.set_binary_type(BinaryType::Arraybuffer);

        let generation = self.shared.generation.get() + 1;
        self.shared.generation.set(generation);
        self.shared.socket_closed.set(false);
        self.shared.online.set(false);

        let on_open = {
            let shared = self.shared.clone();
            let config = self.config.clone();
            let socket = socket.clone();
            Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
                if shared.generation.get() != generation {
                    return;
                }
                shared.online.set(true);
                shared.socket_closed.set(false);
                shared.attempts.set(0);
                shared.last_ping_ms.set(js_sys::Date::now());
                // v3 先认领设备，再交 token；身份仍只由服务端从 token 解析。
                let _ = send_frame(
                    &socket,
                    &WireFrame::ClientHello {
                        device_id: config.device_id.clone(),
                        protocol: PROTOCOL_V3,
                        caps: CAP_RESUME,
                    },
                );
                let _ = send_frame(
                    &socket,
                    &WireFrame::Hello {
                        token: config.token.clone(),
                    },
                );
            })
        };
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let on_message = {
            let shared = self.shared.clone();
            let config = self.config.clone();
            let socket = socket.clone();
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                if shared.generation.get() != generation {
                    return;
                }
                let Some(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>().ok() else {
                    // `/ws` 只接受 binary；text 是协议错误，不猜 JSON。
                    *shared.error.borrow_mut() = Some("收到 text 帧；/ws 只接受 binary".into());
                    return;
                };
                let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                let Ok(frame) = WireFrame::from_bytes(&bytes) else {
                    // 单帧坏数据不打断整条流；真正协议漂移由下面的 fatal 覆盖。
                    *shared.error.borrow_mut() =
                        Some(format!("无法解码 {} 字节的 wire 帧", bytes.len()));
                    return;
                };
                handle_frame(&shared, &socket, &config, frame);
            })
        };
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let on_close = {
            let shared = self.shared.clone();
            Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
                if shared.generation.get() != generation {
                    return;
                }
                shared.online.set(false);
                shared.socket_closed.set(true);
                if shared.error.borrow().is_none() {
                    *shared.error.borrow_mut() = Some("WebSocket 连接已关闭".into());
                }
            })
        };
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        let on_error = {
            let shared = self.shared.clone();
            Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
                if shared.generation.get() != generation {
                    return;
                }
                shared.online.set(false);
                shared.socket_closed.set(true);
                *shared.error.borrow_mut() = Some("WebSocket 错误".into());
            })
        };
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        self.socket = Some(socket);
        self.on_open = Some(on_open);
        self.on_message = Some(on_message);
        self.on_close = Some(on_close);
        self.on_error = Some(on_error);
        Ok(())
    }

    fn detach(&mut self) {
        // 先推代次：旧 socket 的 onclose/onerror 不再影响新连接。
        self.shared.generation.set(self.shared.generation.get() + 1);
        self.shared.online.set(false);
        if let Some(socket) = self.socket.take() {
            socket.set_onopen(None);
            socket.set_onmessage(None);
            socket.set_onclose(None);
            socket.set_onerror(None);
            let _ = socket.close();
        }
        self.on_open = None;
        self.on_message = None;
        self.on_close = None;
        self.on_error = None;
    }

    async fn reconnect(&mut self) -> Result<(), TransportError> {
        let mut last = TransportError::Offline("连接已关闭".into());
        for attempt in 0..6 {
            let exponent = u32::try_from(attempt).unwrap_or(0);
            let delay = RECONNECT_MIN_MS
                .saturating_mul(1u64 << exponent.min(4))
                .min(RECONNECT_MAX_MS);
            WebClock::new().sleep(delay).await;
            if self.shared.stopped.get() {
                return Ok(());
            }
            match self.connect() {
                Ok(()) => return Ok(()),
                Err(err) => last = err,
            }
        }
        Err(last)
    }

    fn maybe_ping(&self) {
        let now = js_sys::Date::now();
        if now - self.shared.last_ping_ms.get() < PING_INTERVAL_MS {
            return;
        }
        if let Some(socket) = self.socket.as_ref() {
            if send_frame(socket, &WireFrame::Ping).is_ok() {
                self.shared.last_ping_ms.set(now);
            }
        }
    }
}

fn handle_frame(shared: &Shared, socket: &WebSocket, config: &Config, frame: WireFrame) {
    match frame {
        WireFrame::HelloAck { user_id, .. } => {
            *shared.self_user.borrow_mut() = Some(user_id);
        }
        WireFrame::Auth { user_id, .. } => {
            *shared.self_user.borrow_mut() = Some(user_id);
            let _ = send_frame(
                socket,
                &WireFrame::Subscribe {
                    scopes: config.scopes.clone(),
                },
            );
            let cursors = shared.last_seq.borrow().clone();
            for (conv_id, cursor) in cursors {
                let _ = send_frame(socket, &WireFrame::Resume { conv_id, cursor });
            }
        }
        WireFrame::HelloReject { reason } => {
            *shared.fatal.borrow_mut() = Some(reason.clone());
            *shared.error.borrow_mut() = Some(format!("凭据被拒：{reason}"));
            shared.online.set(false);
            shared.socket_closed.set(true);
        }
        WireFrame::Msg(env) => {
            shared
                .last_seq
                .borrow_mut()
                .insert(env.conv_id.clone(), env.seq);
            let self_user = shared.self_user.borrow().clone();
            if let Some(event) = envelope_to_event(&env, self_user.as_deref()) {
                if let Ok(mut queue) = shared.queue.try_borrow_mut() {
                    queue.push_back(event);
                }
            }
        }
        // 范围提示不是消息本身；缺口由 HTTP 历史补齐，不在这里造假事件。
        WireFrame::Event { .. }
        | WireFrame::Ping
        | WireFrame::Hello { .. }
        | WireFrame::ClientHello { .. }
        | WireFrame::Subscribe { .. }
        | WireFrame::Ack { .. }
        | WireFrame::Resume { .. } => {}
    }
}

fn scopes_for(request: &SubscribeRequest) -> Vec<String> {
    if request.convs.is_empty() {
        // `SubscribeRequest::all()` 没有会话列表；服务端把 scope 当 opaque，
        // 这里显式声明“全部会话”，而不是发一个空 Subscribe。
        vec!["conv:*".to_string()]
    } else {
        request
            .convs
            .iter()
            .map(|conv| format!("conv:{}", conv.as_str()))
            .collect()
    }
}

fn send_frame(socket: &WebSocket, frame: &WireFrame) -> Result<(), TransportError> {
    if socket.ready_state() != WebSocket::OPEN {
        return Err(TransportError::Offline("WebSocket 未连接".into()));
    }
    let bytes = frame.to_bytes().map_err(TransportError::Protocol)?;
    socket
        .send_with_u8_array(&bytes)
        .map_err(|err| TransportError::Protocol(format!("ws send: {err:?}")))
}

impl Drop for WebSocketEvents {
    fn drop(&mut self) {
        self.shared.stopped.set(true);
        self.detach();
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl EventSource for WebSocketEvents {
    async fn next(&mut self) -> Result<Option<EventEnvelope>, TransportError> {
        loop {
            if let Some(envelope) = self.shared.queue.borrow_mut().pop_front() {
                return Ok(Some(envelope));
            }
            if self.shared.stopped.get() {
                return Ok(None);
            }
            if let Some(reason) = self.shared.fatal.borrow().clone() {
                return Err(TransportError::Protocol(format!("凭据被拒：{reason}")));
            }
            if !self.shared.online.get() {
                if self.shared.socket_closed.get() {
                    self.reconnect().await?;
                } else {
                    // 正在 open，等 onopen 回调。
                    WebClock::new().sleep(POLL_MS).await;
                }
                continue;
            }
            self.maybe_ping();
            WebClock::new().sleep(POLL_MS).await;
        }
    }

    async fn close(&mut self) {
        self.shared.stopped.set(true);
        self.detach();
    }
}

/// 当前 readyState（诊断用）。
#[must_use]
pub fn ready_state(socket: &WebSocket) -> u16 {
    socket.ready_state()
}

/// 给 JS/浏览器测试用的游标解码；不构造游标，只读序号。
#[must_use]
pub fn cursor_seq(cursor: &Cursor) -> Option<u64> {
    cursor.decode().ok().map(|value| value.seq.get())
}
