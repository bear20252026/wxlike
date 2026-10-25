use std::sync::Arc;

use wxlike_discovery::clock::{Clock, ManualClock};
use wxlike_discovery::geo::{Cell, DistanceBand, GeoConfig, GeoService, Visibility};
use wxlike_discovery::matching::{MatchOutcome, MotionSample, ShakeMatcher};
use wxlike_discovery::DiscoveryError;

const NOW: i64 = 1_700_000_000_000;

fn geo_config(queries: u32) -> GeoConfig {
    GeoConfig {
        default_ttl_ms: 30_000,
        max_ttl_ms: 60_000,
        queries_per_minute: queries,
        max_results: 50,
    }
}

#[test]
fn location_is_off_until_explicitly_enabled_and_revocable() {
    let clock = Arc::new(ManualClock::new(NOW));
    let geo = GeoService::new(clock.clone(), geo_config(10));
    assert_eq!(
        geo.publish("alice", 39.9, 116.4, Visibility::Everyone, None)
            .unwrap_err(),
        DiscoveryError::Unauthorized
    );
    geo.set_opt_in("alice", true).unwrap();
    let view = geo
        .publish("alice", 39.9, 116.4, Visibility::Everyone, None)
        .unwrap();
    assert_eq!(view.cell, Cell::quantize(39.9, 116.4).unwrap());
    assert!(geo.presence("alice").is_some());
    geo.revoke("alice").unwrap();
    assert!(geo.presence("alice").is_none());
    assert!(geo.nearby("bob", 39.9, 116.4, 1_000.0).unwrap().is_empty());
}

#[test]
fn nearby_returns_coarse_cells_and_bands_only() {
    let clock = Arc::new(ManualClock::new(NOW));
    let geo = GeoService::new(clock, geo_config(10));
    geo.set_opt_in("alice", true).unwrap();
    let published = geo
        .publish("alice", 39.900_01, 116.400_01, Visibility::Everyone, None)
        .unwrap();
    for (lat, lng) in [
        (39.900_02, 116.400_02),
        (39.900_1, 116.400_1),
        (39.900_2, 116.400_2),
    ] {
        let peers = geo.nearby("bob", lat, lng, 1_000.0).unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].cell, published.cell);
        assert_eq!(peers[0].distance_band, DistanceBand::Under500m);
    }
    assert!(geo.nearby("bob", 39.9, 116.4, 0.0).is_err());
    assert!(geo.nearby("bob", 200.0, 116.4, 1_000.0).is_err());
}

#[test]
fn ttl_expiry_and_rate_limit_are_enforced() {
    let clock = Arc::new(ManualClock::new(NOW));
    let geo = GeoService::new(clock.clone(), geo_config(2));
    geo.set_opt_in("alice", true).unwrap();
    geo.publish("alice", 39.9, 116.4, Visibility::Everyone, Some(1_000))
        .unwrap();
    clock.advance(1_001);
    assert!(geo.nearby("bob", 39.9, 116.4, 1_000.0).unwrap().is_empty());

    geo.set_opt_in("alice", true).unwrap();
    geo.publish("alice", 39.9, 116.4, Visibility::Everyone, None)
        .unwrap();
    assert_eq!(geo.nearby("bob", 39.9, 116.4, 1_000.0).unwrap().len(), 1);
    assert_eq!(
        geo.nearby("bob", 39.9, 116.4, 1_000.0).unwrap_err(),
        DiscoveryError::RateLimited
    );
    clock.advance(60_001);
    geo.publish("alice", 39.9, 116.4, Visibility::Everyone, None)
        .unwrap();
    assert_eq!(geo.nearby("bob", 39.9, 116.4, 1_000.0).unwrap().len(), 1);
}

#[test]
fn blocklist_filters_both_directions_and_reports_are_traceable() {
    let clock = Arc::new(ManualClock::new(NOW));
    let geo = GeoService::new(clock.clone(), geo_config(20));
    geo.set_opt_in("alice", true).unwrap();
    geo.publish("alice", 39.9, 116.4, Visibility::Everyone, None)
        .unwrap();
    geo.block("bob", "alice").unwrap();
    assert!(geo.nearby("bob", 39.9, 116.4, 1_000.0).unwrap().is_empty());
    geo.block("alice", "bob").unwrap();
    geo.unblock("bob", "alice").unwrap();
    assert!(geo.nearby("bob", 39.9, 116.4, 1_000.0).unwrap().is_empty());

    let record = geo.report("bob", "alice", "repeated harassment").unwrap();
    assert_eq!(record.reported_at_ms, NOW);
    assert_eq!(geo.reports_for("alice").len(), 1);
    assert!(geo.report("bob", "alice", "").is_err());
}

#[test]
fn shake_requires_gesture_two_sided_confirmation_and_expires() {
    let clock = Arc::new(ManualClock::new(NOW));
    let matcher = ShakeMatcher::new(clock.clone());
    assert!(matcher
        .start(
            "alice",
            MotionSample {
                magnitude_g: 0.5,
                sampled_at_ms: NOW,
            }
        )
        .is_err());
    assert!(matcher
        .start(
            "alice",
            MotionSample {
                magnitude_g: 3.0,
                sampled_at_ms: NOW - 5_000,
            }
        )
        .is_err());

    let ticket = matcher
        .start(
            "alice",
            MotionSample {
                magnitude_g: 3.0,
                sampled_at_ms: NOW,
            },
        )
        .unwrap();
    assert!(matcher
        .start(
            "alice",
            MotionSample {
                magnitude_g: 3.0,
                sampled_at_ms: NOW,
            }
        )
        .is_err());
    assert!(matcher.join("alice", &ticket.id).is_err());
    let proposal = matcher.join("bob", &ticket.id).unwrap();
    assert!(matcher.join("carol", &ticket.id).is_err());

    assert_eq!(
        matcher.confirm("alice", &proposal).unwrap(),
        MatchOutcome::Pending {
            needs_host: false,
            needs_guest: true
        }
    );
    match matcher.confirm("bob", &proposal).unwrap() {
        MatchOutcome::Matched(record) => {
            assert_eq!(record.users, ["alice".to_owned(), "bob".to_owned()]);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matcher.ticket(&ticket.id).is_none());
}

#[test]
fn shake_window_expires_and_cancel_sets_cooldown() {
    let clock = Arc::new(ManualClock::new(NOW));
    let matcher = ShakeMatcher::new(clock.clone());
    let ticket = matcher
        .start(
            "alice",
            MotionSample {
                magnitude_g: 3.0,
                sampled_at_ms: NOW,
            },
        )
        .unwrap();
    clock.advance(30_001);
    assert!(matcher.ticket(&ticket.id).is_none());
    assert_eq!(
        matcher.join("bob", &ticket.id).unwrap_err(),
        DiscoveryError::Expired
    );
    clock.advance(60_000);
    let ticket = matcher
        .start(
            "alice",
            MotionSample {
                magnitude_g: 3.0,
                sampled_at_ms: clock.now_ms(),
            },
        )
        .unwrap();
    let _proposal = matcher.join("bob", &ticket.id).unwrap();
    matcher.cancel("alice", &ticket.id).unwrap();
    assert_eq!(
        matcher
            .start(
                "alice",
                MotionSample {
                    magnitude_g: 3.0,
                    sampled_at_ms: clock.now_ms(),
                }
            )
            .unwrap_err(),
        DiscoveryError::RateLimited
    );
}
