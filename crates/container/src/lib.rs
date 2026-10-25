//! wxlike-container: JS engine host for dynamic pages.
//!
//! Engine is abstracted behind [`engine::BoaEngine`] so the runtime can be swapped
//! (Boa now; quickjs-ng when Windows build story settles).

pub mod bridge;
pub mod engine;
pub mod package;

use anyhow::Context as _;
use wxlike_dpage::Page;

pub use engine::BoaEngine;

pub struct Container {
    engine: BoaEngine,
    page: Page,
}

impl Container {
    /// Load a page package and prepare the host bridge. No JS runs yet.
    pub fn load(page: Page) -> anyhow::Result<Self> {
        let engine = BoaEngine::new();
        Ok(Self { engine, page })
    }

    /// Run the page lifecycle: eval entry script, then `onLoad(payload)`, `onShow()`.
    /// Returns the title the page set via `wxhost.setTitle` (if any).
    pub fn start(&mut self, payload_json: &str) -> anyhow::Result<String> {
        let code = self.page.main_js()?;
        self.engine.eval(code).context("eval entry script")?;
        let payload = serde_json::Value::parse_js(payload_json);
        self.engine
            .call_global("onLoad", &[payload])
            .context("onLoad")?;
        self.engine.call_global("onShow", &[]).context("onShow")?;
        Ok(self.engine.title().to_string())
    }

    pub fn page(&self) -> &Page {
        &self.page
    }

    pub fn logs(&self) -> Vec<String> {
        self.engine.logs()
    }

    /// Consume the bounded log buffer without cloning its strings.
    pub fn drain_logs(&self) -> Vec<String> {
        self.engine.drain_logs()
    }
}

/// Helper on serde_json::Value: parse with a friendly error (used by demo pages).
trait ParseJs {
    fn parse_js(s: &str) -> serde_json::Value;
}

impl ParseJs for serde_json::Value {
    fn parse_js(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.to_string()))
    }
}
