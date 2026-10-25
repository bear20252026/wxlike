//! WebSocket 事件源。
//!
//! 浏览器里 `WebSocket` 是**回调式**的，没有 async 迭代器；这里用
//! "回调塞队列 + `next()` 轮询队列"把它接成 [`EventSource`]。
//! 轮询间隔 20ms：比一帧还短，用户感知不到；比"每个事件建一个 promise"
//! 简单得多，也不会有 promise 泄漏。
//!
//! 三条必须处理的状态：
//!
//! * `onclose` → `next()` 返回 `None`，调用方据游标重新订阅；
//! * `onerror` → 同样结束，不在这里重连（重连策略在核心，只有一处）；
//! * 消息不是合法 `EventEnvelope` → **丢弃并继续**，不能让一条脏数据
//!   把整条流打断（服务端灰度新字段时就会这样）。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use web_sys::{MessageEvent, WebSocket};

use wxlike_client_core::ports::{Clock, EventSource, TransportError};
use wxlike_types::EventEnvelope;

use crate::clock::WebClock;

/// 轮询间隔（毫秒）。
const POLL_MS: u64 = 20;

/// WebSocket 事件源。
pub struct WebSocketEvents {
    socket: WebSocket,
    queue: Rc<RefCell<VecDeque<EventEnvelope>>>,
    closed: Rc<Cell<bool>>,
    error: Rc<RefCell<Option<String>>>,
}

impl WebSocketEvents {
    /// 建连。`url` 必须是 `ws://` 或 `wss://` 全量地址。
    ///
    /// # Errors
    /// 浏览器拒绝建连（混合内容、非法 URL）。
    pub fn open(url: &str) -> Result<Self, TransportError> {
        let socket = WebSocket::new(url)
            .map_err(|err| TransportError::Protocol(format!("WebSocket 建连失败：{err:?}")))?;
        let queue: Rc<RefCell<VecDeque<EventEnvelope>>> = Rc::new(RefCell::new(VecDeque::new()));
        let closed = Rc::new(Cell::new(false));
        let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

        let on_message = {
            let queue = queue.clone();
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let Some(text) = event.data().as_string() else {
                    return;
                };
                // 脏数据丢掉，不打断流
                if let Ok(envelope) = serde_json::from_str::<EventEnvelope>(&text) {
                    queue.borrow_mut().push_back(envelope);
                }
            })
        };
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        on_message.forget();

        let on_close = {
            let closed = closed.clone();
            Closure::<dyn FnMut(web_sys::Event)>::new(move |_event: web_sys::Event| {
                closed.set(true);
            })
        };
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        on_close.forget();

        let on_error = {
            let closed = closed.clone();
            let error = error.clone();
            Closure::<dyn FnMut(web_sys::Event)>::new(move |_event: web_sys::Event| {
                closed.set(true);
                *error.borrow_mut() = Some("WebSocket 错误".to_string());
            })
        };
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_error.forget();

        Ok(Self {
            socket,
            queue,
            closed,
            error,
        })
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
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl EventSource for WebSocketEvents {
    async fn next(&mut self) -> Result<Option<EventEnvelope>, TransportError> {
        loop {
            if let Some(envelope) = self.queue.borrow_mut().pop_front() {
                return Ok(Some(envelope));
            }
            if self.closed.get() {
                let reason = self.error.borrow().clone();
                return match reason {
                    Some(message) => Err(TransportError::Offline(message)),
                    None => Ok(None),
                };
            }
            WebClock::new().sleep(POLL_MS).await;
        }
    }

    async fn close(&mut self) {
        self.closed.set(true);
        let _ = self.socket.close();
    }
}

/// 当前 readyState（诊断用）。
#[must_use]
pub fn ready_state(socket: &WebSocket) -> u16 {
    socket.ready_state()
}
