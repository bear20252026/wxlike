//! 浏览器传输：`fetch` + `AbortController`。
//!
//! 为什么不用 `XMLHttpRequest`：fetch 的 `AbortSignal` 是**唯一**能在浏览器里
//! 真正取消一个在飞请求的机制。取消按钮点了没反应、超时之后请求还在后台跑，
//! 都是因为用了没法中断的通道。
//!
//! 三个细节：
//!
//! * **截止时间**用 `setTimeout` 到点 `abort()`，不是"等它自己超时"；
//! * **审计标记**（`x-wxlike-command`）必须带上：服务端靠它记审计，浏览器端
//!   绕过执行器自己拼 URL 是历史遗留问题（`crates/ffi` 的注释里记着）；
//! * **错误分类**：`AbortError` 是超时/取消，`TypeError` 是断网/CORS 失败，
//!   两者的用户提示完全不同。

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortController, Request, RequestInit, RequestMode, Response};

use wxlike_client_core::ports::{
    EventSource, Transport, TransportError, TransportRequest, TransportResponse,
};
use wxlike_types::{Deadline, SubscribeRequest};

/// 浏览器传输。
#[derive(Clone)]
pub struct FetchTransport {
    base: String,
    token: Rc<RefCell<Option<String>>>,
}

impl FetchTransport {
    /// 用服务端 base URL 构造。
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            token: Rc::new(RefCell::new(None)),
        }
    }

    /// 换 token（登录/刷新后）。
    ///
    /// `try_borrow_mut`：token 槽被同栈占用（理论上只在 set 与请求构造
    /// 重入时出现）时这次写入被丢弃——比 panic 好得多，且下一次 set/请求
    /// 拿到的是一致的旧值。
    pub fn set_token(&self, token: Option<String>) {
        if let Ok(mut slot) = self.token.try_borrow_mut() {
            *slot = token;
        }
    }
}

fn js_error(context: &str, error: &JsValue) -> TransportError {
    let name = js_sys::Reflect::get(error, &JsValue::from_str("name"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default();
    if name == "AbortError" {
        return TransportError::Timeout;
    }
    let message = js_sys::Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| format!("{error:?}"));
    if name == "TypeError" {
        // fetch 的网络失败一律是 TypeError（CORS、DNS、断网、被拦截）
        return TransportError::Offline(format!("{context}: {message}"));
    }
    TransportError::Protocol(format!("{context}: {name} {message}"))
}

/// 拼 URL：base 去掉尾斜杠，path 以 `/` 开头。
fn join_url(base: &str, path: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), path)
}

/// 到点 abort。返回的 `AbortController` 由调用方持有。
fn schedule_abort(controller: &AbortController, deadline: Option<Deadline>) -> Option<i32> {
    let deadline = deadline?;
    let now = js_sys::Date::now() as i64;
    if deadline.expired(now) {
        controller.abort();
        return None;
    }
    let remaining = deadline.remaining_ms(now);
    let millis = i32::try_from(remaining.min(u64::from(u32::MAX / 2))).unwrap_or(i32::MAX);
    let window = web_sys::window()?;
    let controller = controller.clone();
    let callback = Closure::once_into_js(move || controller.abort());
    window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.unchecked_ref::<js_sys::Function>(),
            millis,
        )
        .ok()
}

fn build_init(request: &TransportRequest, controller: &AbortController) -> RequestInit {
    let init = RequestInit::new();
    init.set_method(request.method.as_str());
    init.set_mode(RequestMode::Cors);
    init.set_signal(Some(&controller.signal()));
    if let Some(body) = request.body.as_ref() {
        let bytes = js_sys::Uint8Array::from(body.as_slice());
        init.set_body(bytes.buffer().as_ref());
    }
    init
}

fn apply_headers(
    request: &Request,
    transport: &FetchTransport,
    source: &TransportRequest,
) -> Result<(), TransportError> {
    let headers = request.headers();
    let _ = headers.set("accept", "application/json");
    if let Some(content_type) = source.content_type.as_deref() {
        headers
            .set("content-type", content_type)
            .map_err(|err| js_error("设置 content-type", &err))?;
    }
    // token 槽用 try_borrow 拿快照：冲突（重入）时报协议错误而不是 panic
    let token = transport
        .token
        .try_borrow()
        .map_err(|_| TransportError::Protocol("token 槽正被占用".to_string()))?
        .clone();
    if let Some(token) = token.as_deref() {
        headers
            .set("authorization", &format!("Bearer {token}"))
            .map_err(|err| js_error("设置 authorization", &err))?;
    }
    if let Some(command) = source.command.as_ref() {
        headers
            .set("x-wxlike-command", command.as_str())
            .map_err(|err| js_error("设置审计标记", &err))?;
    }
    Ok(())
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Transport for FetchTransport {
    async fn send(&self, request: TransportRequest) -> Result<TransportResponse, TransportError> {
        let window =
            web_sys::window().ok_or_else(|| TransportError::Protocol("没有 window".to_string()))?;
        let controller = AbortController::new().map_err(|err| js_error("AbortController", &err))?;
        let timeout_handle = schedule_abort(&controller, request.deadline);
        let init = build_init(&request, &controller);
        let url = join_url(&self.base, &request.path);
        let js_request = Request::new_with_str_and_init(&url, &init)
            .map_err(|err| js_error("构造请求", &err))?;
        apply_headers(&js_request, self, &request)?;
        let response_value = JsFuture::from(window.fetch_with_request(&js_request))
            .await
            .map_err(|err| js_error("fetch", &err))?;
        if let Some(handle) = timeout_handle {
            window.clear_timeout_with_handle(handle);
        }
        let response: Response = response_value
            .dyn_into()
            .map_err(|_| TransportError::Protocol("响应不是 Response".to_string()))?;
        let buffer = JsFuture::from(
            response
                .array_buffer()
                .map_err(|err| js_error("读响应体", &err))?,
        )
        .await
        .map_err(|err| js_error("读响应体", &err))?;
        let status = response.status();
        Ok(TransportResponse {
            status,
            body: js_sys::Uint8Array::new(&buffer).to_vec(),
        })
    }

    async fn subscribe(
        &self,
        _request: SubscribeRequest,
    ) -> Result<Box<dyn EventSource>, TransportError> {
        // WebSocket 适配器在 `crate::ws`；这里不静默降级成轮询，
        // 让调用方明确知道"实时通道没接上"（降级策略在核心，不在这层）
        Err(TransportError::Protocol(
            "请使用 crate::ws::WebSocketEvents 打开实时通道".to_string(),
        ))
    }
}

/// 截止时间是否已经过期（供测试断言用）。
#[must_use]
pub fn deadline_expired(deadline: Option<Deadline>) -> bool {
    deadline
        .map(|deadline| deadline.expired(js_sys::Date::now() as i64))
        .unwrap_or(false)
}
