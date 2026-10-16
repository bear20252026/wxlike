//! Bridge state shared between the host and JS land.

#[derive(Default, Clone, Debug)]
pub struct BridgeState {
    pub logs: Vec<String>,
    pub title: String,
    pub storage: std::collections::BTreeMap<String, String>,
}
