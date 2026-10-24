//! 浏览器时钟：`Date.now()` + `setTimeout`。
//!
//! `Date.now()` 会被用户改（时区、手动校时），所以它只用来算"过了多久"，
//! 不用于任何安全判断——游标与幂等键都不依赖它。
//!
//! 后台标签页的定时器会被节流（Chrome 最狠能压到 1 分钟一次），所以 `sleep`
//! 只能当"至少等这么久"，补发节拍不能靠它精确。

use wasm_bindgen_futures::JsFuture;
use wxlike_client_core::ports::Clock;

/// 浏览器时钟。
#[derive(Clone, Copy, Debug, Default)]
pub struct WebClock;

impl WebClock {
    /// 构造。
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Clock for WebClock {
    fn now_ms(&self) -> i64 {
        js_sys::Date::now() as i64
    }

    async fn sleep(&self, ms: u64) {
        let millis = i32::try_from(ms.min(u64::from(u32::MAX / 2))).unwrap_or(i32::MAX);
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let Some(window) = web_sys::window() else {
                return;
            };
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
        });
        let _ = JsFuture::from(promise).await;
    }
}
