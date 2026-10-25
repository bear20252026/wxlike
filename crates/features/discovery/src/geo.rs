use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use crate::clock::Clock;
use crate::error::{DiscoveryError, Result};

const CELL_DEG: f64 = 0.01;
const METERS_PER_DEG_LAT: f64 = 111_320.0;
const MAX_RADIUS_M: f64 = 10_000.0;
const DEFAULT_TTL_MS: i64 = 30 * 60 * 1000;
const MAX_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const QUERY_WINDOW_MS: i64 = 60 * 1000;
const DEFAULT_QUERIES_PER_MINUTE: u32 = 30;
const MAX_RESULTS: usize = 50;
const BAND_500_M: f64 = 500.0;
const BAND_1_KM: f64 = 1_000.0;
const BAND_3_KM: f64 = 3_000.0;
const BAND_10_KM: f64 = 10_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cell {
    pub lat_idx: i32,
    pub lng_idx: i32,
}

impl Cell {
    pub fn quantize(lat: f64, lng: f64) -> Result<Self> {
        validate_coords(lat, lng)?;
        Ok(Self {
            lat_idx: (lat / CELL_DEG).floor() as i32,
            lng_idx: (lng / CELL_DEG).floor() as i32,
        })
    }

    pub fn center(self) -> (f64, f64) {
        (
            (f64::from(self.lat_idx) + 0.5) * CELL_DEG,
            (f64::from(self.lng_idx) + 0.5) * CELL_DEG,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Friends,
    Everyone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistanceBand {
    Under500m,
    Under1Km,
    Under3Km,
    Under10Km,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceView {
    pub cell: Cell,
    pub visibility: Visibility,
    pub expires_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyPeer {
    pub user_id: String,
    pub cell: Cell,
    pub distance_band: DistanceBand,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportRecord {
    pub reporter: String,
    pub target: String,
    pub reason: String,
    pub reported_at_ms: i64,
}

#[derive(Clone, Debug)]
pub struct GeoConfig {
    pub default_ttl_ms: i64,
    pub max_ttl_ms: i64,
    pub queries_per_minute: u32,
    pub max_results: usize,
}

impl Default for GeoConfig {
    fn default() -> Self {
        Self {
            default_ttl_ms: DEFAULT_TTL_MS,
            max_ttl_ms: MAX_TTL_MS,
            queries_per_minute: DEFAULT_QUERIES_PER_MINUTE,
            max_results: MAX_RESULTS,
        }
    }
}

struct Presence {
    cell: Cell,
    visibility: Visibility,
    expires_at_ms: i64,
    updated_at_ms: i64,
}

pub struct GeoService<C: Clock> {
    clock: C,
    config: GeoConfig,
    presences: Mutex<HashMap<String, Presence>>,
    opted_in: Mutex<HashSet<String>>,
    query_log: Mutex<HashMap<String, VecDeque<i64>>>,
    blocklist: Mutex<HashSet<(String, String)>>,
    reports: Mutex<Vec<ReportRecord>>,
}

impl<C: Clock> GeoService<C> {
    pub fn new(clock: C, config: GeoConfig) -> Self {
        Self {
            clock,
            config,
            presences: Mutex::new(HashMap::new()),
            opted_in: Mutex::new(HashSet::new()),
            query_log: Mutex::new(HashMap::new()),
            blocklist: Mutex::new(HashSet::new()),
            reports: Mutex::new(Vec::new()),
        }
    }

    /// 位置默认关闭；只有显式授权后才允许发布。
    pub fn set_opt_in(&self, actor: &str, enabled: bool) -> Result<()> {
        if actor.is_empty() {
            return Err(DiscoveryError::Unauthorized);
        }
        let mut opted_in = self
            .opted_in
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        if enabled {
            opted_in.insert(actor.to_owned());
        } else {
            opted_in.remove(actor);
            drop(opted_in);
            self.clear_presence(actor)?;
        }
        Ok(())
    }

    pub fn publish(
        &self,
        actor: &str,
        lat: f64,
        lng: f64,
        visibility: Visibility,
        ttl_ms: Option<i64>,
    ) -> Result<PresenceView> {
        let cell = Cell::quantize(lat, lng)?;
        if !self
            .opted_in
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .contains(actor)
        {
            return Err(DiscoveryError::Unauthorized);
        }
        let now_ms = self.clock.now_ms();
        let ttl_ms = ttl_ms
            .unwrap_or(self.config.default_ttl_ms)
            .clamp(1, self.config.max_ttl_ms);
        let expires_at_ms = now_ms + ttl_ms;
        let view = PresenceView {
            cell,
            visibility,
            expires_at_ms,
            updated_at_ms: now_ms,
        };
        self.presences
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .insert(
                actor.to_owned(),
                Presence {
                    cell,
                    visibility,
                    expires_at_ms,
                    updated_at_ms: now_ms,
                },
            );
        Ok(view)
    }

    /// 撤回授权：立即删除可发现状态，后续 `nearby` 不再返回该用户。
    pub fn revoke(&self, actor: &str) -> Result<()> {
        self.clear_presence(actor)?;
        self.opted_in
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .remove(actor);
        Ok(())
    }

    pub fn presence(&self, actor: &str) -> Option<PresenceView> {
        let now_ms = self.clock.now_ms();
        let presences = self.presences.lock().ok()?;
        presences
            .get(actor)
            .filter(|p| p.expires_at_ms > now_ms)
            .map(|p| PresenceView {
                cell: p.cell,
                visibility: p.visibility,
                expires_at_ms: p.expires_at_ms,
                updated_at_ms: p.updated_at_ms,
            })
    }

    /// 只返回粗网格与距离档，永远不回精确坐标或精确距离。
    pub fn nearby(
        &self,
        actor: &str,
        lat: f64,
        lng: f64,
        radius_m: f64,
    ) -> Result<Vec<NearbyPeer>> {
        let origin = Cell::quantize(lat, lng)?;
        if !(radius_m > 0.0 && radius_m <= MAX_RADIUS_M) {
            return Err(DiscoveryError::InvalidArgument("radius"));
        }
        self.check_rate_limit(actor)?;
        let now_ms = self.clock.now_ms();
        let presences = self
            .presences
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let blocked = self
            .blocklist
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .clone();
        let mut peers = Vec::new();
        for (user_id, presence) in presences.iter() {
            if user_id == actor || presence.expires_at_ms <= now_ms {
                continue;
            }
            if is_blocked(&blocked, actor, user_id) {
                continue;
            }
            let Some(band) = band_between(origin, presence.cell, radius_m) else {
                continue;
            };
            peers.push(NearbyPeer {
                user_id: user_id.clone(),
                cell: presence.cell,
                distance_band: band,
            });
        }
        peers.sort_by(|a, b| {
            band_rank(a.distance_band)
                .cmp(&band_rank(b.distance_band))
                .then_with(|| a.user_id.cmp(&b.user_id))
        });
        peers.truncate(self.config.max_results);
        Ok(peers)
    }

    pub fn block(&self, actor: &str, target: &str) -> Result<()> {
        if actor.is_empty() || target.is_empty() || actor == target {
            return Err(DiscoveryError::InvalidArgument("block target"));
        }
        self.blocklist
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .insert((actor.to_owned(), target.to_owned()));
        Ok(())
    }

    pub fn unblock(&self, actor: &str, target: &str) -> Result<()> {
        self.blocklist
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .remove(&(actor.to_owned(), target.to_owned()));
        Ok(())
    }

    pub fn report(&self, actor: &str, target: &str, reason: &str) -> Result<ReportRecord> {
        if actor.is_empty() || target.is_empty() || reason.is_empty() || reason.len() > 200 {
            return Err(DiscoveryError::InvalidArgument("report"));
        }
        let record = ReportRecord {
            reporter: actor.to_owned(),
            target: target.to_owned(),
            reason: reason.to_owned(),
            reported_at_ms: self.clock.now_ms(),
        };
        self.reports
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .push(record.clone());
        Ok(record)
    }

    pub fn reports_for(&self, target: &str) -> Vec<ReportRecord> {
        self.reports
            .lock()
            .map(|reports| {
                reports
                    .iter()
                    .filter(|r| r.target == target)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn clear_presence(&self, actor: &str) -> Result<()> {
        self.presences
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .remove(actor);
        Ok(())
    }

    fn check_rate_limit(&self, actor: &str) -> Result<()> {
        let now_ms = self.clock.now_ms();
        let mut log = self
            .query_log
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let entries = log.entry(actor.to_owned()).or_default();
        while entries
            .front()
            .is_some_and(|ts| now_ms - ts >= QUERY_WINDOW_MS)
        {
            entries.pop_front();
        }
        if entries.len() >= self.config.queries_per_minute as usize {
            return Err(DiscoveryError::RateLimited);
        }
        entries.push_back(now_ms);
        Ok(())
    }
}

fn validate_coords(lat: f64, lng: f64) -> Result<()> {
    if !lat.is_finite()
        || !lng.is_finite()
        || !(-90.0..=90.0).contains(&lat)
        || !(-180.0..=180.0).contains(&lng)
    {
        return Err(DiscoveryError::InvalidArgument("coordinates"));
    }
    Ok(())
}

fn band_between(origin: Cell, peer: Cell, radius_m: f64) -> Option<DistanceBand> {
    let (lat0, lng0) = origin.center();
    let (lat1, lng1) = peer.center();
    let dlat = (lat1 - lat0) * METERS_PER_DEG_LAT;
    let dlng = (lng1 - lng0) * METERS_PER_DEG_LAT * lat0.to_radians().cos().abs();
    let distance = (dlat * dlat + dlng * dlng).sqrt();
    if distance > radius_m {
        return None;
    }
    Some(if distance <= BAND_500_M {
        DistanceBand::Under500m
    } else if distance <= BAND_1_KM {
        DistanceBand::Under1Km
    } else if distance <= BAND_3_KM {
        DistanceBand::Under3Km
    } else if distance <= BAND_10_KM {
        DistanceBand::Under10Km
    } else {
        return None;
    })
}

fn band_rank(band: DistanceBand) -> u8 {
    match band {
        DistanceBand::Under500m => 0,
        DistanceBand::Under1Km => 1,
        DistanceBand::Under3Km => 2,
        DistanceBand::Under10Km => 3,
    }
}

fn is_blocked(blocked: &HashSet<(String, String)>, actor: &str, other: &str) -> bool {
    blocked.contains(&(actor.to_owned(), other.to_owned()))
        || blocked.contains(&(other.to_owned(), actor.to_owned()))
}
