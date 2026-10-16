//! WP-13 observability: what the hub records, in Prometheus text format.
//!
//! Metrics (all labeled `hub=<hub_id>`, so several hubs in one process or one
//! scrape stay distinguishable):
//!
//! | name | kind | meaning |
//! |------|------|---------|
//! | `wxlike_hub_connections` | gauge | users currently bound to this hub |
//! | `wxlike_hub_messages_total` | counter | envelopes accepted for routing |
//! | `wxlike_offline_parked` | gauge | frames parked for offline users |
//! | `wxlike_delivery_seconds` | histogram | inbound → routing-done latency |
//!
//! The latency is exported as a histogram; p99 is
//! `histogram_quantile(0.99, rate(wxlike_delivery_seconds_bucket[1m]))` in
//! Prometheus, which is the native way to ask it.
//!
//! Rendering: [`prometheus_text`] installs a global recorder on first use
//! (idempotent, safe to call from tests and the server alike) and returns the
//! current exposition text. If another recorder is already installed the
//! macros become no-ops and rendering returns what it can — observability must
//! never take the data path down with it.

use std::sync::OnceLock;

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Per-hub recorder handle. Cheap to clone-construct; the metrics crate keeps
/// the aggregation state globally.
#[derive(Clone, Debug)]
pub struct HubMetrics {
    hub_id: String,
}

impl HubMetrics {
    pub fn new(hub_id: impl Into<String>) -> Self {
        // Install the global recorder before the first macro call: values
        // recorded before installation are lost, and the hub records from
        // its very first connection.
        let _ = handle();
        Self {
            hub_id: hub_id.into(),
        }
    }

    pub fn connected(&self) {
        metrics::gauge!("wxlike_hub_connections", "hub" => self.hub_id.clone()).increment(1.0);
    }

    pub fn disconnected(&self) {
        metrics::gauge!("wxlike_hub_connections", "hub" => self.hub_id.clone()).decrement(1.0);
    }

    pub fn message_routed(&self) {
        metrics::counter!("wxlike_hub_messages_total", "hub" => self.hub_id.clone()).increment(1);
    }

    /// `delta` may be negative (drain).
    pub fn offline_parked(&self, delta: i64) {
        metrics::gauge!("wxlike_offline_parked", "hub" => self.hub_id.clone())
            .increment(delta as f64);
    }

    pub fn delivery_latency(&self, elapsed: std::time::Duration) {
        metrics::histogram!("wxlike_delivery_seconds", "hub" => self.hub_id.clone())
            .record(elapsed.as_secs_f64());
    }
}

static HANDLE: OnceLock<Option<PrometheusHandle>> = OnceLock::new();

/// Latency buckets (seconds): half a millisecond to 2.5 s, geometric-ish, so
/// the p99 budget region (tens of ms) has real resolution.
const LATENCY_BUCKETS: [f64; 12] = [
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
];

fn handle() -> Option<&'static PrometheusHandle> {
    HANDLE
        .get_or_init(|| {
            let builder = PrometheusBuilder::new().set_buckets(&LATENCY_BUCKETS);
            let builder = match builder {
                Ok(b) => b,
                Err(err) => {
                    eprintln!("[metrics] bucket config rejected ({err}); default buckets");
                    PrometheusBuilder::new()
                }
            };
            builder
                .install_recorder()
                .map_err(|err| {
                    eprintln!("[metrics] recorder not installed ({err}); metrics are no-ops");
                    err
                })
                .ok()
        })
        .as_ref()
}

/// Installs the recorder if needed, then returns the Prometheus exposition
/// text. Empty string when no recorder could be installed.
pub fn prometheus_text() -> String {
    handle().map_or_else(String::new, |h| h.render())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_contains_all_hub_series() {
        // install + record through the real pipeline
        let m = HubMetrics::new("metrics-test-hub");
        m.connected();
        m.message_routed();
        m.offline_parked(2);
        m.delivery_latency(std::time::Duration::from_millis(3));

        let text = prometheus_text();
        assert!(text.contains("wxlike_hub_connections"), "{text}");
        assert!(text.contains("wxlike_hub_messages_total"), "{text}");
        assert!(text.contains("wxlike_offline_parked"), "{text}");
        assert!(text.contains("wxlike_delivery_seconds_bucket"), "{text}");
        assert!(text.contains("hub=\"metrics-test-hub\""), "{text}");
    }
}
