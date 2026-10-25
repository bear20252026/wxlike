//! G09-04 端到端媒体加密（SFU 不可解密）。
//!
//! DTLS-SRTP 只保护"客户端 ↔ SFU"这一段；SFU 本身能看到明文。本模块把
//! 加密上移到媒体帧：发送端用**只有房间成员知道**的群密钥派生每发送者
//! 子密钥，SFU 只转发密文。
//!
//! 设计要点：
//!
//! * **密钥派发不在服务端**：群密钥走 G02/G03 的端到端消息层；SFU 只广播
//!   "代次变了"，不接触密钥字节。
//! * **退房轮换**：成员集合变化时客户端安装新代次 + 新群密钥；被移除者
//!   没有新密钥，旧代次帧也因 `epoch` 不匹配被拒绝。
//! * **防重放**：每发送者 128 帧滑动窗口；AAD 覆盖 call/epoch/sender/seq，
//!   篡改头部会直接解密失败。
//! * **不落日志**：密钥类型的 Debug 输出恒为 `[redacted]`，Drop 时清零。

use std::collections::{BTreeSet, HashMap};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// RFC 5869 HKDF-SHA256（extract + expand）。密钥派生只依赖仓库里已有的
/// hmac/sha2，不再为它单独引入 hkdf crate。
mod hkdf_sha256 {
    use super::{HmacSha256, Mac};

    /// SHA-256 摘要与 HMAC 输出宽度。
    const HASH_LEN: usize = 32;

    fn hmac(key: &[u8], data: &[&[u8]]) -> [u8; HASH_LEN] {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC accepts any key");
        for part in data {
            mac.update(part);
        }
        mac.finalize().into_bytes().into()
    }

    pub fn extract(salt: &[u8], ikm: &[u8]) -> [u8; HASH_LEN] {
        hmac(salt, &[ikm])
    }

    /// `okm` 最长 255×32 字节（协议里只取 32 字节，够用）。
    pub fn expand(prk: &[u8], info: &[u8], okm: &mut [u8]) {
        let mut t = [0u8; HASH_LEN];
        let mut prev_len = 0usize; // T(0) 为空
        let mut filled = 0usize;
        let mut counter: u8 = 1;
        while filled < okm.len() {
            let block: [u8; HASH_LEN] = hmac(prk, &[&t[..prev_len], info, &[counter]]);
            let n = (okm.len() - filled).min(HASH_LEN);
            okm[filled..filled + n].copy_from_slice(&block[..n]);
            t.copy_from_slice(&block);
            prev_len = HASH_LEN;
            filled += n;
            counter += 1;
        }
    }
}

/// 帧格式版本。
pub const FRAME_VERSION: u8 = 1;
/// 群密钥长度（HKDF 输出）。
pub const KEY_LEN: usize = 32;
/// 每发送者允许的乱序窗口。
pub const REPLAY_WINDOW: u64 = 128;
/// 发送者 id 的长度上限（帧头是单字节长度）。
const MAX_SENDER_LEN: usize = 64;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum E2eeError {
    #[error("a room needs at least one member")]
    EmptyRoom,
    #[error("{0} is not a member of this room")]
    NotMember(String),
    #[error("frame epoch {0} is not the current epoch")]
    UnknownEpoch(u64),
    #[error("malformed encrypted frame")]
    BadFrame,
    #[error("frame authentication failed")]
    Decrypt,
    #[error("replayed or too-old frame")]
    Replay,
    #[error("rotation must increase the epoch")]
    EpochNotIncreasing,
}

/// 32 字节群密钥；Drop 清零，Debug 脱敏。
struct Secret([u8; KEY_LEN]);

impl Clone for Secret {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        for b in &mut self.0 {
            *b = 0;
        }
    }
}

/// 窗口是 128 bit = 两个 u64；位运算都基于这个宽度。
const WORD_BITS: u64 = 64;
/// ChaCha20-Poly1305 的 nonce 宽度。
const NONCE_LEN: usize = 12;

#[derive(Default)]
struct ReplayWindow {
    highest: u64,
    bits: [u64; 2],
}

impl ReplayWindow {
    /// 接受则置位并返回 true；重复或过旧返回 false。
    fn check_and_mark(&mut self, seq: u64) -> bool {
        if seq > self.highest {
            self.shift(seq - self.highest);
            self.highest = seq;
            self.bits[0] |= 1;
            return true;
        }
        let diff = self.highest - seq;
        if diff >= REPLAY_WINDOW {
            return false;
        }
        let (word, bit) = ((diff / WORD_BITS) as usize, 1u64 << (diff % WORD_BITS));
        if self.bits[word] & bit != 0 {
            return false;
        }
        self.bits[word] |= bit;
        true
    }

    fn shift(&mut self, by: u64) {
        if by >= REPLAY_WINDOW {
            self.bits = [0, 0];
        } else if by >= WORD_BITS {
            let lo = self.bits[0];
            self.bits[0] = 0;
            self.bits[1] = lo << (by - WORD_BITS);
        } else {
            let (lo, hi) = (self.bits[0], self.bits[1]);
            self.bits[0] = lo << by;
            self.bits[1] = (hi << by) | (lo >> (WORD_BITS - by));
        }
    }
}

/// 一端持有的媒体密钥环。一个通话/直播一个实例。
pub struct MediaKeyRing {
    call: String,
    epoch: u64,
    secret: Secret,
    members: BTreeSet<String>,
    replay: HashMap<String, ReplayWindow>,
}

impl std::fmt::Debug for MediaKeyRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaKeyRing")
            .field("call", &self.call)
            .field("epoch", &self.epoch)
            .field("members", &self.members)
            .field("secret", &"[redacted]")
            .finish()
    }
}

impl MediaKeyRing {
    pub fn new(
        call: impl Into<String>,
        epoch: u64,
        secret: [u8; KEY_LEN],
        members: impl IntoIterator<Item = String>,
    ) -> Result<Self, E2eeError> {
        let call = call.into();
        let members: BTreeSet<String> = members.into_iter().filter(|m| !m.is_empty()).collect();
        if call.is_empty() || members.is_empty() {
            return Err(E2eeError::EmptyRoom);
        }
        Ok(Self {
            call,
            epoch,
            secret: Secret(secret),
            members,
            replay: HashMap::new(),
        })
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn call(&self) -> &str {
        &self.call
    }

    pub fn members(&self) -> &BTreeSet<String> {
        &self.members
    }

    pub fn is_member(&self, user: &str) -> bool {
        self.members.contains(user)
    }

    /// 成员变化后安装新代次。代次必须严格递增；被移除者的旧环不会得到这里
    /// 的密钥，因此既解不开新帧，旧代次帧也被拒绝。
    pub fn install_rotation(
        &mut self,
        epoch: u64,
        secret: [u8; KEY_LEN],
        members: impl IntoIterator<Item = String>,
    ) -> Result<(), E2eeError> {
        if epoch <= self.epoch {
            return Err(E2eeError::EpochNotIncreasing);
        }
        let members: BTreeSet<String> = members.into_iter().filter(|m| !m.is_empty()).collect();
        if members.is_empty() {
            return Err(E2eeError::EmptyRoom);
        }
        self.epoch = epoch;
        self.secret = Secret(secret);
        self.members = members;
        self.replay.clear();
        Ok(())
    }

    /// 加密一帧。`seq` 必须由发送者在当前代次内单调递增。
    pub fn seal(&self, sender: &str, seq: u64, plaintext: &[u8]) -> Result<Vec<u8>, E2eeError> {
        if !self.members.contains(sender) {
            return Err(E2eeError::NotMember(sender.to_string()));
        }
        if sender.len() > MAX_SENDER_LEN {
            return Err(E2eeError::BadFrame);
        }
        let header = self.header(sender, seq);
        let cipher = self.sender_cipher(sender)?;
        let nonce: Nonce = Self::nonce(self.epoch, seq).into();
        let body = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &header,
                },
            )
            .map_err(|_| E2eeError::Decrypt)?;
        let mut frame = header;
        frame.extend_from_slice(&body);
        Ok(frame)
    }

    /// 解密一帧并做重放检查。只有解密成功后才推进窗口。
    pub fn open(&mut self, sender: &str, frame: &[u8]) -> Result<Vec<u8>, E2eeError> {
        let (epoch, seq, body) = Self::parse(frame)?;
        if epoch != self.epoch {
            return Err(E2eeError::UnknownEpoch(epoch));
        }
        if !self.members.contains(sender) {
            return Err(E2eeError::NotMember(sender.to_string()));
        }
        let header = &frame[..frame.len() - body.len()];
        let cipher = self.sender_cipher(sender)?;
        let nonce: Nonce = Self::nonce(epoch, seq).into();
        let plaintext = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: body,
                    aad: header,
                },
            )
            .map_err(|_| E2eeError::Decrypt)?;
        let window = self.replay.entry(sender.to_string()).or_default();
        if !window.check_and_mark(seq) {
            return Err(E2eeError::Replay);
        }
        Ok(plaintext)
    }

    fn header(&self, sender: &str, seq: u64) -> Vec<u8> {
        let mut header = Vec::with_capacity(1 + 8 + 1 + sender.len() + 8);
        header.push(FRAME_VERSION);
        header.extend_from_slice(&self.epoch.to_be_bytes());
        header.push(sender.len() as u8);
        header.extend_from_slice(sender.as_bytes());
        header.extend_from_slice(&seq.to_be_bytes());
        header
    }

    fn parse(frame: &[u8]) -> Result<(u64, u64, &[u8]), E2eeError> {
        if frame.len() < 1 + 8 + 1 + 8 + 16 || frame[0] != FRAME_VERSION {
            return Err(E2eeError::BadFrame);
        }
        let epoch = u64::from_be_bytes(frame[1..9].try_into().map_err(|_| E2eeError::BadFrame)?);
        let sender_len = frame[9] as usize;
        if sender_len == 0 || sender_len > MAX_SENDER_LEN {
            return Err(E2eeError::BadFrame);
        }
        let seq_at = 10 + sender_len;
        if frame.len() < seq_at + 8 + 16 {
            return Err(E2eeError::BadFrame);
        }
        let seq = u64::from_be_bytes(
            frame[seq_at..seq_at + 8]
                .try_into()
                .map_err(|_| E2eeError::BadFrame)?,
        );
        Ok((epoch, seq, &frame[seq_at + 8..]))
    }

    fn sender_cipher(&self, sender: &str) -> Result<ChaCha20Poly1305, E2eeError> {
        let salt = self.call.as_bytes();
        let info = format!("wxlike-media-e2ee-v1|{}|{}", self.epoch, sender);
        let mut key = [0u8; KEY_LEN];
        hkdf_sha256::expand(
            &hkdf_sha256::extract(salt, &self.secret.0),
            info.as_bytes(),
            &mut key,
        );
        let cipher = ChaCha20Poly1305::new_from_slice(&key).map_err(|_| E2eeError::BadFrame)?;
        key.fill(0);
        Ok(cipher)
    }

    fn nonce(epoch: u64, seq: u64) -> [u8; NONCE_LEN] {
        let mut nonce = [0u8; NONCE_LEN];
        nonce[..4].copy_from_slice(&(epoch as u32).to_be_bytes());
        nonce[4..].copy_from_slice(&seq.to_be_bytes());
        nonce
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hkdf_matches_rfc5869_test_case_1() {
        // https://datatracker.ietf.org/doc/html/rfc5869#appendix-A.1
        let ikm = [0x0b; 22];
        let salt = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        ];
        let info = [0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9];
        let prk = hkdf_sha256::extract(&salt, &ikm);
        assert_eq!(
            hex::encode(prk),
            "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"
        );
        let mut okm = [0u8; 42];
        hkdf_sha256::expand(&prk, &info, &mut okm);
        assert_eq!(
            hex::encode(okm),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf\
             34007208d5b887185865"
        );
    }

    fn secret(byte: u8) -> [u8; KEY_LEN] {
        [byte; KEY_LEN]
    }

    fn ring() -> MediaKeyRing {
        MediaKeyRing::new(
            "call-1",
            1,
            secret(7),
            ["alice".to_string(), "bob".to_string()],
        )
        .unwrap()
    }

    #[test]
    fn ciphertext_round_trips_and_hides_plaintext() {
        let alice = ring();
        let mut bob = ring();
        let plain = b"opus frame: hello".to_vec();
        let frame = alice.seal("alice", 1, &plain).unwrap();
        assert!(
            !frame.windows(plain.len()).any(|w| w == plain),
            "密文不含明文"
        );
        assert_eq!(bob.open("alice", &frame).unwrap(), plain);
    }

    #[test]
    fn tampering_header_or_body_fails_authentication() {
        let alice = ring();
        let mut bob = ring();
        let frame = alice.seal("alice", 1, b"payload").unwrap();

        let mut body = frame.clone();
        *body.last_mut().unwrap() ^= 0x01;
        assert_eq!(bob.open("alice", &body), Err(E2eeError::Decrypt));

        let mut header = frame;
        header[9] = 3; // sender 长度被改，头部 AAD 不再匹配
        assert!(bob.open("alice", &header).is_err());
    }

    #[test]
    fn replay_and_old_frames_are_refused() {
        let alice = ring();
        let mut bob = ring();
        let f1 = alice.seal("alice", 1, b"one").unwrap();
        let f2 = alice.seal("alice", 2, b"two").unwrap();
        assert_eq!(bob.open("alice", &f1).unwrap(), b"one");
        assert_eq!(bob.open("alice", &f2).unwrap(), b"two");
        assert_eq!(bob.open("alice", &f1), Err(E2eeError::Replay));

        let far = alice.seal("alice", 10_000, b"far").unwrap();
        bob.open("alice", &far).unwrap();
        assert_eq!(bob.open("alice", &f1), Err(E2eeError::Replay));
    }

    #[test]
    fn rotation_locks_out_a_removed_member() {
        let old = secret(7);
        let mut alice = MediaKeyRing::new("c", 1, old, ["alice".into(), "bob".into()]).unwrap();
        let mut bob = MediaKeyRing::new("c", 1, old, ["alice".into(), "bob".into()]).unwrap();
        let mut mallory = MediaKeyRing::new(
            "c",
            1,
            old,
            ["alice".into(), "bob".into(), "mallory".into()],
        )
        .unwrap();

        let before = alice.seal("alice", 1, b"before").unwrap();
        assert!(mallory.open("alice", &before).is_ok(), "移除前可解");

        let next = secret(9);
        alice
            .install_rotation(2, next, ["alice".to_string(), "bob".to_string()])
            .unwrap();
        bob.install_rotation(2, next, ["alice".to_string(), "bob".to_string()])
            .unwrap();
        let after = alice.seal("alice", 2, b"after").unwrap();
        assert_eq!(bob.open("alice", &after).unwrap(), b"after");
        // 被移除者：没有新密钥，且新代次帧直接拒绝
        assert!(mallory.open("alice", &after).is_err());
        // 旧代次帧也不再被剩余成员接受
        let mut fresh_bob =
            MediaKeyRing::new("c", 2, next, ["alice".into(), "bob".into()]).unwrap();
        assert_eq!(
            fresh_bob.open("alice", &before),
            Err(E2eeError::UnknownEpoch(1))
        );
    }

    #[test]
    fn non_members_cannot_seal_and_debug_redacts_keys() {
        let ring = ring();
        assert_eq!(
            ring.seal("mallory", 1, b"x"),
            Err(E2eeError::NotMember("mallory".into()))
        );
        let rendered = format!("{ring:?}");
        assert!(rendered.contains("redacted"));
        assert!(!rendered.contains("0707"), "{rendered}");
    }

    #[test]
    fn epoch_must_increase() {
        let mut r = ring();
        assert_eq!(
            r.install_rotation(1, secret(1), ["alice".into()]),
            Err(E2eeError::EpochNotIncreasing)
        );
        assert_eq!(
            r.install_rotation(2, secret(1), Vec::<String>::new()),
            Err(E2eeError::EmptyRoom)
        );
    }
}
