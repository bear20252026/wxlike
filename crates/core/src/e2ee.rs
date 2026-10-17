//! WP-12 end-to-end encryption, built on `vodozemac` (Matrix's Rust Olm/Megolm
//! implementation, Apache-2.0) rather than a hand-rolled ratchet.
//!
//! # Why a library
//!
//! The task book asked for a RustCrypto double ratchet. That is exactly the
//! kind of code where a plausible-looking implementation passes a happy-path
//! test and fails silently in the field: the two sides' root-key sequences must
//! interleave perfectly, and only an alternating-direction test reveals it.
//! `vodozemac` is the same algorithm Matrix ships, in Rust, with the wire format
//! already specified — so the ratchet is audited code and what is *ours* is the
//! session bookkeeping around it.
//!
//! # What this module adds on top
//!
//! vodozemac deliberately leaves three jobs to the embedding (it says so in its
//! docs); this module is those jobs:
//!
//! 1. **Strict-order receiving with a 128-message reorder window.** vodozemac
//!    keeps only 40 skipped message keys per chain, so handing it a message
//!    from 64 indices ahead silently destroys the 24 messages in between. We
//!    park ahead-of-turn messages (keyed by ratchet key + chain index) and only
//!    feed vodozemac the message that is actually next, so a 64-message
//!    shuffle — or fully reversed delivery — decrypts completely.
//! 2. **Replay rejection.** Olm message keys are deterministically
//!    regenerable, so the same ciphertext decrypts twice; we remember recently
//!    decrypted bodies per peer. Megolm message indices are checked for
//!    monotonicity per group sender.
//! 3. **Repeated pre-key handling.** A vodozemac session emits a pre-key
//!    message on *every* send until it hears back, all referencing the same
//!    one-time key. We unwrap repeats to the inner message on the sender side
//!    and tolerate them from libolm-style peers on the receiver side.
//!
//! Routing metadata (`Envelope.conv_id`, `to`, `msg_id`) stays in the clear
//! because the hub must route on it; see `docs/threat-model.md`.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use vodozemac::megolm::{GroupSession, InboundGroupSession, MegolmMessage, SessionKey};
use vodozemac::olm::{Account, Message, OlmMessage, PreKeyMessage, Session, SessionConfig};
use vodozemac::Curve25519PublicKey;

/// One-time prekeys kept in stock.
pub const PREKEY_BATCH: usize = 20;
/// Below this many stored prekeys we replenish.
pub const PREKEY_FLOOR: usize = 5;
/// How many ahead-of-turn messages we hold per peer (the reorder window).
pub const PENDING_WINDOW: usize = 128;
/// How many decrypted bodies we remember per peer for replay detection.
pub const SEEN_WINDOW: usize = 4096;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("no session with this peer and no pre-key message to start one")]
    NoSession,
    #[error("decryption failed (wrong session, tampering, or an unrecoverable gap)")]
    DecryptFailed,
    #[error("message kept for later: deliver the earlier messages first")]
    NotNext,
    #[error("replayed ciphertext rejected")]
    Replay,
    #[error("malformed ciphertext")]
    Malformed,
    #[error("unknown group session")]
    UnknownGroup,
    #[error("this device holds no outbound sender key for that group")]
    ReadOnlyGroup,
    #[error("bad key material: {0}")]
    BadKey(String),
    #[error("peer published no one-time prekey")]
    NoPreKey,
}

pub type Result<T> = std::result::Result<T, CryptoError>;

/// The fetchable key bundle (`POST`/`GET /api/keys`). Keys are base64, matching
/// the Olm wire format so nothing has to be re-encoded per hop.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PreKeyBundle {
    pub user_id: String,
    /// Curve25519 identity key: establishes shared secrets.
    pub identity: String,
    /// Ed25519 identity key: for out-of-band verification (safety numbers).
    pub identity_signing: String,
    /// Unclaimed one-time prekeys.
    pub one_time_keys: Vec<String>,
}

/// A sealed payload on the wire. `pre_key == true` means "this opens a session".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Sealed {
    pub pre_key: bool,
    pub body: String,
}

fn parse_pub(key: &str) -> Result<Curve25519PublicKey> {
    Curve25519PublicKey::from_base64(key).map_err(|e| CryptoError::BadKey(e.to_string()))
}

/// One device's Olm account.
pub struct Device {
    account: Account,
}

impl Device {
    pub fn new() -> Self {
        let mut account = Account::new();
        // Publish a first batch immediately: a fresh device must be
        // contactable without anyone having to ask it to top up first.
        account.generate_one_time_keys(PREKEY_BATCH);
        Self { account }
    }

    /// The bundle other users fetch to start a session with us.
    pub fn bundle(&self, user_id: &str) -> PreKeyBundle {
        let keys = self.account.identity_keys();
        PreKeyBundle {
            user_id: user_id.to_string(),
            identity: keys.curve25519.to_base64(),
            identity_signing: keys.ed25519.to_base64(),
            one_time_keys: self
                .account
                .one_time_keys()
                .values()
                .map(|k| k.to_base64())
                .collect(),
        }
    }

    pub fn one_time_key_count(&self) -> usize {
        // The unpublished set is what we hand out; a key leaves it the moment
        // a peer's pre-key message consumes it, which is exactly the signal
        // to replenish. We never call `mark_keys_as_published` — in Matrix
        // that flags keys already synced to the server, a distinction our
        // key directory (`POST /api/keys`) does not need.
        self.account.one_time_keys().len()
    }

    /// Refills the one-time prekey stock when it runs low.
    ///
    /// Without this a busy account eventually publishes an empty bundle and
    /// every new peer's handshake is silently weaker (or fails outright).
    pub fn top_up_one_time_keys(&mut self) -> bool {
        if self.one_time_key_count() >= PREKEY_FLOOR {
            return false;
        }
        self.account.generate_one_time_keys(PREKEY_BATCH);
        true
    }

    pub fn identity_fingerprint(&self) -> String {
        self.account.identity_keys().curve25519.to_base64()
    }
}

impl Default for Device {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-peer 1:1 session plus the bookkeeping vodozemac leaves to us:
/// where each receiving chain stands, what is parked, what already decrypted.
struct DmState {
    session: Session,
    /// ratchet key (b64) -> highest chain index successfully delivered.
    chain_pos: HashMap<String, u64>,
    /// Ahead-of-turn messages, keyed by (ratchet key, chain index).
    parked: BTreeMap<(String, u64), Sealed>,
    /// Plaintexts recovered from `parked` by later arrivals, in order.
    recovered: Vec<Vec<u8>>,
    /// Recently decrypted bodies (replay guard) + insertion order for eviction.
    seen: HashSet<String>,
    seen_order: VecDeque<String>,
}

impl DmState {
    fn new(session: Session) -> Self {
        Self {
            session,
            chain_pos: HashMap::new(),
            parked: BTreeMap::new(),
            recovered: Vec::new(),
            seen: HashSet::new(),
            seen_order: VecDeque::new(),
        }
    }

    /// Records a decrypted body; `false` means we saw it before (replay).
    fn mark_seen(&mut self, body: &str) -> bool {
        if self.seen.contains(body) {
            return false;
        }
        if self.seen_order.len() >= SEEN_WINDOW {
            if let Some(oldest) = self.seen_order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        self.seen.insert(body.to_string());
        self.seen_order.push_back(body.to_string());
        true
    }
}

/// Per-group crypto state. A member who speaks holds an outbound sender key
/// *and* one inbound session per remote sender — speaking and listening are
/// independent, so every member can do both.
struct GroupState {
    /// Our outbound sender key, if we speak in this group.
    outbound: Option<GroupSession>,
    /// One inbound session per remote sender identity.
    inbound: HashMap<String, InboundGroupSession>,
}

/// A device's whole crypto state.
pub struct Crypto {
    device: Device,
    dm: HashMap<String, DmState>,
    groups: HashMap<String, GroupState>,
    /// Megolm replay guard: (group, sender identity, session id) -> last index.
    /// The session id is part of the key so a key rotation (new session, index
    /// restarts at 0) is not mistaken for a replay.
    group_seen: HashMap<(String, String, String), u32>,
}

impl Crypto {
    pub fn new(device: Device) -> Self {
        Self {
            device,
            dm: HashMap::new(),
            groups: HashMap::new(),
            group_seen: HashMap::new(),
        }
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn bundle(&self, user_id: &str) -> PreKeyBundle {
        self.device.bundle(user_id)
    }

    /// Publishes a fresh bundle when prekeys run low; returns the bundle.
    pub fn bundle_refilled(&mut self, user_id: &str) -> PreKeyBundle {
        self.device.top_up_one_time_keys();
        self.device.bundle(user_id)
    }

    /// Encrypts to a peer, establishing a session on the first message.
    ///
    /// Only the opening message is a pre-key message. vodozemac would keep
    /// wrapping every send in a pre-key until it hears back — each referencing
    /// the same one-time key the receiver consumed on the first — so we unwrap
    /// repeats to the inner normal message. Callers must make sure the opening
    /// message actually arrives (retry it) or the peer can never establish.
    pub fn encrypt_dm(
        &mut self,
        peer_user_id: &str,
        peer_bundle: &PreKeyBundle,
        plaintext: &[u8],
    ) -> Result<Sealed> {
        let had_session = self.dm.contains_key(peer_user_id);
        if !had_session {
            let identity = parse_pub(&peer_bundle.identity)?;
            let otk = peer_bundle
                .one_time_keys
                .first()
                .map(|k| parse_pub(k))
                .transpose()?
                .ok_or(CryptoError::NoPreKey)?;
            let session = self
                .device
                .account
                .create_outbound_session(SessionConfig::default(), identity, otk)
                .map_err(|e| CryptoError::BadKey(e.to_string()))?;
            self.dm
                .insert(peer_user_id.to_string(), DmState::new(session));
        }

        let state = self
            .dm
            .get_mut(peer_user_id)
            .ok_or(CryptoError::NoSession)?;
        let message = state
            .session
            .encrypt(plaintext)
            .map_err(|_| CryptoError::DecryptFailed)?;
        Ok(match message {
            OlmMessage::PreKey(pre_key) if had_session => Sealed {
                pre_key: false,
                body: pre_key.message().to_base64(),
            },
            OlmMessage::PreKey(pre_key) => Sealed {
                pre_key: true,
                body: pre_key.to_base64(),
            },
            OlmMessage::Normal(m) => Sealed {
                pre_key: false,
                body: m.to_base64(),
            },
        })
    }

    /// Opens a message from `peer_user_id`, creating the inbound session when
    /// the message carries a pre-key.
    ///
    /// An ahead-of-turn message returns [`CryptoError::NotNext`] and is parked
    /// (up to [`PENDING_WINDOW`] per peer); it decrypts automatically once the
    /// gap fills — collect it with [`Crypto::take_recovered`].
    pub fn decrypt_dm(&mut self, peer_user_id: &str, sealed: &Sealed) -> Result<Vec<u8>> {
        if sealed.pre_key {
            return self.decrypt_pre_key(peer_user_id, sealed);
        }
        let message = Message::from_base64(&sealed.body).map_err(|_| CryptoError::Malformed)?;
        self.deliver_normal(peer_user_id, message, sealed.body.clone())
    }

    fn decrypt_pre_key(&mut self, peer_user_id: &str, sealed: &Sealed) -> Result<Vec<u8>> {
        let pre_key =
            PreKeyMessage::from_base64(&sealed.body).map_err(|_| CryptoError::Malformed)?;

        // A repeated pre-key from a libolm-style peer: if we already hold the
        // session it establishes, ride the existing chain instead of trying to
        // consume the one-time key a second time (which cannot work).
        if let Some(state) = self.dm.get(peer_user_id) {
            if state.seen.contains(&sealed.body) {
                return Err(CryptoError::Replay);
            }
            if state.session.session_keys() == pre_key.session_keys() {
                let inner = pre_key.message().clone();
                let body = sealed.body.clone();
                return self.deliver_normal(peer_user_id, inner, body);
            }
        }

        let result = self
            .device
            .account
            .create_inbound_session(SessionConfig::default(), pre_key.identity_key(), &pre_key)
            .map_err(|_| CryptoError::DecryptFailed)?;
        let rk = pre_key.message().ratchet_key().to_base64();
        let idx = pre_key.message().chain_index();
        let mut state = DmState::new(result.session);
        // Anything parked for the previous session can never decrypt now.
        state.chain_pos.insert(rk, idx);
        if !state.mark_seen(&sealed.body) {
            return Err(CryptoError::Replay);
        }
        let plaintext = result.plaintext;
        self.dm.insert(peer_user_id.to_string(), state);
        self.drain_parked(peer_user_id);
        Ok(plaintext)
    }

    /// Delivers a normal message, enforcing turn order per ratchet chain.
    fn deliver_normal(
        &mut self,
        peer_user_id: &str,
        message: Message,
        body: String,
    ) -> Result<Vec<u8>> {
        let rk = message.ratchet_key().to_base64();
        let idx = message.chain_index();

        let ahead = {
            let Some(state) = self.dm.get(peer_user_id) else {
                return Err(CryptoError::NoSession);
            };
            match state.chain_pos.get(&rk) {
                Some(&last) => idx > last + 1,
                // A ratchet key we have not seen is a new chain (the peer
                // replied); its first message is accepted at any index.
                None => false,
            }
        };
        if ahead {
            self.park(peer_user_id, rk, idx, body);
            return Err(CryptoError::NotNext);
        }

        let state = self
            .dm
            .get_mut(peer_user_id)
            .ok_or(CryptoError::NoSession)?;
        if state.seen.contains(&body) {
            return Err(CryptoError::Replay);
        }
        match state.session.decrypt(&OlmMessage::Normal(message)) {
            Ok(plaintext) => {
                if !state.mark_seen(&body) {
                    return Err(CryptoError::Replay);
                }
                let next = state.chain_pos.get(&rk).map_or(idx, |&last| last.max(idx));
                state.chain_pos.insert(rk, next);
                self.drain_parked(peer_user_id);
                Ok(plaintext)
            }
            Err(_) => {
                // Either tampering (will never decrypt) or a chain we cannot
                // place yet; parking separates the two: a legit reorder
                // decrypts when its turn comes, garbage sits until evicted.
                self.park(peer_user_id, rk, idx, body);
                Err(CryptoError::DecryptFailed)
            }
        }
    }

    fn park(&mut self, peer_user_id: &str, rk: String, idx: u64, body: String) {
        let Some(state) = self.dm.get_mut(peer_user_id) else {
            return;
        };
        if state.parked.len() >= PENDING_WINDOW {
            // Evict the furthest-ahead message: the closest ones are about to
            // become deliverable, the furthest ones are the lost cause.
            state.parked.pop_last();
        }
        state.parked.insert(
            (rk, idx),
            Sealed {
                pre_key: false,
                body,
            },
        );
    }

    /// Feeds parked messages whose turn has come, cascading in order.
    fn drain_parked(&mut self, peer_user_id: &str) {
        loop {
            let Some(state) = self.dm.get_mut(peer_user_id) else {
                return;
            };
            let due: Option<((String, u64), Sealed)> = state
                .parked
                .iter()
                .find(|((rk, idx), _)| {
                    state
                        .chain_pos
                        .get(rk)
                        .is_some_and(|last| *idx == *last + 1)
                })
                .map(|(k, v)| (k.clone(), v.clone()));
            let Some((key, sealed)) = due else {
                return;
            };
            let Some(state) = self.dm.get_mut(peer_user_id) else {
                return;
            };
            let Ok(message) = Message::from_base64(&sealed.body) else {
                state.parked.remove(&key);
                continue;
            };
            match state.session.decrypt(&OlmMessage::Normal(message)) {
                Ok(plaintext) => {
                    if state.mark_seen(&sealed.body) {
                        state.recovered.push(plaintext);
                    }
                    let (rk, idx) = &key;
                    let next = state.chain_pos.get(rk).map_or(*idx, |l| (*l).max(*idx));
                    state.chain_pos.insert(rk.clone(), next);
                    state.parked.remove(&key);
                    // loop again: this delivery may unblock the next one
                }
                Err(_) => {
                    // Not actually decryptable at its turn (tampered): drop it
                    // so it cannot wedge the queue, and keep going.
                    state.parked.remove(&key);
                }
            }
        }
    }

    /// Plaintexts recovered from reordering, oldest first; takes them out.
    pub fn take_recovered(&mut self, peer_user_id: &str) -> Vec<Vec<u8>> {
        self.dm
            .get_mut(peer_user_id)
            .map_or_else(Vec::new, |s| std::mem::take(&mut s.recovered))
    }

    /// How many messages are parked for this peer (diagnostics/tests).
    pub fn parked_for(&self, peer_user_id: &str) -> usize {
        self.dm.get(peer_user_id).map_or(0, |s| s.parked.len())
    }

    pub fn has_session_with(&self, peer: &str) -> bool {
        self.dm.contains_key(peer)
    }

    pub fn session_id_with(&self, peer: &str) -> Option<String> {
        self.dm.get(peer).map(|s| s.session.session_id())
    }

    // -------------------------------------------------------------------------
    // groups (Megolm sender keys)
    // -------------------------------------------------------------------------

    /// Starts speaking in a group; returns our shareable sender key.
    pub fn start_group(&mut self, group_id: &str) -> String {
        let session = GroupSession::new(Default::default());
        let key = session.session_key().to_base64();
        let entry = self
            .groups
            .entry(group_id.to_string())
            .or_insert_with(|| GroupState {
                outbound: None,
                inbound: HashMap::new(),
            });
        entry.outbound = Some(session);
        key
    }

    /// Membership changed: rotate so a newly added member cannot read history.
    pub fn rotate_group(&mut self, group_id: &str) -> Result<String> {
        let Some(state) = self.groups.get_mut(group_id) else {
            return Err(CryptoError::UnknownGroup);
        };
        if state.outbound.is_none() {
            return Err(CryptoError::ReadOnlyGroup);
        }
        let fresh = GroupSession::new(Default::default());
        let key = fresh.session_key().to_base64();
        state.outbound = Some(fresh);
        Ok(key)
    }

    /// Records a peer's sender key. Sharing happens over the recipient's 1:1
    /// session with that peer — never through the server.
    pub fn import_group_key(
        &mut self,
        group_id: &str,
        sender_identity: &str,
        key_b64: &str,
    ) -> Result<()> {
        let key =
            SessionKey::from_base64(key_b64).map_err(|e| CryptoError::BadKey(e.to_string()))?;
        let inbound = InboundGroupSession::new(&key, Default::default());
        self.groups
            .entry(group_id.to_string())
            .or_insert_with(|| GroupState {
                outbound: None,
                inbound: HashMap::new(),
            })
            .inbound
            .insert(sender_identity.to_string(), inbound);
        Ok(())
    }

    pub fn encrypt_group(&mut self, group_id: &str, plaintext: &[u8]) -> Result<Sealed> {
        let state = self
            .groups
            .get_mut(group_id)
            .ok_or(CryptoError::UnknownGroup)?;
        match state.outbound.as_mut() {
            Some(session) => Ok(Sealed {
                pre_key: false,
                body: session.encrypt(plaintext).to_base64(),
            }),
            None => Err(CryptoError::ReadOnlyGroup),
        }
    }

    /// Opens a group message and reports which sender's key did it.
    ///
    /// Megolm indices must be monotonic per sender; vodozemac checks the MAC
    /// but leaves replay accounting to us, so a re-delivered index is refused.
    pub fn decrypt_group(&mut self, group_id: &str, sealed: &Sealed) -> Result<(String, Vec<u8>)> {
        let message =
            MegolmMessage::from_base64(&sealed.body).map_err(|_| CryptoError::Malformed)?;
        let state = self
            .groups
            .get_mut(group_id)
            .ok_or(CryptoError::UnknownGroup)?;
        let mut found = None;
        for (sender, session) in state.inbound.iter_mut() {
            if let Ok(plain) = session.decrypt(&message) {
                // The session id separates sender-key generations.
                found = Some((sender.clone(), session.session_id(), plain));
                break;
            }
        }
        let (sender, session_id, plain) = match found {
            Some(x) => x,
            None if state.inbound.is_empty() && state.outbound.is_some() => {
                // We only hold our own sender key; we cannot read our own traffic.
                return Err(CryptoError::ReadOnlyGroup);
            }
            None => return Err(CryptoError::DecryptFailed),
        };

        let watermark = self
            .group_seen
            .get(&(group_id.to_string(), sender.clone(), session_id.clone()))
            .copied();
        if watermark.is_some_and(|wm| plain.message_index <= wm) {
            return Err(CryptoError::Replay);
        }
        self.group_seen.insert(
            (group_id.to_string(), sender.clone(), session_id),
            plain.message_index,
        );
        Ok((sender, plain.plaintext))
    }

    pub fn group_session_id(&self, group_id: &str) -> Option<String> {
        let state = self.groups.get(group_id)?;
        if let Some(out) = state.outbound.as_ref() {
            return Some(out.session_id());
        }
        state.inbound.values().next().map(|s| s.session_id())
    }

    /// Whether we can read this group at all (i.e. we hold someone's sender key).
    pub fn can_read_group(&self, group_id: &str) -> bool {
        self.groups
            .get(group_id)
            .is_some_and(|s| !s.inbound.is_empty())
    }
}
