//! E2EE scaffolding: X25519 key agreement + HKDF-SHA256 + ChaCha20-Poly1305.
//!
//! Scaffold level: static-static key agreement per peer. Production would move
//! to a double-ratchet / post-compromise-security design on top of this.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

pub const SEAL_OVERHEAD: usize = 12 /* nonce */ + 16 /* tag */;

pub struct Identity {
    sk: StaticSecret,
    pk: PublicKey,
}

impl Identity {
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        Self::from_seed(seed)
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        let sk = StaticSecret::from(seed);
        let pk = PublicKey::from(&sk);
        Self { sk, pk }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.pk.to_bytes()
    }

    pub fn public_hex(&self) -> String {
        hex::encode(self.public_key())
    }

    /// Static-static shared secret with a peer, passed through HKDF for domain separation.
    pub fn dm_key(&self, peer_public: [u8; 32]) -> [u8; 32] {
        let shared = self.sk.diffie_hellman(&PublicKey::from(peer_public));
        let hk = Hkdf::<Sha256>::new(Some(b"wxlike-v1"), shared.as_bytes());
        let mut key = [0u8; 32];
        hk.expand(b"dm-v1", &mut key).expect("len is valid");
        key
    }
}

/// Seal to `nonce || ciphertext||tag`.
pub fn seal(key: [u8; 32], plaintext: &[u8]) -> Vec<u8> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: b"wxlike-dm",
            },
        )
        .expect("encryption cannot fail with valid key");
    let mut out = Vec::with_capacity(SEAL_OVERHEAD + plaintext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    out
}

pub fn open(key: [u8; 32], sealed: &[u8]) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(sealed.len() >= SEAL_OVERHEAD, "sealed blob too short");
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let pt = cipher
        .decrypt(
            Nonce::from_slice(&sealed[..12]),
            Payload {
                msg: &sealed[12..],
                aad: b"wxlike-dm",
            },
        )
        .map_err(|_| anyhow::anyhow!("decryption failed (wrong key or tampered data)"))?;
    Ok(pt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e2ee_roundtrip() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let k_a = alice.dm_key(bob.public_key());
        let k_b = bob.dm_key(alice.public_key());
        assert_eq!(k_a, k_b, "both sides derive the same key");

        let sealed = seal(k_a, b"hello rust");
        assert_eq!(open(k_b, &sealed).unwrap(), b"hello rust");

        let tampered = {
            let mut s = sealed.clone();
            *s.last_mut().unwrap() ^= 0xff;
            s
        };
        assert!(open(k_b, &tampered).is_err(), "tampering must fail auth");
    }

    #[test]
    fn identity_deterministic_from_seed() {
        let a = Identity::from_seed([7u8; 32]);
        let b = Identity::from_seed([7u8; 32]);
        assert_eq!(a.public_hex(), b.public_hex());
    }
}
