//! Mini-app host: one lifecycle and one budget ledger per launched app.
//!
//! The host owns *policy*; the platform shell owns the actual WebView/iframe.
//! Desktop and mobile receive a per-app data directory and custom scheme,
//! the web target receives a unique origin plus a sandboxed iframe. Guest
//! code never receives the host session token or the main IPC surface.

pub mod budget;
pub mod profile;

use std::time::Instant;

use wxlike_dpage::Page;

use crate::Container;
use budget::{BudgetLedger, BudgetLimits};
use profile::{DeviceTarget, IsolationProfile};

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum HostError {
    #[error("invalid app id")]
    BadAppId,
    #[error("invalid storage key")]
    BadStorageKey,
    #[error("host is {0}, expected {1}")]
    InvalidState(&'static str, &'static str),
    #[error("budget exceeded: {0}")]
    BudgetExceeded(&'static str),
    #[error("guest error: {0}")]
    Guest(String),
    #[error("wasm version {0} is not on the patched allowlist")]
    WasmVersionNotAllowed(String),
    #[error("page could not be loaded: {0}")]
    Load(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostState {
    Running,
    Paused,
    Terminated,
}

impl HostState {
    pub fn as_str(self) -> &'static str {
        match self {
            HostState::Running => "running",
            HostState::Paused => "paused",
            HostState::Terminated => "terminated",
        }
    }
}

#[derive(Debug)]
pub struct MiniHost {
    profile: IsolationProfile,
    budgets: BudgetLedger,
    state: HostState,
    session_id: String,
}

impl MiniHost {
    pub fn launch(
        target: DeviceTarget,
        app_id: &str,
        limits: BudgetLimits,
    ) -> Result<Self, HostError> {
        let profile = IsolationProfile::new(target, app_id)?;
        Ok(Self {
            profile,
            budgets: BudgetLedger::new(limits),
            state: HostState::Running,
            session_id: new_session_id(),
        })
    }

    pub fn launch_default(target: DeviceTarget, app_id: &str) -> Result<Self, HostError> {
        Self::launch(target, app_id, default_limits(target))
    }

    pub fn state(&self) -> HostState {
        self.state
    }

    pub fn profile(&self) -> &IsolationProfile {
        &self.profile
    }

    pub fn budgets(&self) -> &BudgetLedger {
        &self.budgets
    }

    pub fn budgets_mut(&mut self) -> &mut BudgetLedger {
        &mut self.budgets
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn pause(&mut self) -> Result<(), HostError> {
        self.expect(HostState::Running)?;
        self.state = HostState::Paused;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<(), HostError> {
        self.expect(HostState::Paused)?;
        self.state = HostState::Running;
        Ok(())
    }

    pub fn terminate(&mut self) -> Result<(), HostError> {
        if self.state == HostState::Terminated {
            return Err(HostError::InvalidState("terminated", "running"));
        }
        self.state = HostState::Terminated;
        Ok(())
    }

    /// Run the page lifecycle under the launch budget.
    ///
    /// * an infinite loop trips Boa's iteration limit and terminates the host;
    /// * a payload larger than the memory budget is refused before eval;
    /// * exceeding the CPU budget terminates the host after the call.
    pub fn run(&mut self, page: &Page, payload: &str) -> Result<String, HostError> {
        self.expect(HostState::Running)?;
        let page_bytes: u64 = page.files.values().map(|f| f.len() as u64).sum();
        if let Err(err) = self.budgets.reserve_memory(page_bytes) {
            self.state = HostState::Terminated;
            return Err(err);
        }
        let started = Instant::now();
        let mut container =
            Container::load(page.clone()).map_err(|e| HostError::Load(e.to_string()))?;
        let result = container.start(payload);
        let elapsed = started.elapsed().as_millis().max(1) as u64;
        if let Err(err) = self.budgets.charge_cpu(elapsed) {
            self.state = HostState::Terminated;
            return Err(err);
        }
        match result {
            Ok(title) => Ok(title),
            Err(err) => {
                let message = err.to_string();
                if message.to_lowercase().contains("iteration") {
                    self.state = HostState::Terminated;
                    Err(HostError::BudgetExceeded("cpu"))
                } else {
                    Err(HostError::Guest(message))
                }
            }
        }
    }

    fn expect(&self, expected: HostState) -> Result<(), HostError> {
        if self.state == expected {
            Ok(())
        } else {
            Err(HostError::InvalidState(
                self.state.as_str(),
                expected.as_str(),
            ))
        }
    }
}

fn default_limits(target: DeviceTarget) -> BudgetLimits {
    match target {
        DeviceTarget::Desktop => BudgetLimits::desktop(),
        DeviceTarget::Web => BudgetLimits::web(),
        DeviceTarget::Ios | DeviceTarget::Android => BudgetLimits::mobile(),
    }
}

fn new_session_id() -> String {
    use rand::RngCore as _;
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);
    hex::encode(nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_transitions_are_strict() {
        let mut host = MiniHost::launch_default(DeviceTarget::Desktop, "app.one").unwrap();
        assert_eq!(host.state(), HostState::Running);
        host.pause().unwrap();
        assert_eq!(host.state(), HostState::Paused);
        assert_eq!(
            host.pause().unwrap_err(),
            HostError::InvalidState("paused", "running")
        );
        host.resume().unwrap();
        host.terminate().unwrap();
        assert_eq!(
            host.terminate().unwrap_err(),
            HostError::InvalidState("terminated", "running")
        );
    }

    #[test]
    fn sessions_are_unique_per_launch() {
        let a = MiniHost::launch_default(DeviceTarget::Web, "app.one").unwrap();
        let b = MiniHost::launch_default(DeviceTarget::Web, "app.one").unwrap();
        assert_ne!(a.session_id(), b.session_id());
        assert_eq!(a.session_id().len(), 32);
    }

    #[test]
    fn invalid_app_ids_are_refused() {
        for bad in ["", "../evil", "-app", ".app", "app/one"] {
            assert_eq!(
                MiniHost::launch_default(DeviceTarget::Desktop, bad).unwrap_err(),
                HostError::BadAppId
            );
        }
    }
}
