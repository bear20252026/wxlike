//! Boa-backed engine with the `wxhost` bridge installed.
//!
//! Bridge state lives in a thread_local (Boa `NativeFunction` only accepts
//! `Copy` closures / fn pointers in 0.20). `BoaEngine::new` resets it, so each
//! engine instance starts from a clean slate on its thread.

use std::collections::BTreeMap;

use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsResult, JsValue, NativeFunction, Source};

use crate::bridge::BridgeState;

thread_local! {
    static BRIDGE: std::cell::RefCell<BridgeState> = std::cell::RefCell::new(BridgeState::default());
}

pub struct BoaEngine {
    ctx: Context,
}

/// AR-4/H-7 (audit round 4): page packages are untrusted code. Without a
/// loop bound, `while (true) {}` in any page freezes the host thread
/// forever — the desktop app, or whatever process embeds this container.
/// The budget is per loop; pages that legitimately iterate a data grid stay
/// far below it. Recursion/stack caps are Boa's defaults (512 / 10k frames).
pub const LOOP_ITERATION_LIMIT: u64 = 1_000_000;

/// Maximum retained log lines per bridge.
pub const MAX_LOG_LINES: usize = 256;
/// Maximum UTF-8 bytes retained for a log line, including separators.
pub const MAX_LOG_LINE_BYTES: usize = 4 * 1024;

impl Default for BoaEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl BoaEngine {
    pub fn new() -> Self {
        BRIDGE.with(|b| *b.borrow_mut() = BridgeState::default());
        let mut ctx = Context::default();
        ctx.runtime_limits_mut()
            .set_loop_iteration_limit(LOOP_ITERATION_LIMIT);
        install_bridge(&mut ctx).expect("install wxhost bridge");
        Self { ctx }
    }

    pub fn eval(&mut self, code: &str) -> anyhow::Result<String> {
        let value = self
            .ctx
            .eval(Source::from_bytes(code.as_bytes()))
            .map_err(|e| anyhow::Error::msg(e.to_string()))?;
        Ok(dump(&value, &mut self.ctx))
    }

    pub fn call_global(
        &mut self,
        name: &str,
        args_json: &[serde_json::Value],
    ) -> anyhow::Result<String> {
        // AR-4/M-3 (audit round 4): never build JS source from the name or
        // the args. `format!("{name}({args})")` + eval meant a crafted name
        // (or an arg containing `)` / quotes) executed attacker-controlled
        // JS. The name is now only a property lookup; the args are values,
        // never code.
        let func = self
            .ctx
            .global_object()
            .get(boa_engine::JsString::from(name), &mut self.ctx)
            .map_err(|e| anyhow::Error::msg(e.to_string()))?;
        let callable = func
            .as_callable()
            .ok_or_else(|| anyhow::anyhow!("global '{name}' is not a function"))?;
        let mut js_args = Vec::with_capacity(args_json.len());
        for a in args_json {
            js_args.push(
                JsValue::from_json(a, &mut self.ctx)
                    .map_err(|e| anyhow::Error::msg(e.to_string()))?,
            );
        }
        let value = callable
            .call(&JsValue::undefined(), &js_args, &mut self.ctx)
            .map_err(|e| anyhow::Error::msg(e.to_string()))?;
        Ok(dump(&value, &mut self.ctx))
    }

    pub fn title(&self) -> String {
        BRIDGE.with(|b| b.borrow().title.clone())
    }

    pub fn logs(&self) -> Vec<String> {
        BRIDGE.with(|b| b.borrow().logs.iter().cloned().collect())
    }

    /// Consume pending log lines without cloning their contents.
    pub fn drain_logs(&self) -> Vec<String> {
        BRIDGE.with(|b| b.borrow_mut().logs.drain(..).collect())
    }

    pub fn storage(&self) -> BTreeMap<String, String> {
        BRIDGE.with(|b| b.borrow().storage.clone())
    }
}

fn log_line(args: &[JsValue], ctx: &mut Context) -> String {
    let mut line = String::with_capacity(MAX_LOG_LINE_BYTES);
    for arg in args {
        if !line.is_empty() && line.len() < MAX_LOG_LINE_BYTES {
            line.push(' ');
        }
        let remaining = MAX_LOG_LINE_BYTES - line.len();
        if remaining == 0 {
            break;
        }
        let text = dump(arg, ctx);
        let mut end = remaining.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        line.push_str(&text[..end]);
    }
    line
}

fn install_bridge(ctx: &mut Context) -> JsResult<()> {
    fn wx_log(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
        let line = log_line(args, ctx);
        BRIDGE.with(|b| {
            let mut state = b.borrow_mut();
            if state.logs.len() >= MAX_LOG_LINES {
                state.logs.pop_front();
            }
            state.logs.push_back(line);
        });
        Ok(JsValue::undefined())
    }

    fn wx_set_title(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
        if let Some(v) = args.first() {
            let t = v.to_string(ctx)?;
            BRIDGE.with(|b| b.borrow_mut().title = t.to_std_string_escaped());
        }
        Ok(JsValue::undefined())
    }

    fn wx_store_set(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
        let key = match args.first() {
            Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
            None => return Ok(JsValue::undefined()),
        };
        let val = match args.get(1) {
            Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
            None => return Ok(JsValue::undefined()),
        };
        BRIDGE.with(|b| b.borrow_mut().storage.insert(key, val));
        Ok(JsValue::undefined())
    }

    fn wx_store_get(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
        let key = match args.first() {
            Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
            None => return Ok(JsValue::undefined()),
        };
        Ok(
            match BRIDGE.with(|b| b.borrow().storage.get(&key).cloned()) {
                Some(v) => JsValue::from(boa_engine::JsString::from(v)),
                None => JsValue::undefined(),
            },
        )
    }

    fn wx_now_ms(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
        Ok(JsValue::from(now_ms() as f64))
    }

    let obj = ObjectInitializer::new(ctx)
        .function(
            NativeFunction::from_fn_ptr(wx_log),
            boa_engine::js_string!("log"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(wx_set_title),
            boa_engine::js_string!("setTitle"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(wx_store_set),
            boa_engine::js_string!("storeSet"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(wx_store_get),
            boa_engine::js_string!("storeGet"),
            1,
        )
        .function(
            NativeFunction::from_fn_ptr(wx_now_ms),
            boa_engine::js_string!("nowMs"),
            0,
        )
        .build();
    ctx.register_global_property(boa_engine::js_string!("wxhost"), obj, Attribute::all())?;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn dump(value: &JsValue, ctx: &mut Context) -> String {
    match value {
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        _ => value
            .to_string(ctx)
            .map(|s| s.to_std_string_escaped())
            .unwrap_or_else(|_| "<unprintable>".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_bursts_keep_only_the_latest_bounded_lines() {
        let mut eng = BoaEngine::new();
        eng.eval("for (let i = 0; i < 10000; i++) { wxhost.log(i); }")
            .unwrap();
        let logs = eng.logs();
        assert_eq!(logs.len(), MAX_LOG_LINES);
        assert_eq!(logs.first().unwrap(), "9744");
        assert_eq!(logs.last().unwrap(), "9999");
        assert_eq!(eng.drain_logs(), logs);
        assert!(eng.logs().is_empty());
    }

    #[test]
    fn long_unicode_log_lines_are_truncated_on_character_boundaries() {
        let mut eng = BoaEngine::new();
        eng.eval("wxhost.log('汉'.repeat(10000), 'must not grow');")
            .unwrap();
        let logs = eng.logs();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].len() <= MAX_LOG_LINE_BYTES);
        assert!(logs[0].starts_with('汉'));
        assert!(!logs[0].contains("must not grow"));
    }

    #[test]
    fn bridge_log_and_title() {
        let mut eng = BoaEngine::new();
        eng.eval("wxhost.log('a', 1, true);").unwrap();
        eng.eval("wxhost.setTitle('My Page');").unwrap();
        assert_eq!(eng.logs(), vec!["a 1 true"]);
        assert_eq!(eng.title(), "My Page");
    }

    #[test]
    fn storage_roundtrip() {
        let mut eng = BoaEngine::new();
        eng.eval("wxhost.storeSet('k','v');").unwrap();
        let got: String = eng.eval("wxhost.storeGet('k')").unwrap();
        assert_eq!(got, "v");
    }

    #[test]
    fn fresh_engine_resets_state() {
        let _ = BoaEngine::new(); // pollute
        let mut eng = BoaEngine::new();
        eng.eval("wxhost.log('only this');").unwrap();
        assert_eq!(eng.logs(), vec!["only this"]);
    }

    #[test]
    fn lifecycle_call() {
        let mut eng = BoaEngine::new();
        eng.eval(
            "globalThis.onLoad = function(p){ wxhost.log('load', p); };
             globalThis.onShow = function(){ wxhost.log('show'); };",
        )
        .unwrap();
        eng.call_global("onLoad", &[serde_json::Value::String("payload".into())])
            .unwrap();
        eng.call_global("onShow", &[]).unwrap();
        assert_eq!(eng.logs(), vec!["load payload", "show"]);
    }
}
