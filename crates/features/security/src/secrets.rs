//! G02-04 本地密钥库（SecretStore）。
//!
//! # 为什么不是"再放一个密钥文件"
//!
//! 旧路径 `secret-device.key` 是**裸密钥**：目录被复制走，密钥就跟着走了。
//! 这里的做法是引入一把随机**主密钥**，只让主密钥去加密数据，然后用三条互相
//! 独立的路径各自包裹主密钥：
//!
//! ```text
//!             ┌── 口令 (WXSC2: Argon2id 头 + 盐) ──┐
//! 主密钥 K ───┼── 恢复码 (HKDF-SHA256 → XChaCha) ──┼── 包裹后写进库文件
//!             └── OS 密钥库 (seal_at_rest) ────────┘
//! ```
//!
//! * 换口令只重包 K，**不动数据**（大文件不必重加密）；
//! * 恢复码是一次性展示的高熵串，丢了 K 就真的找不回来——产品上必须说清楚，
//!   绝不伪造"无密钥找回"；
//! * OS 密钥库那条路径与口令恢复包**分开存**：系统钥匙串被清掉时还能用口令开，
//!   反之亦然；
//! * 解锁后 K 只在内存里（`Zeroizing`），`lock()` 立刻清零；
//! * 迁移/轮换都是"新包先能解开，再原子替换"，中途断电只会留下一个临时文件。

use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead as _, Payload};
use chacha20poly1305::{KeyInit as _, XChaCha20Poly1305, XNonce};
use zeroize::{Zeroize, Zeroizing};

use crate::{Result, SecError};

/// 库文件魔数。旧格式（裸密钥、WXSC1 快照）一律不认。
pub const VAULT_MAGIC: &[u8; 5] = b"WXSV1";
/// 库文件布局版本。
pub const VAULT_VERSION: u8 = 1;
/// 主密钥长度。
pub const KEY_LEN: usize = 32;
/// XChaCha20Poly1305 的 nonce 长度。
pub const NONCE_LEN: usize = 24;
/// 恢复码随机字节数（展示成 64 个 hex 字符）。
pub const RECOVERY_BYTES: usize = 32;
/// 魔数长度。
const MAGIC_LEN: usize = 5;
/// 头部各字段偏移，与 [`SecretStore::header`] 的写入顺序一一对应。
const OFF_VERSION: usize = MAGIC_LEN;
const OFF_PASS_LEN: usize = OFF_VERSION + 1;
const OFF_REC_LEN: usize = OFF_PASS_LEN + 2;
const OFF_OS_LEN: usize = OFF_REC_LEN + 2;
const OFF_PAYLOAD_LEN: usize = OFF_OS_LEN + 2;
/// 头部固定长度：magic + version + 三个 u16 + 一个 u32。
const HEADER_LEN: usize = OFF_PAYLOAD_LEN + 4;
/// XChaCha20Poly1305 认证标签长度（算密文总长要用）。
const TAG_LEN: usize = 16;
/// 单个包裹段的最大长度（防止畸形文件让我们按它声明的长度读满内存）。
const MAX_WRAP_LEN: usize = 64 * 1024;
/// 载荷上限：本地密钥库不该超过这个量级。
const MAX_PAYLOAD_LEN: usize = 8 * 1024 * 1024;
/// 临时文件后缀（迁移中断后残留）。
pub const TMP_SUFFIX: &str = ".tmp";

/// 密钥库接口。`seal`/`open` 作用在"受保护载荷"上，不是主密钥本身。
pub trait SecretStorePort {
    fn unlock(&mut self, passphrase: &str) -> Result<()>;
    fn lock(&mut self);
    fn is_unlocked(&self) -> bool;
    fn seal(&mut self, plaintext: &[u8]) -> Result<()>;
    fn open(&self) -> Result<Vec<u8>>;
    fn rotate(&mut self, new_passphrase: &str) -> Result<()>;
    fn recover(&mut self, recovery_code: &str, new_passphrase: &str) -> Result<()>;
}

/// 本地密钥库。
pub struct SecretStore {
    pass_wrap: Vec<u8>,
    rec_wrap: Vec<u8>,
    os_wrap: Vec<u8>,
    payload: Vec<u8>,
    master: Option<Zeroizing<[u8; KEY_LEN]>>,
    /// 只在 `create` 之后有值：恢复码必须展示给用户一次，之后服务端/本地都不留。
    recovery_code: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretStore")
            .field("unlocked", &self.master.is_some())
            .field("has_recovery", &!self.rec_wrap.is_empty())
            .field("has_os_wrap", &!self.os_wrap.is_empty())
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut out);
    out
}

fn aead(key: &[u8; KEY_LEN]) -> Result<XChaCha20Poly1305> {
    XChaCha20Poly1305::new_from_slice(key).map_err(|_| SecError::Corrupted("bad key length"))
}

fn seal_with(key: &[u8; KEY_LEN], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let nonce = random_bytes::<NONCE_LEN>();
    let body = aead(key)?
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| SecError::Corrupted("seal failed"))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&body);
    Ok(out)
}

fn open_with(key: &[u8; KEY_LEN], blob: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    if blob.len() <= NONCE_LEN {
        return Err(SecError::Corrupted("truncated ciphertext"));
    }
    let (nonce, body) = blob.split_at(NONCE_LEN);
    aead(key)?
        .decrypt(XNonce::from_slice(nonce), Payload { msg: body, aad })
        .map_err(|_| SecError::Corrupted("payload failed authentication"))
}

/// 恢复码 → 32 字节包裹密钥（HKDF-SHA256，域分离的 info）。
fn recovery_key(code: &str) -> Result<[u8; KEY_LEN]> {
    let hk = hkdf::Hkdf::<sha2::Sha256>::new(Some(b"wxlike-vault-recovery-v1"), code.as_bytes());
    let mut out = [0u8; KEY_LEN];
    hk.expand(b"master-key-wrap", &mut out)
        .map_err(|_| SecError::Corrupted("hkdf expand"))?;
    Ok(out)
}

impl SecretStore {
    /// 新建密钥库：生成主密钥与恢复码。**恢复码只在这里返回一次**。
    pub fn create(passphrase: &str) -> Result<Self> {
        if passphrase.is_empty() {
            return Err(SecError::BadInput("passphrase"));
        }
        let master = random_bytes::<KEY_LEN>();
        let code = hex::encode(random_bytes::<RECOVERY_BYTES>());
        let pass_wrap = wxlike_core::e2ee::seal_secret_blob(passphrase, &master)?;
        let rec_key = recovery_key(&code)?;
        let rec_wrap = seal_with(&rec_key, &master, b"wxlike-vault-recovery")?;
        Ok(Self {
            pass_wrap,
            rec_wrap,
            os_wrap: Vec::new(),
            payload: Vec::new(),
            master: Some(Zeroizing::new(master)),
            recovery_code: Some(Zeroizing::new(code)),
        })
    }

    /// 恢复码（仅 `create` 之后可用）。
    pub fn recovery_code(&self) -> Option<&str> {
        self.recovery_code.as_deref().map(|s| s.as_str())
    }

    /// 用口令解锁。
    pub fn unlock(&mut self, passphrase: &str) -> Result<()> {
        let plain = wxlike_core::e2ee::open_secret_blob(passphrase, &self.pass_wrap)?;
        let key = to_key(&plain)?;
        self.master = Some(Zeroizing::new(key));
        Ok(())
    }

    /// 用 OS 密钥库里的 32 字节密钥解锁（与口令路径互相独立）。
    pub fn unlock_with_os_key(&mut self, os_key: &[u8; KEY_LEN]) -> Result<()> {
        if self.os_wrap.is_empty() {
            return Err(SecError::NotFound("os key wrap"));
        }
        let plain = wxlike_core::e2ee::open_at_rest(os_key, &self.os_wrap)?;
        let key = to_key(&plain)?;
        self.master = Some(Zeroizing::new(key));
        Ok(())
    }

    /// 绑定一条 OS 密钥库包裹（`os_key` 由系统钥匙串保管）。
    pub fn bind_os_key(&mut self, os_key: &[u8; KEY_LEN]) -> Result<()> {
        let master = self.master_key()?;
        self.os_wrap = wxlike_core::e2ee::seal_at_rest(os_key, &master)?;
        Ok(())
    }

    /// 上锁：主密钥立刻清零。
    pub fn lock_now(&mut self) {
        if let Some(key) = self.master.as_mut() {
            key.zeroize();
        }
        self.master = None;
    }

    pub fn is_unlocked(&self) -> bool {
        self.master.is_some()
    }

    /// 加密受保护载荷（覆盖写）。要求已解锁。
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<()> {
        if plaintext.len() > MAX_PAYLOAD_LEN {
            return Err(SecError::BadInput("payload too large"));
        }
        let key = self.master_key()?;
        // AAD 里的长度必须是**密文**长度：它由明文长度唯一确定
        // （nonce + 明文 + tag），这样 `open` 才能用实际密文长度重建同一个头部。
        let ct_len = NONCE_LEN + plaintext.len() + TAG_LEN;
        let header = self.header(ct_len as u32);
        self.payload = seal_with(&key, plaintext, &header)?;
        Ok(())
    }

    /// 解密受保护载荷。要求已解锁。
    pub fn open(&self) -> Result<Vec<u8>> {
        if self.payload.is_empty() {
            return Ok(Vec::new());
        }
        let key = self.master_key()?;
        let header = self.header(self.payload.len() as u32);
        open_with(&key, &self.payload, &header)
    }

    /// 换口令：只重包主密钥，**不动载荷**；新包先验证能解再替换。
    pub fn rotate(&mut self, new_passphrase: &str) -> Result<()> {
        if new_passphrase.is_empty() {
            return Err(SecError::BadInput("passphrase"));
        }
        let master = self.master_key()?;
        let fresh = wxlike_core::e2ee::seal_secret_blob(new_passphrase, &master)?;
        let check = wxlike_core::e2ee::open_secret_blob(new_passphrase, &fresh)?;
        if to_key(&check)? != master {
            return Err(SecError::Corrupted("rotation produced a different key"));
        }
        self.pass_wrap = fresh;
        Ok(())
    }

    /// 用恢复码重置口令（主密钥不变，因此数据仍可解）。
    pub fn recover(&mut self, recovery_code: &str, new_passphrase: &str) -> Result<()> {
        if self.rec_wrap.is_empty() {
            return Err(SecError::NotFound("recovery wrap"));
        }
        let rec_key = recovery_key(recovery_code)?;
        let plain = open_with(&rec_key, &self.rec_wrap, b"wxlike-vault-recovery")?;
        let key = to_key(&plain)?;
        self.master = Some(Zeroizing::new(key));
        self.rotate(new_passphrase)
    }

    fn master_key(&self) -> Result<[u8; KEY_LEN]> {
        match self.master.as_ref() {
            Some(k) => Ok(**k),
            None => Err(SecError::Forbidden("vault locked")),
        }
    }

    fn header(&self, payload_len: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN);
        out.extend_from_slice(VAULT_MAGIC);
        out.push(VAULT_VERSION);
        out.extend_from_slice(&(self.pass_wrap.len() as u16).to_be_bytes());
        out.extend_from_slice(&(self.rec_wrap.len() as u16).to_be_bytes());
        out.extend_from_slice(&(self.os_wrap.len() as u16).to_be_bytes());
        out.extend_from_slice(&payload_len.to_be_bytes());
        out
    }

    /// 序列化成库文件字节。
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        if self.pass_wrap.len() > MAX_WRAP_LEN
            || self.rec_wrap.len() > MAX_WRAP_LEN
            || self.os_wrap.len() > MAX_WRAP_LEN
            || self.payload.len() > MAX_PAYLOAD_LEN
        {
            return Err(SecError::BadInput("vault too large"));
        }
        let mut out = self.header(self.payload.len() as u32);
        out.extend_from_slice(&self.pass_wrap);
        out.extend_from_slice(&self.rec_wrap);
        out.extend_from_slice(&self.os_wrap);
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    /// 从库文件字节恢复（**未解锁**状态）。
    pub fn from_bytes(blob: &[u8]) -> Result<Self> {
        if blob.len() < HEADER_LEN || &blob[..MAGIC_LEN] != VAULT_MAGIC {
            return Err(SecError::BadInput("not a WXSV1 vault"));
        }
        if blob[OFF_VERSION] != VAULT_VERSION {
            return Err(SecError::BadInput("unsupported vault version"));
        }
        let pass_len = u16::from_be_bytes([blob[OFF_PASS_LEN], blob[OFF_PASS_LEN + 1]]) as usize;
        let rec_len = u16::from_be_bytes([blob[OFF_REC_LEN], blob[OFF_REC_LEN + 1]]) as usize;
        let os_len = u16::from_be_bytes([blob[OFF_OS_LEN], blob[OFF_OS_LEN + 1]]) as usize;
        let payload_len = u32::from_be_bytes([
            blob[OFF_PAYLOAD_LEN],
            blob[OFF_PAYLOAD_LEN + 1],
            blob[OFF_PAYLOAD_LEN + 2],
            blob[OFF_PAYLOAD_LEN + 3],
        ]) as usize;
        if [pass_len, rec_len, os_len]
            .iter()
            .any(|n| *n > MAX_WRAP_LEN)
            || payload_len > MAX_PAYLOAD_LEN
        {
            return Err(SecError::Corrupted("declared length out of range"));
        }
        let need = HEADER_LEN + pass_len + rec_len + os_len + payload_len;
        if blob.len() != need {
            return Err(SecError::Corrupted("truncated vault"));
        }
        let mut at = HEADER_LEN;
        let pass_wrap = blob[at..at + pass_len].to_vec();
        at += pass_len;
        let rec_wrap = blob[at..at + rec_len].to_vec();
        at += rec_len;
        let os_wrap = blob[at..at + os_len].to_vec();
        at += os_len;
        let payload = blob[at..at + payload_len].to_vec();
        Ok(Self {
            pass_wrap,
            rec_wrap,
            os_wrap,
            payload,
            master: None,
            recovery_code: None,
        })
    }

    /// 原子落盘：先写 `.tmp` 再 rename，覆盖前不做任何破坏性操作。
    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        let bytes = self.to_bytes()?;
        let tmp = tmp_path(path);
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// 读库文件（未解锁）。
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    /// 从旧 `secret-device.key`（裸 32 字节密钥）迁移到口令库。
    ///
    /// 顺序是刻意的：**先建库 → 写载荷 → 重新解开校验 → 原子落盘 → 最后才删旧文件**。
    /// 中途任何一步失败，旧文件都还在，用户不会丢密钥。
    pub fn migrate_legacy(
        legacy_key_path: &Path,
        vault_path: &Path,
        passphrase: &str,
    ) -> Result<Self> {
        let legacy = std::fs::read(legacy_key_path)?;
        if legacy.len() != KEY_LEN {
            return Err(SecError::BadInput("legacy key must be 32 bytes"));
        }
        let mut store = Self::create(passphrase)?;
        store.seal(&legacy)?;
        let check = store.open()?;
        if check != legacy {
            return Err(SecError::Corrupted("migration verification failed"));
        }
        store.save_atomic(vault_path)?;
        // 再读一遍落盘结果，确认不是"写成功但内容不对"。
        let mut reloaded = Self::load(vault_path)?;
        reloaded.unlock(passphrase)?;
        if reloaded.open()? != legacy {
            return Err(SecError::Corrupted("saved vault does not reopen"));
        }
        std::fs::remove_file(legacy_key_path)?;
        Ok(store)
    }
}

fn to_key(plain: &[u8]) -> Result<[u8; KEY_LEN]> {
    if plain.len() != KEY_LEN {
        return Err(SecError::Corrupted("unexpected key length"));
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(plain);
    Ok(key)
}

/// 库文件旁边的临时文件路径。
pub fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(TMP_SUFFIX);
    PathBuf::from(name)
}

impl SecretStorePort for SecretStore {
    fn unlock(&mut self, passphrase: &str) -> Result<()> {
        SecretStore::unlock(self, passphrase)
    }

    fn lock(&mut self) {
        self.lock_now();
    }

    fn is_unlocked(&self) -> bool {
        SecretStore::is_unlocked(self)
    }

    fn seal(&mut self, plaintext: &[u8]) -> Result<()> {
        SecretStore::seal(self, plaintext)
    }

    fn open(&self) -> Result<Vec<u8>> {
        SecretStore::open(self)
    }

    fn rotate(&mut self, new_passphrase: &str) -> Result<()> {
        SecretStore::rotate(self, new_passphrase)
    }

    fn recover(&mut self, recovery_code: &str, new_passphrase: &str) -> Result<()> {
        SecretStore::recover(self, recovery_code, new_passphrase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("wxlike-vault-{}", hex::encode(random_bytes::<8>())));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn roundtrip_seal_and_open() {
        let mut store = SecretStore::create("correct horse").unwrap();
        assert!(store.is_unlocked());
        assert!(store.recovery_code().is_some());
        store.seal(b"device account pickle").unwrap();
        assert_eq!(store.open().unwrap(), b"device account pickle");

        let bytes = store.to_bytes().unwrap();
        let mut reloaded = SecretStore::from_bytes(&bytes).unwrap();
        assert!(!reloaded.is_unlocked());
        assert!(matches!(reloaded.open(), Err(SecError::Forbidden(_))));
        reloaded.unlock("correct horse").unwrap();
        assert_eq!(reloaded.open().unwrap(), b"device account pickle");
    }

    #[test]
    fn wrong_passphrase_is_distinct_from_corruption() {
        let mut store = SecretStore::create("right").unwrap();
        store.seal(b"non-empty payload").unwrap();
        let bytes = store.to_bytes().unwrap();
        let mut locked = SecretStore::from_bytes(&bytes).unwrap();
        assert!(matches!(
            locked.unlock("wrong"),
            Err(SecError::Crypto(
                wxlike_core::e2ee::CryptoError::WrongPassphrase
            ))
        ));
        let mut broken = bytes.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xFF;
        let mut store2 = SecretStore::from_bytes(&broken).unwrap();
        store2.unlock("right").unwrap();
        assert!(matches!(store2.open(), Err(SecError::Corrupted(_))));
    }

    #[test]
    fn old_and_foreign_files_are_rejected() {
        let store = SecretStore::create("pw").unwrap();
        let mut bytes = store.to_bytes().unwrap();
        bytes[..5].copy_from_slice(b"WXSC2");
        assert!(matches!(
            SecretStore::from_bytes(&bytes),
            Err(SecError::BadInput("not a WXSV1 vault"))
        ));
        assert!(matches!(
            SecretStore::from_bytes(&[0u8; 4]),
            Err(SecError::BadInput(_))
        ));
        let truncated = &store.to_bytes().unwrap()[..HEADER_LEN];
        assert!(matches!(
            SecretStore::from_bytes(truncated),
            Err(SecError::Corrupted(_))
        ));
    }

    #[test]
    fn rotate_only_rewraps_and_keeps_data() {
        let mut store = SecretStore::create("old").unwrap();
        store.seal(b"payload").unwrap();
        store.rotate("new").unwrap();
        assert_eq!(store.open().unwrap(), b"payload");
        let bytes = store.to_bytes().unwrap();
        let mut reloaded = SecretStore::from_bytes(&bytes).unwrap();
        assert!(reloaded.unlock("old").is_err(), "旧口令必须失效");
        reloaded.unlock("new").unwrap();
        assert_eq!(reloaded.open().unwrap(), b"payload");
    }

    #[test]
    fn recovery_code_resets_passphrase_without_losing_data() {
        let mut store = SecretStore::create("forgotten").unwrap();
        let code = store.recovery_code().unwrap().to_string();
        store.seal(b"still here").unwrap();
        let bytes = store.to_bytes().unwrap();
        drop(store);

        // 用户忘了口令：不尝试 unlock，直接走恢复码。
        let mut locked = SecretStore::from_bytes(&bytes).unwrap();
        locked.recover(&code, "brand new").unwrap();
        assert_eq!(locked.open().unwrap(), b"still here");
        assert!(
            locked.unlock("forgotten").is_err(),
            "旧口令在重置后必须失效"
        );
        locked.rotate("again").unwrap();
        let mut again = SecretStore::from_bytes(&locked.to_bytes().unwrap()).unwrap();
        assert!(again.unlock("brand new").is_err());
        again.unlock("again").unwrap();
        assert_eq!(again.open().unwrap(), b"still here");
    }

    #[test]
    fn lock_zeroes_and_blocks_use() {
        let mut store = SecretStore::create("pw").unwrap();
        store.seal(b"x").unwrap();
        store.lock_now();
        assert!(!store.is_unlocked());
        assert!(matches!(store.seal(b"y"), Err(SecError::Forbidden(_))));
        assert!(matches!(store.open(), Err(SecError::Forbidden(_))));
        store.unlock("pw").unwrap();
        assert_eq!(store.open().unwrap(), b"x");
    }

    #[test]
    fn os_key_path_is_independent_of_passphrase() {
        let mut store = SecretStore::create("pw").unwrap();
        let os_key = random_bytes::<KEY_LEN>();
        store.bind_os_key(&os_key).unwrap();
        store.seal(b"secret").unwrap();
        let bytes = store.to_bytes().unwrap();

        let mut locked = SecretStore::from_bytes(&bytes).unwrap();
        locked.unlock_with_os_key(&os_key).unwrap();
        assert_eq!(locked.open().unwrap(), b"secret");

        let mut other = SecretStore::from_bytes(&bytes).unwrap();
        let wrong = random_bytes::<KEY_LEN>();
        assert!(other.unlock_with_os_key(&wrong).is_err());
    }

    #[test]
    fn legacy_key_alone_cannot_open_the_vault() {
        let dir = tempdir();
        let legacy = dir.join("secret-device.key");
        let vault = dir.join("vault.bin");
        std::fs::write(&legacy, random_bytes::<KEY_LEN>()).unwrap();
        let store = SecretStore::migrate_legacy(&legacy, &vault, "pw").unwrap();
        assert!(!legacy.exists(), "迁移成功后旧裸密钥必须删除");
        assert!(vault.exists());

        // 把整个目录复制走：没有口令，拿到文件也解不开。
        let copy = dir.join("copy");
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::copy(&vault, copy.join("vault.bin")).unwrap();
        let mut stolen = SecretStore::load(&copy.join("vault.bin")).unwrap();
        assert!(stolen.unlock("guess").is_err());
        assert!(stolen
            .unlock_with_os_key(&random_bytes::<KEY_LEN>())
            .is_err());
        assert!(matches!(stolen.open(), Err(SecError::Forbidden(_))));

        let mut owner = SecretStore::load(&vault).unwrap();
        owner.unlock("pw").unwrap();
        assert_eq!(owner.open().unwrap().len(), KEY_LEN);
        assert_eq!(store.open().unwrap().len(), KEY_LEN);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn interrupted_migration_leaves_legacy_intact() {
        let dir = tempdir();
        let legacy = dir.join("secret-device.key");
        std::fs::write(&legacy, [7u8; 16]).unwrap();
        assert!(matches!(
            SecretStore::migrate_legacy(&legacy, &dir.join("vault.bin"), "pw"),
            Err(SecError::BadInput(_))
        ));
        assert!(legacy.exists(), "失败时旧文件必须还在");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_passphrase_is_rejected() {
        assert!(matches!(
            SecretStore::create(""),
            Err(SecError::BadInput(_))
        ));
        let mut store = SecretStore::create("pw").unwrap();
        assert!(matches!(store.rotate(""), Err(SecError::BadInput(_))));
    }
}
