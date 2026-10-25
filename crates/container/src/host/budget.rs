//! Per-call resource budgets for a guest app instance.
//!
//! Budgets are accounted separately (CPU/memory/storage/network/bridge calls)
//! so a denial names what actually ran out. Limits are per launch, not per
//! process: terminating one mini-app must not reset another app's usage.

use crate::host::HostError;

const DESKTOP_CPU_MS: u64 = 5_000;
const DESKTOP_MEMORY: u64 = 256 * 1024 * 1024;
const DESKTOP_STORAGE: u64 = 16 * 1024 * 1024;
const DESKTOP_NETWORK: u64 = 8 * 1024 * 1024;
const DESKTOP_BRIDGE_CALLS: u64 = 512;

const WEB_CPU_MS: u64 = 3_000;
const WEB_MEMORY: u64 = 128 * 1024 * 1024;
const WEB_STORAGE: u64 = 8 * 1024 * 1024;
const WEB_NETWORK: u64 = 4 * 1024 * 1024;
const WEB_BRIDGE_CALLS: u64 = 256;

const MOBILE_CPU_MS: u64 = 3_000;
const MOBILE_MEMORY: u64 = 96 * 1024 * 1024;
const MOBILE_STORAGE: u64 = 8 * 1024 * 1024;
const MOBILE_NETWORK: u64 = 4 * 1024 * 1024;
const MOBILE_BRIDGE_CALLS: u64 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetLimits {
    pub cpu_millis: u64,
    pub memory_bytes: u64,
    pub storage_bytes: u64,
    pub network_bytes: u64,
    pub bridge_calls: u64,
}

impl BudgetLimits {
    pub const fn desktop() -> Self {
        Self {
            cpu_millis: DESKTOP_CPU_MS,
            memory_bytes: DESKTOP_MEMORY,
            storage_bytes: DESKTOP_STORAGE,
            network_bytes: DESKTOP_NETWORK,
            bridge_calls: DESKTOP_BRIDGE_CALLS,
        }
    }

    pub const fn web() -> Self {
        Self {
            cpu_millis: WEB_CPU_MS,
            memory_bytes: WEB_MEMORY,
            storage_bytes: WEB_STORAGE,
            network_bytes: WEB_NETWORK,
            bridge_calls: WEB_BRIDGE_CALLS,
        }
    }

    pub const fn mobile() -> Self {
        Self {
            cpu_millis: MOBILE_CPU_MS,
            memory_bytes: MOBILE_MEMORY,
            storage_bytes: MOBILE_STORAGE,
            network_bytes: MOBILE_NETWORK,
            bridge_calls: MOBILE_BRIDGE_CALLS,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BudgetUsage {
    pub cpu_millis: u64,
    pub memory_bytes: u64,
    pub storage_bytes: u64,
    pub network_bytes: u64,
    pub bridge_calls: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct BudgetLedger {
    limits: BudgetLimits,
    usage: BudgetUsage,
}

impl BudgetLedger {
    pub fn new(limits: BudgetLimits) -> Self {
        Self {
            limits,
            usage: BudgetUsage::default(),
        }
    }

    pub fn limits(&self) -> BudgetLimits {
        self.limits
    }

    pub fn usage(&self) -> BudgetUsage {
        self.usage
    }

    pub fn charge_cpu(&mut self, millis: u64) -> Result<(), HostError> {
        let used = checked_add(self.usage.cpu_millis, millis, "cpu")?;
        if used > self.limits.cpu_millis {
            return Err(HostError::BudgetExceeded("cpu"));
        }
        self.usage.cpu_millis = used;
        Ok(())
    }

    pub fn reserve_memory(&mut self, bytes: u64) -> Result<(), HostError> {
        let used = checked_add(self.usage.memory_bytes, bytes, "memory")?;
        if used > self.limits.memory_bytes {
            return Err(HostError::BudgetExceeded("memory"));
        }
        self.usage.memory_bytes = used;
        Ok(())
    }

    pub fn charge_storage(&mut self, bytes: u64) -> Result<(), HostError> {
        let used = checked_add(self.usage.storage_bytes, bytes, "storage")?;
        if used > self.limits.storage_bytes {
            return Err(HostError::BudgetExceeded("storage"));
        }
        self.usage.storage_bytes = used;
        Ok(())
    }

    pub fn release_storage(&mut self, bytes: u64) {
        self.usage.storage_bytes = self.usage.storage_bytes.saturating_sub(bytes);
    }

    pub fn charge_network(&mut self, bytes: u64) -> Result<(), HostError> {
        let used = checked_add(self.usage.network_bytes, bytes, "network")?;
        if used > self.limits.network_bytes {
            return Err(HostError::BudgetExceeded("network"));
        }
        self.usage.network_bytes = used;
        Ok(())
    }

    pub fn charge_bridge_call(&mut self) -> Result<(), HostError> {
        let used = checked_add(self.usage.bridge_calls, 1, "bridge calls")?;
        if used > self.limits.bridge_calls {
            return Err(HostError::BudgetExceeded("bridge calls"));
        }
        self.usage.bridge_calls = used;
        Ok(())
    }
}

fn checked_add(current: u64, delta: u64, budget: &'static str) -> Result<u64, HostError> {
    current
        .checked_add(delta)
        .ok_or(HostError::BudgetExceeded(budget))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> BudgetLedger {
        BudgetLedger::new(BudgetLimits {
            cpu_millis: 10,
            memory_bytes: 100,
            storage_bytes: 50,
            network_bytes: 40,
            bridge_calls: 2,
        })
    }

    #[test]
    fn every_budget_is_enforced_separately() {
        let mut ledger = tiny();
        assert_eq!(
            ledger.charge_cpu(11).unwrap_err(),
            HostError::BudgetExceeded("cpu")
        );
        assert_eq!(
            ledger.reserve_memory(101).unwrap_err(),
            HostError::BudgetExceeded("memory")
        );
        assert_eq!(
            ledger.charge_storage(51).unwrap_err(),
            HostError::BudgetExceeded("storage")
        );
        assert_eq!(
            ledger.charge_network(41).unwrap_err(),
            HostError::BudgetExceeded("network")
        );
        ledger.charge_bridge_call().unwrap();
        ledger.charge_bridge_call().unwrap();
        assert_eq!(
            ledger.charge_bridge_call().unwrap_err(),
            HostError::BudgetExceeded("bridge calls")
        );
    }

    #[test]
    fn storage_can_be_released_on_delete() {
        let mut ledger = tiny();
        ledger.charge_storage(50).unwrap();
        ledger.release_storage(30);
        ledger.charge_storage(30).unwrap();
        assert_eq!(ledger.usage().storage_bytes, 50);
    }
}
