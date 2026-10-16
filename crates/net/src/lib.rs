//! wxlike-net: QUIC long-connection transport, broadcast hub (legacy),
//! and MQ-routed hub for multi-node deployment.

pub mod client;
pub mod framing;
pub mod mq_hub;
pub mod offline;
pub mod server;
pub mod transport;
pub mod wire;

pub use client::Client;
pub use mq_hub::{run_mq_hub, run_mq_hub_with, HubHandle, MqHub, MqHubOptions};
pub use server::run_hub;
