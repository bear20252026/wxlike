use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use wxlike_discovery::clock::ManualClock;
use wxlike_discovery::location_share::{
    BackgroundMode, Coord, CoordSystem, LocationShareService, MapProvider, NavigationPort,
    NavigationTarget, PlatformLocationPort, Poi, PoiPort, SharePrecision,
};
use wxlike_discovery::DiscoveryError;

const NOW: i64 = 1_700_000_000_000;

struct FakePlatform {
    mode: BackgroundMode,
}

impl PlatformLocationPort for FakePlatform {
    fn background_mode(&self) -> BackgroundMode {
        self.mode
    }
}

fn service(
    clock: Arc<ManualClock>,
    mode: BackgroundMode,
) -> LocationShareService<Arc<ManualClock>, FakePlatform> {
    LocationShareService::new(clock, FakePlatform { mode })
}

fn coord(lat: f64, lng: f64) -> Coord {
    Coord::new(lat, lng, CoordSystem::Gcj02).unwrap()
}

#[test]
fn denied_background_permission_blocks_share() {
    let clock = Arc::new(ManualClock::new(NOW));
    let shares = service(clock, BackgroundMode::Denied);
    assert_eq!(
        shares
            .start(
                "alice",
                "conv_1",
                vec!["alice".into(), "bob".into()],
                None,
                None,
                SharePrecision::Exact
            )
            .unwrap_err(),
        DiscoveryError::Unauthorized
    );
}

#[test]
fn two_members_share_positions_with_seq_interval_and_leave_semantics() {
    let clock = Arc::new(ManualClock::new(NOW));
    let shares = service(clock.clone(), BackgroundMode::Always);
    let session = shares
        .start(
            "alice",
            "conv_1",
            vec!["alice".into(), "bob".into(), "carol".into()],
            Some(60_000),
            Some(5_000),
            SharePrecision::Exact,
        )
        .unwrap();
    assert!(!session.background_limited);
    shares.join("bob", &session.id).unwrap();
    assert_eq!(
        shares.join("mallory", &session.id).unwrap_err(),
        DiscoveryError::Forbidden
    );

    let first = shares
        .update("bob", &session.id, coord(39.9, 116.4), 8.0, true)
        .unwrap();
    assert_eq!(first.seq, 1);
    assert_eq!(
        shares
            .update("bob", &session.id, coord(39.91, 116.41), 8.0, true)
            .unwrap_err(),
        DiscoveryError::RateLimited
    );
    clock.advance(5_001);
    let second = shares
        .update("bob", &session.id, coord(39.91, 116.41), 8.0, true)
        .unwrap();
    assert!(second.seq > first.seq);
    shares
        .update("alice", &session.id, coord(39.8, 116.3), 12.0, true)
        .unwrap();

    let latest = shares.latest("bob", &session.id).unwrap();
    assert_eq!(latest.len(), 2);
    assert!(latest.iter().all(|p| !p.stale));
    assert_eq!(
        shares.latest("mallory", &session.id).unwrap_err(),
        DiscoveryError::Forbidden
    );

    shares.leave("bob", &session.id).unwrap();
    assert_eq!(
        shares.latest("bob", &session.id).unwrap_err(),
        DiscoveryError::Forbidden
    );
    let latest = shares.latest("alice", &session.id).unwrap();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].user_id, "alice");
}

#[test]
fn revoked_member_and_stopped_session_cannot_read_new_positions() {
    let clock = Arc::new(ManualClock::new(NOW));
    let shares = service(clock.clone(), BackgroundMode::WhileInUse);
    let session = shares
        .start(
            "alice",
            "conv_1",
            vec!["alice".into(), "bob".into()],
            None,
            None,
            SharePrecision::Exact,
        )
        .unwrap();
    assert!(session.background_limited);
    shares.join("bob", &session.id).unwrap();
    shares
        .update("bob", &session.id, coord(39.9, 116.4), 9.0, true)
        .unwrap();
    shares.remove_member("alice", &session.id, "bob").unwrap();
    assert_eq!(
        shares.latest("bob", &session.id).unwrap_err(),
        DiscoveryError::Forbidden
    );
    assert_eq!(
        shares
            .update("bob", &session.id, coord(39.9, 116.4), 9.0, true)
            .unwrap_err(),
        DiscoveryError::Forbidden
    );
    shares.stop("alice", &session.id).unwrap();
    assert_eq!(
        shares.latest("alice", &session.id).unwrap_err(),
        DiscoveryError::Expired
    );
    assert_eq!(
        shares.stop("bob", &session.id).unwrap_err(),
        DiscoveryError::Forbidden
    );
}

#[test]
fn offline_updates_are_rejected_and_stale_positions_are_marked() {
    let clock = Arc::new(ManualClock::new(NOW));
    let shares = service(clock.clone(), BackgroundMode::Always);
    let session = shares
        .start(
            "alice",
            "conv_1",
            vec!["alice".into(), "bob".into()],
            None,
            Some(2_000),
            SharePrecision::Exact,
        )
        .unwrap();
    shares.join("bob", &session.id).unwrap();
    shares
        .update("bob", &session.id, coord(39.9, 116.4), 9.0, true)
        .unwrap();
    assert_eq!(
        shares
            .update("bob", &session.id, coord(39.91, 116.41), 9.0, false)
            .unwrap_err(),
        DiscoveryError::DependencyUnavailable
    );
    clock.advance(7_000);
    let latest = shares.latest("alice", &session.id).unwrap();
    assert!(latest[0].stale);
    assert_eq!(latest[0].updated_at_ms, NOW);
}

#[test]
fn approximate_precision_rounds_coordinates() {
    let clock = Arc::new(ManualClock::new(NOW));
    let shares = service(clock, BackgroundMode::Always);
    let session = shares
        .start(
            "alice",
            "conv_1",
            vec!["alice".into(), "bob".into()],
            None,
            None,
            SharePrecision::Approx100m,
        )
        .unwrap();
    shares.join("bob", &session.id).unwrap();
    let view = shares
        .update(
            "bob",
            &session.id,
            coord(39.912_345, 116.456_789),
            9.0,
            true,
        )
        .unwrap();
    assert!((view.coord.lat - 39.912).abs() < 1e-9);
    assert!((view.coord.lng - 116.457).abs() < 1e-9);
}

struct FakePoi {
    calls: Mutex<Vec<(String, u32)>>,
}

#[async_trait]
impl PoiPort for FakePoi {
    async fn search(
        &self,
        query: &str,
        near: Coord,
        limit: u32,
    ) -> Result<Vec<Poi>, DiscoveryError> {
        self.calls.lock().unwrap().push((query.to_owned(), limit));
        Ok(vec![Poi {
            id: "poi_1".into(),
            name: format!("{query} cafe"),
            address: "street 1".into(),
            coord: near,
            category: "cafe".into(),
        }])
    }
}

struct FakeNavigation {
    opened: Mutex<Vec<MapProvider>>,
}

#[async_trait]
impl NavigationPort for FakeNavigation {
    async fn open(&self, target: &NavigationTarget) -> Result<(), DiscoveryError> {
        self.opened.lock().unwrap().push(target.provider);
        Ok(())
    }
}

#[tokio::test]
async fn poi_and_navigation_use_ports_and_reject_cross_crs_targets() {
    let clock = Arc::new(ManualClock::new(NOW));
    let shares = service(clock, BackgroundMode::Always);
    let pois = FakePoi {
        calls: Mutex::new(Vec::new()),
    };
    let found = shares
        .search_poi(&pois, "coffee", coord(39.9, 116.4), 10)
        .await
        .unwrap();
    assert_eq!(found[0].name, "coffee cafe");
    assert_eq!(*pois.calls.lock().unwrap(), vec![("coffee".into(), 10)]);
    assert!(shares
        .search_poi(&pois, "", coord(39.9, 116.4), 10)
        .await
        .is_err());

    let navigation = FakeNavigation {
        opened: Mutex::new(Vec::new()),
    };
    let mismatched = Coord::new(39.9, 116.4, CoordSystem::Wgs84).unwrap();
    assert_eq!(
        NavigationTarget::new(MapProvider::Amap, mismatched, "cafe").unwrap_err(),
        DiscoveryError::InvalidArgument("coordinate system")
    );
    let target = NavigationTarget::new(MapProvider::Amap, coord(39.9, 116.4), "cafe").unwrap();
    shares.navigate(&navigation, &target).await.unwrap();
    assert_eq!(*navigation.opened.lock().unwrap(), vec![MapProvider::Amap]);
}

#[test]
fn session_ttl_expires() {
    let clock = Arc::new(ManualClock::new(NOW));
    let shares = service(clock.clone(), BackgroundMode::Always);
    let session = shares
        .start(
            "alice",
            "conv_1",
            vec!["alice".into(), "bob".into()],
            Some(1_000),
            None,
            SharePrecision::Exact,
        )
        .unwrap();
    clock.advance(1_001);
    shares.join("bob", &session.id).unwrap_err();
    assert_eq!(
        shares.latest("alice", &session.id).unwrap_err(),
        DiscoveryError::Expired
    );
}
