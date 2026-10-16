//! Boa-backed engine with the `wxhost` bridge installed.
//!
//! Bridge state lives in a thread_local (Boa `NativeFunction` only accepts
//! `Copy` closures / fn pointers in 0.20). `BoaEngine::new` resets it, so each
//! engine instance starts from a clean slate on its thread.

use std::collections::BTreeMap;

use anyhow::Context as _;
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

impl Default for BoaEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl BoaEngine {
    pub fn new() -> Self {
        BRIDGE.with(|b| *b.borrow_mut() = BridgeState::default());
        let mut ctx = Context::default();
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
        let mut js_args = Vec::new();
        for a in args_json {
            js_args.push(serde_json::to_string(a).context("serialize arg")?);
        }
        // Build the call via eval to keep argument marshalling trivial at scaffold stage.
        let script = format!("{}({})", name, js_args.join(", "));
        let value = self
            .ctx
            .eval(Source::from_bytes(script.as_bytes()))
            .map_err(|e| anyhow::Error::msg(e.to_string()))?;
        Ok(dump(&value, &mut self.ctx))
    }

    pub fn title(&self) -> String {
        BRIDGE.with(|b| b.borrow().title.clone())
    }

    pub fn logs(&self) -> Vec<String> {
        BRIDGE.with(|b| b.borrow().logs.clone())
    }

    pub fn storage(&self) -> BTreeMap<String, String> {
        BRIDGE.with(|b| b.borrow().storage.clone())
    }
}

fn install_bridge(ctx: &mut Context) -> JsResult<()> {
    fn wx_log(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
        let line = args
            .iter()
            .map(|v| dump(v, ctx))
            .collect::<Vec<_>>()
            .join(" ");
        BRIDGE.with(|b| b.borrow_mut().logs.push(line));
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
