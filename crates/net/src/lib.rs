//! wxlike-net: QUIC long-connection transport, broadcast hub (legacy),
//! and MQ-routed hub for multi-node deployment.

pub mod auth;
pub mod avalanche;
pub mod client;
pub mod conn;
pub mod diag;
pub mod framing;
pub mod heartbeat;
pub mod membership;
pub mod metrics;
pub mod mq_hub;
pub mod offline;
pub mod policy;
pub mod server;
pub mod transport;
pub mod wire;

pub use auth::{HubAuthenticator, HubIdentity, StaticAuth};
pub use client::Client;
pub use conn::PeerConn;
pub use membership::Membership;
pub use metrics::{prometheus_text, HubMetrics};
pub use mq_hub::{
    run_mq_hub, run_mq_hub_with, HubHandle, MqHub, MqHubOptions, OfflinePush,
    DEFAULT_CLAIM_IDLE_MS, DEFAULT_DEDUP_CAP,
};
pub use policy::{DeliveryPolicy, SendGate};
pub use server::run_hub;
pub use transport::{cert_fingerprint, connect_tofu, TofuPins};
