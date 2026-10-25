//! Object storage seam plus the local filesystem adapter.
//!
//! Only content-addressed ids reach a path, and the id is validated *before*
//! any path is built. Upload staging, transcode workspaces and the derivable
//! cache live under separate directories so cleaning one cannot touch objects.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::error::{MediaError, Result};
use crate::ids::{is_sha256_hex, random_hex};

pub const STREAM_BUFFER_BYTES: usize = 64 * 1024;
const MAX_TOKEN_CHARS: usize = 128;
const PART_PREFIX: &str = ".part-";
const UPLOAD_DIR: &str = ".uploads";
const WORK_DIR: &str = ".work";
const CACHE_DIR: &str = "cache";
const SHARD_CHARS: usize = 2;

pub trait ObjectStore: Send + Sync {
    fn put_file(&self, object_id: &str, source: &Path) -> Result<u64>;
    fn put_bytes(&self, object_id: &str, bytes: &[u8]) -> Result<u64>;
    fn open(&self, object_id: &str) -> Result<Option<File>>;
    fn read_range(&self, object_id: &str, start: u64, len: usize) -> Result<Option<Vec<u8>>>;
    fn size(&self, object_id: &str) -> Result<Option<u64>>;
    fn delete(&self, object_id: &str) -> Result<bool>;
    fn upload_path(&self, upload_id: &str) -> PathBuf;
    fn work_path(&self, job_id: &str) -> PathBuf;
    fn cache_path(&self, key: &str) -> PathBuf;
}

pub struct LocalObjectStore {
    root: PathBuf,
}

impl LocalObjectStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        for sub in [UPLOAD_DIR, WORK_DIR, CACHE_DIR] {
            std::fs::create_dir_all(root.join(sub))?;
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn object_path(&self, object_id: &str) -> Result<PathBuf> {
        if !is_sha256_hex(object_id) {
            return Err(MediaError::NotFound);
        }
        Ok(self.root.join(&object_id[..SHARD_CHARS]).join(object_id))
    }

    fn staged_path(dir: &Path, tag: &str) -> PathBuf {
        dir.join(format!("{PART_PREFIX}{tag}"))
    }

    fn publish(&self, object_id: &str, tmp: &Path) -> Result<u64> {
        let dest = self.object_path(object_id)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let size = std::fs::metadata(tmp)?.len();
        if dest.exists() {
            std::fs::remove_file(tmp).ok();
            return Ok(size);
        }
        if let Err(err) = std::fs::rename(tmp, &dest) {
            std::fs::remove_file(tmp).ok();
            if dest.exists() {
                return Ok(size);
            }
            return Err(err.into());
        }
        Ok(size)
    }

    fn ensure_token(token: &str) -> Result<()> {
        let valid = !token.is_empty()
            && token.len() <= MAX_TOKEN_CHARS
            && token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
        if valid {
            Ok(())
        } else {
            Err(MediaError::bad("malformed storage token"))
        }
    }
}

impl ObjectStore for LocalObjectStore {
    fn put_file(&self, object_id: &str, source: &Path) -> Result<u64> {
        let mut input = File::open(source)?;
        let tmp = Self::staged_path(&self.root, &random_hex(8));
        let mut output = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
        std::io::copy(&mut input, &mut output)?;
        output.flush()?;
        drop(output);
        self.publish(object_id, &tmp)
    }

    fn put_bytes(&self, object_id: &str, bytes: &[u8]) -> Result<u64> {
        let tmp = Self::staged_path(&self.root, &random_hex(8));
        std::fs::write(&tmp, bytes)?;
        self.publish(object_id, &tmp)
    }

    fn open(&self, object_id: &str) -> Result<Option<File>> {
        let path = self.object_path(object_id)?;
        match File::open(path) {
            Ok(file) => Ok(Some(file)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn read_range(&self, object_id: &str, start: u64, len: usize) -> Result<Option<Vec<u8>>> {
        let Some(mut file) = self.open(object_id)? else {
            return Ok(None);
        };
        let total = file.metadata()?.len();
        if start >= total {
            return Ok(Some(Vec::new()));
        }
        let take = len.min((total - start) as usize);
        file.seek(SeekFrom::Start(start))?;
        let mut buf = vec![0u8; take];
        file.read_exact(&mut buf)?;
        Ok(Some(buf))
    }

    fn size(&self, object_id: &str) -> Result<Option<u64>> {
        let path = self.object_path(object_id)?;
        match std::fs::metadata(path) {
            Ok(meta) => Ok(Some(meta.len())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn delete(&self, object_id: &str) -> Result<bool> {
        let path = self.object_path(object_id)?;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err.into()),
        }
    }

    fn upload_path(&self, upload_id: &str) -> PathBuf {
        let safe = Self::ensure_token(upload_id).is_ok();
        let name = if safe { upload_id } else { "invalid" };
        self.root.join(UPLOAD_DIR).join(format!("{name}.part"))
    }

    fn work_path(&self, job_id: &str) -> PathBuf {
        let safe = Self::ensure_token(job_id).is_ok();
        let name = if safe { job_id } else { "invalid" };
        self.root.join(WORK_DIR).join(name)
    }

    fn cache_path(&self, key: &str) -> PathBuf {
        let safe = Self::ensure_token(key).is_ok();
        let name = if safe { key } else { "invalid" };
        self.root.join(CACHE_DIR).join(name)
    }
}

/// Hash a file without loading it: fixed-size buffer, constant memory.
pub fn stream_digest_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; STREAM_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Disk usage of a staging tree, used by the upload quota and cache planner.
pub fn tree_bytes(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.metadata().ok())
        .filter(|meta| meta.is_file())
        .map(|meta| meta.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_building_rejects_non_content_ids() {
        let dir = std::env::temp_dir().join(format!("wxlike-media-store-{}", random_hex(4)));
        let store = LocalObjectStore::new(&dir).unwrap();
        assert!(store.put_bytes("../../etc/passwd", b"x").is_err());
        assert!(matches!(store.size("../../x"), Err(MediaError::NotFound)));
        assert!(store.upload_path("../escape").ends_with("invalid.part"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dedup_and_range_reads_are_real() {
        let dir = std::env::temp_dir().join(format!("wxlike-media-range-{}", random_hex(4)));
        let store = LocalObjectStore::new(&dir).unwrap();
        let bytes = b"hello media world";
        let id = crate::ids::sha256_hex(bytes);
        store.put_bytes(&id, bytes).unwrap();
        store.put_bytes(&id, bytes).unwrap();
        assert_eq!(store.size(&id).unwrap(), Some(bytes.len() as u64));
        let got = store.read_range(&id, 6, 5).unwrap().unwrap();
        assert_eq!(got, b"media");
        std::fs::remove_dir_all(dir).ok();
    }
}
