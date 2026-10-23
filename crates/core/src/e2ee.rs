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
use vodozemac::megolm::{
    GroupSession, GroupSessionPickle, InboundGroupSession, InboundGroupSessionPickle,
    MegolmMessage, SessionKey,
};
use vodozemac::olm::{
    Account, AccountPickle, Message, OlmMessage, PreKeyMessage, Session, SessionConfig,
    SessionPickle,
};
use vodozemac::Curve25519PublicKey;

/// One-time prekeys kept in stock.
pub const PREKEY_BATCH: usize = 20;
/// Below this many stored prekeys we replenish.
pub const PREKEY_FLOOR: usize = 5;
/// How many ahead-of-turn messages we hold per peer (the reorder window).
pub const PENDING_WINDOW: usize = 128;
/// How many decrypted bodies we remember per peer for replay detection.
pub const SEEN_WINDOW: usize = 4096;
/// M-6（第四轮审计）：群水位表 (group, sender, session) 键数上限。超过即
/// FIFO 遗忘最久未更新的键——该会话的重放窗口随之重开，这是把内存从
/// 单调增长收敛回来的代价；被驱逐的是最不活跃的会话，最近对话不受影响。
pub const GROUP_SEEN_CAP: usize = 4096;

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
    #[error("pre-key bundle failed signature verification: server-supplied keys are not trusted")]
    UnverifiedBundle,
    #[error("peer published no one-time prekey")]
    NoPreKey,
    #[error("stored crypto state is unreadable: {0}")]
    Snapshot(String),
    /// 口令错：KDF 成功但校验标签对不上。与"文件损坏"分开，UI 要分别提示。
    #[error("wrong passphrase")]
    WrongPassphrase,
    /// 文件损坏/被改/不是本格式。旧 WXSC1 也走这里，消息里写明原因。
    #[error("snapshot file is corrupted or not a wxlike snapshot: {0}")]
    CorruptedSnapshot(String),
}

pub type Result<T> = std::result::Result<T, CryptoError>;

/// 本地静态加密的魔数（版本 + 用途）。换算法/换格式就换魔数，旧文件直接
/// 报"读不懂"，不会半加载。
const AT_REST_MAGIC: &[u8; 5] = b"WXSC1";
const AT_REST_NONCE: usize = 24;
/// 静态加密的设备密钥长度（XChaCha20-Poly1305 的 256 位密钥）。
pub const DEVICE_KEY_BYTES: usize = 32;

/// 快照落盘前的静态加密（XChaCha20-Poly1305）。
///
/// vodozemac 的 pickle 是**明文**结构体，磁盘上直接放等于把身份私钥写在
/// 文件里。调用方（桌面端）用一把 32 字节设备密钥密封它，密钥单独存、
/// 权限 0600。
pub fn seal_at_rest(key: &[u8; DEVICE_KEY_BYTES], plaintext: &[u8]) -> Result<Vec<u8>> {
    use chacha20poly1305::aead::{Aead as _, KeyInit as _};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};

    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce = [0u8; AT_REST_NONCE];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
    let body = cipher
        .encrypt(XNonce::from_slice(&nonce), plaintext)
        .map_err(|_| CryptoError::Snapshot("seal failed".into()))?;
    let mut out = Vec::with_capacity(AT_REST_MAGIC.len() + AT_REST_NONCE + body.len());
    out.extend_from_slice(AT_REST_MAGIC);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&body);
    Ok(out)
}

/// [`seal_at_rest`] 的逆操作。密钥不对、文件被改、版本不认识都报错。
pub fn open_at_rest(key: &[u8; DEVICE_KEY_BYTES], blob: &[u8]) -> Result<Vec<u8>> {
    use chacha20poly1305::aead::{Aead as _, KeyInit as _};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};

    if blob.len() < AT_REST_MAGIC.len() + AT_REST_NONCE
        || &blob[..AT_REST_MAGIC.len()] != AT_REST_MAGIC
    {
        return Err(CryptoError::Snapshot(
            "not a wxlike secret-chat store (bad header)".into(),
        ));
    }
    let nonce = &blob[AT_REST_MAGIC.len()..AT_REST_MAGIC.len() + AT_REST_NONCE];
    let body = &blob[AT_REST_MAGIC.len() + AT_REST_NONCE..];
    XChaCha20Poly1305::new(key.into())
        .decrypt(XNonce::from_slice(nonce), body)
        .map_err(|_| CryptoError::Snapshot("wrong device key or corrupted store".into()))
}

// ---------------------------------------------------------------------------
// T-A2 (#48): passphrase-derived snapshots (WXSC2)
// ---------------------------------------------------------------------------

/// 口令派生快照的魔数。WXSC1 是旧的"裸设备密钥"格式，明确拒绝、不静默迁移。
pub const SNAPSHOT_MAGIC_V2: &[u8; 5] = b"WXSC2";
/// 外层格式版本（WXSC2 的布局版本）。
const SNAPSHOT_FORMAT_VERSION: u8 = 1;

/// Argon2id 参数（OWASP 推荐：19 MiB / t=2 / p=1）。
///
/// **参数随文件头存储**，不依赖 crate 默认值：argon2 未来若改默认参数，
/// 旧快照仍要按写入时的参数解开。
const SNAPSHOT_ARGON2_M_COST: u32 = 19 * 1024;
const SNAPSHOT_ARGON2_T_COST: u32 = 2;
const SNAPSHOT_ARGON2_P_COST: u32 = 1;
/// 打开时接受的上限：恶意文件不能让我们按它指定的参数吃掉几 GB 内存。
const SNAPSHOT_ARGON2_MAX_M_COST: u32 = 256 * 1024;
const SNAPSHOT_ARGON2_MAX_T_COST: u32 = 16;
const SNAPSHOT_ARGON2_MAX_P_COST: u32 = 8;

const SNAPSHOT_KEY_LEN: usize = 32;
const SNAPSHOT_CHECK_LEN: usize = 16;
const SNAPSHOT_NONCE_LEN: usize = 24;
const SNAPSHOT_SALT_MAX: usize = 64;
/// magic(5) + version(1) + m/t/p(12) + salt_len(1)
const SNAPSHOT_FIXED_HEADER: usize = 19;

/// 口令校验标签：从派生密钥再散列一步。它**不含密钥本身**，只用来把
/// "口令错"与"密文被改"分成两个错误——AEAD 自己无法区分这两者。
fn snapshot_check(key: &[u8; SNAPSHOT_KEY_LEN]) -> [u8; SNAPSHOT_CHECK_LEN] {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    h.update(b"wxlike-snapshot-check-v1");
    h.update(key);
    let digest = h.finalize();
    let mut out = [0u8; SNAPSHOT_CHECK_LEN];
    out.copy_from_slice(&digest[..SNAPSHOT_CHECK_LEN]);
    out
}

/// 定长比较，避免把校验标签的匹配位置泄漏给本地计时观察。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

fn derive_snapshot_key(
    passphrase: &str,
    m: u32,
    t: u32,
    p: u32,
    salt: &[u8],
) -> Result<[u8; SNAPSHOT_KEY_LEN]> {
    let params = argon2::Params::new(m, t, p, Some(SNAPSHOT_KEY_LEN))
        .map_err(|e| CryptoError::CorruptedSnapshot(format!("bad KDF parameters: {e}")))?;
    let kdf = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; SNAPSHOT_KEY_LEN];
    kdf.hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| CryptoError::Snapshot(format!("key derivation failed: {e}")))?;
    Ok(key)
}

fn seal_snapshot(passphrase: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
    use argon2::password_hash::SaltString;
    use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};

    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    let salt = salt.as_str().as_bytes();
    let key = derive_snapshot_key(
        passphrase,
        SNAPSHOT_ARGON2_M_COST,
        SNAPSHOT_ARGON2_T_COST,
        SNAPSHOT_ARGON2_P_COST,
        salt,
    )?;

    let mut header = Vec::with_capacity(SNAPSHOT_FIXED_HEADER + salt.len() + SNAPSHOT_CHECK_LEN);
    header.extend_from_slice(SNAPSHOT_MAGIC_V2);
    header.push(SNAPSHOT_FORMAT_VERSION);
    header.extend_from_slice(&SNAPSHOT_ARGON2_M_COST.to_be_bytes());
    header.extend_from_slice(&SNAPSHOT_ARGON2_T_COST.to_be_bytes());
    header.extend_from_slice(&SNAPSHOT_ARGON2_P_COST.to_be_bytes());
    header.push(salt.len() as u8);
    header.extend_from_slice(salt);
    header.extend_from_slice(&snapshot_check(&key));

    let mut nonce = [0u8; SNAPSHOT_NONCE_LEN];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
    let body = XChaCha20Poly1305::new((&key).into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &header,
            },
        )
        .map_err(|_| CryptoError::Snapshot("seal failed".into()))?;

    let mut out = header;
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&body);
    Ok(out)
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// WXSC2 头部解析产物：KDF 参数与各段边界。
struct SnapshotHeader<'a> {
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
    salt: &'a [u8],
    stored_check: &'a [u8],
    /// 密文体起点（同时是 AAD 的终点）。
    body_start: usize,
}

/// 头部解析与校验（magic/版本/KDF 范围/盐长/截断）。与解密拆开，各自
/// 保持在质量闸门的函数行数预算内。
fn parse_snapshot_header(blob: &[u8]) -> Result<SnapshotHeader<'_>> {
    if blob.len() < SNAPSHOT_FIXED_HEADER || &blob[..5] != SNAPSHOT_MAGIC_V2 {
        if blob.len() >= AT_REST_MAGIC.len() && &blob[..AT_REST_MAGIC.len()] == AT_REST_MAGIC {
            return Err(CryptoError::CorruptedSnapshot(
                "legacy WXSC1 snapshot (raw device key) is no longer accepted;                  delete it and re-enable secret chat"
                    .into(),
            ));
        }
        return Err(CryptoError::CorruptedSnapshot(
            "not a WXSC2 snapshot (bad magic or truncated header)".into(),
        ));
    }
    if blob[5] != SNAPSHOT_FORMAT_VERSION {
        return Err(CryptoError::CorruptedSnapshot(format!(
            "unsupported WXSC2 format version {}",
            blob[5]
        )));
    }
    let m = be_u32(&blob[6..10]);
    let t = be_u32(&blob[10..14]);
    let p = be_u32(&blob[14..18]);
    if !(1..=SNAPSHOT_ARGON2_MAX_M_COST).contains(&m)
        || !(1..=SNAPSHOT_ARGON2_MAX_T_COST).contains(&t)
        || !(1..=SNAPSHOT_ARGON2_MAX_P_COST).contains(&p)
    {
        return Err(CryptoError::CorruptedSnapshot(
            "KDF parameters out of range".into(),
        ));
    }
    let salt_len = blob[18] as usize;
    if salt_len == 0 || salt_len > SNAPSHOT_SALT_MAX {
        return Err(CryptoError::CorruptedSnapshot("bad salt length".into()));
    }
    let salt_end = SNAPSHOT_FIXED_HEADER + salt_len;
    let check_end = salt_end + SNAPSHOT_CHECK_LEN;
    let nonce_end = check_end + SNAPSHOT_NONCE_LEN;
    if blob.len() < nonce_end {
        return Err(CryptoError::CorruptedSnapshot("truncated header".into()));
    }
    Ok(SnapshotHeader {
        m_cost: m,
        t_cost: t,
        p_cost: p,
        salt: &blob[SNAPSHOT_FIXED_HEADER..salt_end],
        stored_check: &blob[salt_end..check_end],
        body_start: nonce_end,
    })
}

fn open_snapshot(passphrase: &str, blob: &[u8]) -> Result<Vec<u8>> {
    use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};

    let h = parse_snapshot_header(blob)?;
    let key = derive_snapshot_key(passphrase, h.m_cost, h.t_cost, h.p_cost, h.salt)?;
    if !ct_eq(h.stored_check, &snapshot_check(&key)) {
        return Err(CryptoError::WrongPassphrase);
    }
    // body_start 是密文体起点；nonce 恰在它之前的 NONCE_LEN 字节，
    // AAD 是再之前的整个头部（到 stored_check 为止）。
    let aad_end = h.body_start - SNAPSHOT_NONCE_LEN;
    let nonce = &blob[aad_end..h.body_start];
    let body = &blob[h.body_start..];
    XChaCha20Poly1305::new((&key).into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: body,
                aad: &blob[..aad_end],
            },
        )
        .map_err(|_| CryptoError::CorruptedSnapshot("ciphertext failed authentication".into()))
}

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
    #[serde(default)]
    pub one_time_keys: Vec<String>,
    /// Signature (base64) for each entry of `one_time_keys`, same order.
    /// A claim pops one (key, signature) pair, so the subset a sender
    /// receives stays verifiable on its own.
    #[serde(default)]
    pub one_time_key_signatures: Vec<String>,
    /// Ed25519 signature (base64) by the owner's `identity_signing` key over
    /// `(user_id, identity)` — the binding that stops a zero-trust directory
    /// from swapping someone's identity key. A bundle without valid
    /// signatures is **rejected** by [`Crypto::encrypt_dm`]: "the server
    /// returned this key" is not evidence the key is the owner's.
    ///
    /// `#[serde(default)]` keeps old JSON parseable at the transport layer;
    /// verification still fails closed on the empty value.
    #[serde(default)]
    pub signature: String,
}

/// Length-prefixed field framing for signing inputs: no delimiter that could
/// appear inside a field, so no crafted (user_id, identity) pair can be
/// rearranged into another bundle's signing input.
fn push_field(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u64).to_be_bytes());
    buf.extend_from_slice(s.as_bytes());
}

/// Domain-separated signing input for a single one-time key.
fn one_time_key_input(key: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(24 + key.len());
    buf.extend_from_slice(b"wxlike-otk-v1");
    push_field(&mut buf, key);
    buf
}

impl PreKeyBundle {
    /// Canonical byte string covered by `signature`: user id + Curve25519
    /// identity key. The OTKs are covered individually (see
    /// [`PreKeyBundle::one_time_key_signatures`]) because a claim hands a
    /// sender only a subset of them.
    pub fn signing_input(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(96);
        buf.extend_from_slice(b"wxlike-prekey-bundle-v1");
        push_field(&mut buf, &self.user_id);
        push_field(&mut buf, &self.identity);
        buf
    }

    /// Fail-closed signature check: the identity binding must verify, and
    /// every one-time key must carry a valid signature by the same Ed25519
    /// key (this is what stops the directory from splicing a fresh-looking
    /// OTK onto someone else's identity). Empty/garbage signature, wrong key,
    /// length mismatch, or any tampered field ⇒ [`CryptoError::UnverifiedBundle`].
    pub fn verify_signature(&self) -> Result<()> {
        let signing = vodozemac::Ed25519PublicKey::from_base64(&self.identity_signing)
            .map_err(|e| CryptoError::BadKey(format!("identity_signing malformed: {e}")))?;
        let signature = vodozemac::Ed25519Signature::from_base64(&self.signature)
            .map_err(|_| CryptoError::UnverifiedBundle)?;
        signing
            .verify(&self.signing_input(), &signature)
            .map_err(|_| CryptoError::UnverifiedBundle)?;
        if self.one_time_keys.len() != self.one_time_key_signatures.len() {
            return Err(CryptoError::UnverifiedBundle);
        }
        for (key, sig) in self.one_time_keys.iter().zip(&self.one_time_key_signatures) {
            let sig = vodozemac::Ed25519Signature::from_base64(sig)
                .map_err(|_| CryptoError::UnverifiedBundle)?;
            signing
                .verify(&one_time_key_input(key), &sig)
                .map_err(|_| CryptoError::UnverifiedBundle)?;
        }
        Ok(())
    }

    /// Structural check: every Curve25519/Ed25519 field must decode to a real
    /// curve point. Keeps garbage out of the server-side directory so a
    /// poisoned bundle cannot make a user unreachable ("publishes keys nobody
    /// can start a session with").
    pub fn validate_shape(&self) -> Result<()> {
        parse_pub(&self.identity).map_err(|e| CryptoError::BadKey(format!("identity: {e}")))?;
        vodozemac::Ed25519PublicKey::from_base64(&self.identity_signing)
            .map_err(|e| CryptoError::BadKey(format!("identity_signing: {e}")))?;
        for (i, key) in self.one_time_keys.iter().enumerate() {
            parse_pub(key).map_err(|e| CryptoError::BadKey(format!("one_time_keys[{i}]: {e}")))?;
        }
        Ok(())
    }
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
        self.bundle_limited(user_id, usize::MAX)
    }

    /// Same as [`Device::bundle`], publishing at most `max_otks` one-time keys.
    /// Exists so callers (and the drained-directory tests) can publish a thin
    /// bundle; signatures cover whichever keys are actually published.
    pub fn bundle_limited(&self, user_id: &str, max_otks: usize) -> PreKeyBundle {
        let keys = self.account.identity_keys();
        let otks: Vec<String> = self
            .account
            .one_time_keys()
            .values()
            .take(max_otks)
            .map(|k| k.to_base64())
            .collect();
        // Each OTK is signed on its own: a claim pops one (key, signature)
        // pair, and the subset must still verify standalone.
        let otk_sigs = otks
            .iter()
            .map(|k| self.account.sign(one_time_key_input(k)).to_base64())
            .collect();
        let mut bundle = PreKeyBundle {
            user_id: user_id.to_string(),
            identity: keys.curve25519.to_base64(),
            identity_signing: keys.ed25519.to_base64(),
            one_time_keys: otks,
            one_time_key_signatures: otk_sigs,
            signature: String::new(),
        };
        // The Ed25519 half of the account signs the identity binding.
        // `Account::sign` is Matrix's own device-key signing operation.
        bundle.signature = self.account.sign(bundle.signing_input()).to_base64();
        bundle
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

    /// Human-comparable safety number over the Ed25519 signing key: the value
    /// two users compare out-of-band to confirm no MITM sits between them
    /// (the TOFU first-connect window in `docs/threat-model.md`). Grouped
    /// hex of SHA-256 over the public key, so a transcription error breaks the
    /// check instead of silently matching.
    pub fn safety_number(&self) -> String {
        use sha2::{Digest as _, Sha256};
        let keys = self.account.identity_keys();
        let digest = hex::encode(Sha256::digest(keys.ed25519.as_bytes()));
        digest
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap_or_default().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Rebuilds a device from a stored account pickle (restart / new process).
    pub fn from_account(account: Account) -> Self {
        Self { account }
    }

    /// 用口令派生密钥密封本设备的 Olm 账户（WXSC2）。
    ///
    /// 口令不落盘：盐与 Argon2id 参数随密文头存储，32 字节密钥只在内存里
    /// 从口令现算。旧 `seal_at_rest`（裸设备密钥）留给桌面端迁移期使用。
    pub fn seal_with_passphrase(&self, passphrase: &str) -> Result<Vec<u8>> {
        let plain = serde_json::to_vec(&self.account.pickle())
            .map_err(|e| CryptoError::Snapshot(e.to_string()))?;
        seal_snapshot(passphrase, &plain)
    }

    /// [`Device::seal_with_passphrase`] 的逆操作。
    ///
    /// 口令错 → [`CryptoError::WrongPassphrase`]；文件被改/截断/旧 WXSC1
    /// → [`CryptoError::CorruptedSnapshot`]（消息里注明具体原因）。
    pub fn open_with_passphrase(passphrase: &str, blob: &[u8]) -> Result<Self> {
        let plain = open_snapshot(passphrase, blob)?;
        let pickle: AccountPickle = serde_json::from_slice(&plain)
            .map_err(|e| CryptoError::CorruptedSnapshot(e.to_string()))?;
        Ok(Self::from_account(Account::from_pickle(pickle)))
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

/// On-disk format version. Bump when the shape changes: an old snapshot is
/// then refused with a clear error instead of half-loading.
const SNAPSHOT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    account: AccountPickle,
    dm: BTreeMap<String, DmSnapshot>,
    groups: BTreeMap<String, GroupSnapshot>,
}

#[derive(Serialize, Deserialize)]
struct DmSnapshot {
    session: SessionPickle,
    chain_pos: HashMap<String, u64>,
}

#[derive(Serialize, Deserialize)]
struct GroupSnapshot {
    outbound: Option<GroupSessionPickle>,
    inbound: HashMap<String, InboundGroupSessionPickle>,
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
    /// Insertion order for [`GROUP_SEEN_CAP`] eviction (not snapshotted, like
    /// the watermarks themselves).
    group_seen_order: VecDeque<(String, String, String)>,
}

impl Crypto {
    pub fn new(device: Device) -> Self {
        Self {
            device,
            dm: HashMap::new(),
            groups: HashMap::new(),
            group_seen: HashMap::new(),
            group_seen_order: VecDeque::new(),
        }
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn bundle(&self, user_id: &str) -> PreKeyBundle {
        self.device.bundle(user_id)
    }

    /// See [`Device::bundle_limited`].
    pub fn bundle_limited(&self, user_id: &str, max_otks: usize) -> PreKeyBundle {
        self.device.bundle_limited(user_id, max_otks)
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
            // Zero-trust key directory (docs/threat-model.md): the bundle came
            // from the server, which may have swapped the identity key. Only
            // a signature by the owner's Ed25519 key makes it usable. This
            // fails closed — an unsigned or tampered bundle never starts a
            // session, mirroring `encrypt_dm_existing`'s no-plaintext rule.
            peer_bundle.verify_signature()?;
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

    /// 用**已有会话**加密；没有会话就是错误（`NoSession`）。
    ///
    /// 与 [`Crypto::encrypt_dm`] 的区别只有一点，但很关键：**它不会新建会话，
    /// 也不需要 bundle**。秘密聊天里"第二次及以后的消息"必须走这条——
    /// 以前调用层自己判断 `has_session_with` 然后传 `bundle = None`，
    /// 结果落进了"没有 bundle 就发明文"的分支（issue #4）。
    ///
    /// fail closed：会话不存在时报错，绝不退化成明文。
    pub fn encrypt_dm_existing(&mut self, peer_user_id: &str, plaintext: &[u8]) -> Result<Sealed> {
        let state = self
            .dm
            .get_mut(peer_user_id)
            .ok_or(CryptoError::NoSession)?;
        let message = state
            .session
            .encrypt(plaintext)
            .map_err(|_| CryptoError::DecryptFailed)?;
        Ok(match message {
            // 已有会话却产生 PreKey 消息是 olm 的正常行为（对方还没回），
            // 但**不能**标成 `pre_key: true`：那会让接收方以为要新建会话。
            OlmMessage::PreKey(pre_key) => Sealed {
                pre_key: false,
                body: pre_key.message().to_base64(),
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

    /// M-5（第四轮审计）：按**链进度距离**选驱逐对象。单个棘轮键下，字典序
    /// 恰好等于 idx 序，`pop_last` 碰巧正确；棘轮轮换后各键是不相关的 base64，
    /// 字典序与进度无关——可能把"距可投递只差 1"的消息挤掉而留下没救的。
    fn evict_furthest_ahead(
        parked: &BTreeMap<(String, u64), Sealed>,
        chain_pos: &HashMap<String, u64>,
    ) -> Option<(String, u64)> {
        parked
            .keys()
            .max_by_key(|(rk, idx)| idx.saturating_sub(chain_pos.get(rk).copied().unwrap_or(0)))
            .cloned()
    }

    fn park(&mut self, peer_user_id: &str, rk: String, idx: u64, body: String) {
        let Some(state) = self.dm.get_mut(peer_user_id) else {
            return;
        };
        if state.parked.len() >= PENDING_WINDOW {
            if let Some(victim) = Self::evict_furthest_ahead(&state.parked, &state.chain_pos) {
                state.parked.remove(&victim);
            }
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
    // persistence
    // -------------------------------------------------------------------------

    /// Everything that must survive a restart: the account (identity + unused
    /// one-time keys) and every 1:1 ratchet.
    ///
    /// Ratchet state is *not* a cache. If it is lost the peer's next message
    /// arrives at an index our chain never saw, so a restart without this
    /// snapshot silently turns every running conversation into "cannot
    /// decrypt". That is why the desktop layer persists after **every** send
    /// and every successful receive, not just at shutdown.
    ///
    /// Not included, on purpose: the parked-ahead buffer and the replay window.
    /// Both are rebuilt from traffic we are about to receive again, and
    /// persisting them would mean writing a few megabytes per message.
    pub fn snapshot(&self) -> Result<Vec<u8>> {
        let snap = Snapshot {
            version: SNAPSHOT_VERSION,
            account: self.device.account.pickle(),
            dm: self
                .dm
                .iter()
                .map(|(peer, state)| {
                    (
                        peer.clone(),
                        DmSnapshot {
                            session: state.session.pickle(),
                            chain_pos: state.chain_pos.clone(),
                        },
                    )
                })
                .collect(),
            groups: self
                .groups
                .iter()
                .map(|(id, state)| {
                    (
                        id.clone(),
                        GroupSnapshot {
                            outbound: state.outbound.as_ref().map(GroupSession::pickle),
                            inbound: state
                                .inbound
                                .iter()
                                .map(|(sender, s)| (sender.clone(), s.pickle()))
                                .collect(),
                        },
                    )
                })
                .collect(),
        };
        serde_json::to_vec(&snap).map_err(|e| CryptoError::Snapshot(e.to_string()))
    }

    /// Rebuilds a `Crypto` from [`Crypto::snapshot`]. Rejects a snapshot
    /// written by a different (incompatible) version instead of guessing.
    pub fn restore(bytes: &[u8]) -> Result<Self> {
        let snap: Snapshot =
            serde_json::from_slice(bytes).map_err(|e| CryptoError::Snapshot(e.to_string()))?;
        if snap.version != SNAPSHOT_VERSION {
            return Err(CryptoError::Snapshot(format!(
                "unsupported snapshot version {} (expected {SNAPSHOT_VERSION})",
                snap.version
            )));
        }
        let mut crypto = Self::new(Device::from_account(Account::from_pickle(snap.account)));
        for (peer, state) in snap.dm {
            let mut dm = DmState::new(Session::from_pickle(state.session));
            dm.chain_pos = state.chain_pos;
            crypto.dm.insert(peer, dm);
        }
        for (id, state) in snap.groups {
            crypto.groups.insert(
                id,
                GroupState {
                    outbound: state.outbound.map(GroupSession::from_pickle),
                    inbound: state
                        .inbound
                        .into_iter()
                        .map(|(sender, s)| (sender, InboundGroupSession::from_pickle(s)))
                        .collect(),
                },
            );
        }
        Ok(crypto)
    }

    /// 用口令派生密钥密封**整份状态**（账户 + 所有棘轮），落盘格式 WXSC2。
    ///
    /// 这是桌面端应该用的入口：只封 `Device` 不够——棘轮状态不在账户里，
    /// 丢了它重启后老会话全部解不开。
    pub fn seal_with_passphrase(&self, passphrase: &str) -> Result<Vec<u8>> {
        seal_snapshot(passphrase, &self.snapshot()?)
    }

    /// [`Crypto::seal_with_passphrase`] 的逆操作；错误语义同
    /// [`Device::open_with_passphrase`]。
    pub fn open_with_passphrase(passphrase: &str, blob: &[u8]) -> Result<Self> {
        Self::restore(&open_snapshot(passphrase, blob)?)
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
        self.mark_group_seen(
            (group_id.to_string(), sender.clone(), session_id),
            plain.message_index,
        );
        Ok((sender, plain.plaintext))
    }

    /// M-6：水位写入带 FIFO 上限，长驻进程内存不再随会话数单调增长。
    fn mark_group_seen(&mut self, key: (String, String, String), idx: u32) {
        if self.group_seen.len() >= GROUP_SEEN_CAP {
            if let Some(oldest) = self.group_seen_order.pop_front() {
                self.group_seen.remove(&oldest);
            }
        }
        self.group_seen_order.push_back(key.clone());
        self.group_seen.insert(key, idx);
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

#[cfg(test)]
mod audit_round4 {
    use super::*;

    /// M-5：驱逐按链进度距离选，不按键字典序。构造"rk_a 只差 1 步可投递、
    /// rk_z 落后 90 步"的场景——字典序上 rk_z 最大，pop_last 会错杀 rk_a。
    #[test]
    fn eviction_picks_largest_chain_gap_not_lexicographic_order() {
        let mut parked = BTreeMap::new();
        let sealed = || Sealed {
            pre_key: false,
            body: "x".to_string(),
        };
        parked.insert(("rk_a".to_string(), 5), sealed());
        parked.insert(("rk_z".to_string(), 90), sealed());
        let mut chain_pos = HashMap::new();
        chain_pos.insert("rk_a".to_string(), 4u64); // gap 1：马上就绪
        chain_pos.insert("rk_z".to_string(), 0u64); // gap 90：没救的

        let victim = Crypto::evict_furthest_ahead(&parked, &chain_pos);
        assert_eq!(
            victim,
            Some(("rk_z".to_string(), 90)),
            "the furthest-ahead-by-progress message is evicted, not the lexicographic max"
        );
        // 空表与单元素边界
        let mut one = BTreeMap::new();
        one.insert(("k".to_string(), 7), sealed());
        assert_eq!(
            Crypto::evict_furthest_ahead(&one, &HashMap::new()),
            Some(("k".to_string(), 7))
        );
        assert_eq!(
            Crypto::evict_furthest_ahead(&BTreeMap::new(), &HashMap::new()),
            None
        );
    }

    /// M-6：群水位表有上限，长驻进程内存收敛。
    #[test]
    fn group_seen_watermarks_are_capped() {
        let mut crypto = Crypto::new(Device::new());
        for i in 0..(GROUP_SEEN_CAP + 64) {
            crypto.mark_group_seen(
                (
                    format!("g{}", i % 3),
                    format!("sender{}", i),
                    format!("sess{}", i),
                ),
                1,
            );
        }
        assert!(
            crypto.group_seen.len() <= GROUP_SEEN_CAP,
            "watermark table must be capped at {GROUP_SEEN_CAP}, got {}",
            crypto.group_seen.len()
        );
        assert_eq!(crypto.group_seen.len(), crypto.group_seen_order.len());
    }
}
