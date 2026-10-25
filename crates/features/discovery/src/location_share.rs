use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::clock::Clock;
use crate::error::{DiscoveryError, Result};
use crate::id::new_id;

const DEFAULT_TTL_MS: i64 = 60 * 60 * 1000;
const MAX_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const MIN_INTERVAL_MS: i64 = 1_000;
const DEFAULT_INTERVAL_MS: i64 = 5_000;
const MAX_MEMBERS: usize = 50;
const STALE_INTERVALS: i64 = 3;
const MAX_ACCURACY_M: f32 = 10_000.0;
const APPROX_DECIMALS: f64 = 1_000.0;
const POI_MAX_RESULTS: u32 = 50;
const MAX_NAME_BYTES: usize = 128;

/// 坐标系必须随坐标一起传递；不同坐标系不得静默混用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordSystem {
    Wgs84,
    Gcj02,
    Bd09,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coord {
    pub lat: f64,
    pub lng: f64,
    pub system: CoordSystem,
}

impl Coord {
    pub fn new(lat: f64, lng: f64, system: CoordSystem) -> Result<Self> {
        if !lat.is_finite()
            || !lng.is_finite()
            || !(-90.0..=90.0).contains(&lat)
            || !(-180.0..=180.0).contains(&lng)
        {
            return Err(DiscoveryError::InvalidArgument("coordinates"));
        }
        Ok(Self { lat, lng, system })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharePrecision {
    Exact,
    Approx100m,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundMode {
    WhileInUse,
    Always,
    Denied,
}

#[async_trait]
pub trait PlatformLocationPort: Send + Sync {
    fn background_mode(&self) -> BackgroundMode;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareSession {
    pub id: String,
    pub conversation_id: String,
    pub host: String,
    pub members: BTreeSet<String>,
    pub expires_at_ms: i64,
    pub interval_ms: i64,
    pub precision: SharePrecision,
    pub background_limited: bool,
    pub state: ShareState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareState {
    Active,
    Stopped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PositionView {
    pub user_id: String,
    pub coord: Coord,
    pub accuracy_m: f32,
    pub seq: u64,
    pub updated_at_ms: i64,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Poi {
    pub id: String,
    pub name: String,
    pub address: String,
    pub coord: Coord,
    pub category: String,
}

#[async_trait]
pub trait PoiPort: Send + Sync {
    async fn search(&self, query: &str, near: Coord, limit: u32) -> Result<Vec<Poi>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapProvider {
    Amap,
    Baidu,
    Apple,
}

impl MapProvider {
    fn accepts(self, system: CoordSystem) -> bool {
        match self {
            Self::Amap => system == CoordSystem::Gcj02,
            Self::Baidu => system == CoordSystem::Bd09,
            Self::Apple => system == CoordSystem::Wgs84,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NavigationTarget {
    pub provider: MapProvider,
    pub coord: Coord,
    pub name: String,
}

impl NavigationTarget {
    pub fn new(provider: MapProvider, coord: Coord, name: &str) -> Result<Self> {
        if name.is_empty() || name.len() > MAX_NAME_BYTES {
            return Err(DiscoveryError::InvalidArgument("navigation name"));
        }
        if !provider.accepts(coord.system) {
            return Err(DiscoveryError::InvalidArgument("coordinate system"));
        }
        Ok(Self {
            provider,
            coord,
            name: name.to_owned(),
        })
    }
}

#[async_trait]
pub trait NavigationPort: Send + Sync {
    async fn open(&self, target: &NavigationTarget) -> Result<()>;
}

struct SessionRecord {
    conversation_id: String,
    host: String,
    members: BTreeSet<String>,
    joined: BTreeSet<String>,
    expires_at_ms: i64,
    interval_ms: i64,
    precision: SharePrecision,
    background_limited: bool,
    state: ShareState,
    seq: u64,
}

struct PositionRecord {
    coord: Coord,
    accuracy_m: f32,
    seq: u64,
    updated_at_ms: i64,
}

pub struct LocationShareService<C: Clock, L: PlatformLocationPort> {
    clock: C,
    platform: L,
    sessions: Mutex<HashMap<String, SessionRecord>>,
    positions: Mutex<HashMap<String, HashMap<String, PositionRecord>>>,
}

impl<C: Clock, L: PlatformLocationPort> LocationShareService<C, L> {
    pub fn new(clock: C, platform: L) -> Self {
        Self {
            clock,
            platform,
            sessions: Mutex::new(HashMap::new()),
            positions: Mutex::new(HashMap::new()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &self,
        host: &str,
        conversation_id: &str,
        mut allowed_members: Vec<String>,
        ttl_ms: Option<i64>,
        interval_ms: Option<i64>,
        precision: SharePrecision,
    ) -> Result<ShareSession> {
        if host.is_empty() || conversation_id.is_empty() || allowed_members.is_empty() {
            return Err(DiscoveryError::InvalidArgument("share start"));
        }
        let mode = self.platform.background_mode();
        if mode == BackgroundMode::Denied {
            return Err(DiscoveryError::Unauthorized);
        }
        allowed_members.sort();
        allowed_members.dedup();
        if allowed_members.len() > MAX_MEMBERS {
            return Err(DiscoveryError::InvalidArgument("session member cap"));
        }
        if !allowed_members.iter().any(|member| member == host) {
            allowed_members.push(host.to_owned());
        }
        let now_ms = self.clock.now_ms();
        let ttl_ms = ttl_ms.unwrap_or(DEFAULT_TTL_MS).clamp(1, MAX_TTL_MS);
        let interval_ms = interval_ms
            .unwrap_or(DEFAULT_INTERVAL_MS)
            .clamp(MIN_INTERVAL_MS, DEFAULT_TTL_MS);
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        if sessions
            .values()
            .any(|s| s.host == host && s.state == ShareState::Active && s.expires_at_ms > now_ms)
        {
            return Err(DiscoveryError::Conflict("active share session"));
        }
        let id = new_id("share");
        let record = SessionRecord {
            conversation_id: conversation_id.to_owned(),
            host: host.to_owned(),
            members: allowed_members.iter().cloned().collect(),
            joined: [host.to_owned()].into_iter().collect(),
            expires_at_ms: now_ms + ttl_ms,
            interval_ms,
            precision,
            background_limited: mode == BackgroundMode::WhileInUse,
            state: ShareState::Active,
            seq: 0,
        };
        let view = session_view(&id, &record);
        sessions.insert(id.clone(), record);
        Ok(ShareSession { id, ..view })
    }

    /// 显式加入才可读位置；会话成员由 G03 的会话授权集传入。
    pub fn join(&self, actor: &str, session_id: &str) -> Result<ShareSession> {
        let now_ms = self.clock.now_ms();
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let record = active_session(&mut sessions, session_id, now_ms)?;
        if !record.members.contains(actor) {
            return Err(DiscoveryError::Forbidden);
        }
        record.joined.insert(actor.to_owned());
        Ok(session_view(session_id, record))
    }

    pub fn update(
        &self,
        actor: &str,
        session_id: &str,
        coord: Coord,
        accuracy_m: f32,
        online: bool,
    ) -> Result<PositionView> {
        if !online {
            return Err(DiscoveryError::DependencyUnavailable);
        }
        if !accuracy_m.is_finite() || accuracy_m <= 0.0 || accuracy_m > MAX_ACCURACY_M {
            return Err(DiscoveryError::InvalidArgument("accuracy"));
        }
        let now_ms = self.clock.now_ms();
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let record = active_session(&mut sessions, session_id, now_ms)?;
        if !record.joined.contains(actor) {
            return Err(DiscoveryError::Forbidden);
        }
        let mut positions = self
            .positions
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let session_positions = positions.entry(session_id.to_owned()).or_default();
        if let Some(previous) = session_positions.get(actor) {
            if now_ms - previous.updated_at_ms < record.interval_ms {
                return Err(DiscoveryError::RateLimited);
            }
        }
        record.seq += 1;
        let coord = round_coord(coord, record.precision);
        let view = PositionView {
            user_id: actor.to_owned(),
            coord,
            accuracy_m,
            seq: record.seq,
            updated_at_ms: now_ms,
            stale: false,
        };
        session_positions.insert(
            actor.to_owned(),
            PositionRecord {
                coord,
                accuracy_m,
                seq: record.seq,
                updated_at_ms: now_ms,
            },
        );
        Ok(view)
    }

    pub fn latest(&self, reader: &str, session_id: &str) -> Result<Vec<PositionView>> {
        let now_ms = self.clock.now_ms();
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let record = active_session(&mut sessions, session_id, now_ms)?;
        if !record.joined.contains(reader) {
            return Err(DiscoveryError::Forbidden);
        }
        let interval_ms = record.interval_ms;
        let members: BTreeSet<String> = record.joined.clone();
        let positions = self
            .positions
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let Some(session_positions) = positions.get(session_id) else {
            return Ok(Vec::new());
        };
        Ok(session_positions
            .iter()
            .filter(|(user, _)| members.contains(*user))
            .map(|(user, position)| PositionView {
                user_id: user.clone(),
                coord: position.coord,
                accuracy_m: position.accuracy_m,
                seq: position.seq,
                updated_at_ms: position.updated_at_ms,
                stale: now_ms - position.updated_at_ms > interval_ms * STALE_INTERVALS,
            })
            .collect())
    }

    pub fn leave(&self, actor: &str, session_id: &str) -> Result<()> {
        if actor.is_empty() {
            return Err(DiscoveryError::Unauthorized);
        }
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let record = sessions
            .get_mut(session_id)
            .ok_or(DiscoveryError::NotFound)?;
        if actor == record.host {
            return Err(DiscoveryError::InvalidArgument("host must stop session"));
        }
        record.joined.remove(actor);
        drop(sessions);
        self.positions
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .get_mut(session_id)
            .map(|positions| positions.remove(actor));
        Ok(())
    }

    /// 群成员被移除或权限撤销：立即清掉位置并禁止继续读写。
    pub fn remove_member(&self, host: &str, session_id: &str, member: &str) -> Result<()> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let record = sessions
            .get_mut(session_id)
            .ok_or(DiscoveryError::NotFound)?;
        if record.host != host {
            return Err(DiscoveryError::Forbidden);
        }
        record.joined.remove(member);
        record.members.remove(member);
        drop(sessions);
        self.positions
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .get_mut(session_id)
            .map(|positions| positions.remove(member));
        Ok(())
    }

    pub fn stop(&self, host: &str, session_id: &str) -> Result<()> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?;
        let record = sessions
            .get_mut(session_id)
            .ok_or(DiscoveryError::NotFound)?;
        if record.host != host {
            return Err(DiscoveryError::Forbidden);
        }
        record.state = ShareState::Stopped;
        record.joined.clear();
        drop(sessions);
        self.positions
            .lock()
            .map_err(|_| DiscoveryError::DependencyUnavailable)?
            .remove(session_id);
        Ok(())
    }

    pub async fn search_poi<P: PoiPort>(
        &self,
        port: &P,
        query: &str,
        near: Coord,
        limit: u32,
    ) -> Result<Vec<Poi>> {
        if query.is_empty() || query.len() > MAX_NAME_BYTES || limit == 0 || limit > POI_MAX_RESULTS
        {
            return Err(DiscoveryError::InvalidArgument("poi query"));
        }
        port.search(query, near, limit).await
    }

    pub async fn navigate<P: NavigationPort>(
        &self,
        port: &P,
        target: &NavigationTarget,
    ) -> Result<()> {
        port.open(target).await
    }
}

fn active_session<'a>(
    sessions: &'a mut HashMap<String, SessionRecord>,
    session_id: &str,
    now_ms: i64,
) -> Result<&'a mut SessionRecord> {
    let record = sessions
        .get_mut(session_id)
        .ok_or(DiscoveryError::NotFound)?;
    if record.state != ShareState::Active || record.expires_at_ms <= now_ms {
        return Err(DiscoveryError::Expired);
    }
    Ok(record)
}

fn session_view(id: &str, record: &SessionRecord) -> ShareSession {
    ShareSession {
        id: id.to_owned(),
        conversation_id: record.conversation_id.clone(),
        host: record.host.clone(),
        members: record.members.clone(),
        expires_at_ms: record.expires_at_ms,
        interval_ms: record.interval_ms,
        precision: record.precision,
        background_limited: record.background_limited,
        state: record.state,
    }
}

fn round_coord(coord: Coord, precision: SharePrecision) -> Coord {
    match precision {
        SharePrecision::Exact => coord,
        SharePrecision::Approx100m => Coord {
            lat: (coord.lat * APPROX_DECIMALS).round() / APPROX_DECIMALS,
            lng: (coord.lng * APPROX_DECIMALS).round() / APPROX_DECIMALS,
            system: coord.system,
        },
    }
}
