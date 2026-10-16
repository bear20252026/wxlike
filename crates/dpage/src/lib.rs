//! wxlike-dpage: dynamic page packages.
//!
//! Own container design, informed by the CSO analysis of the research corpus:
//! a small stub header (magic + id + flags) followed by a Zstandard-compressed
//! payload. The stub lets a host identify and index packages without
//! decompressing; the payload is a bincode-serialized file map.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

pub const MAGIC: &[u8; 4] = b"DPG1";
pub const STUB_LEN: usize = 20; // magic(4) + version(4) + id(4) + payload_len(8)

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Manifest {
    pub name: String,
    /// entry script path, relative to the package root
    pub main_js: String,
    /// optional page title
    #[serde(default)]
    pub title: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct FileMap(pub Vec<FileEntry>);

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileEntry {
    pub path: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Page {
    pub id: u32,
    pub manifest: Manifest,
    pub files: std::collections::BTreeMap<String, Vec<u8>>,
}

impl Page {
    pub fn main_js(&self) -> anyhow::Result<&str> {
        let data = self
            .files
            .get(&self.manifest.main_js)
            .with_context(|| format!("missing entry script {}", self.manifest.main_js))?;
        std::str::from_utf8(data).context("entry script is not UTF-8")
    }
}

/// Pack a directory tree (manifest.json + assets) into a `.dpage` blob.
pub fn pack_dir(dir: &std::path::Path) -> anyhow::Result<Vec<u8>> {
    let manifest_path = dir.join("manifest.json");
    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)
        .with_context(|| format!("parse {}", manifest_path.display()))?;

    let mut files = FileMap::default();
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut FileMap) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out)?;
            } else {
                let rel = path
                    .strip_prefix(base)?
                    .to_string_lossy()
                    .replace('\\', "/");
                if rel == "manifest.json" {
                    continue;
                }
                out.0.push(FileEntry {
                    path: rel,
                    data: std::fs::read(&path)?,
                });
            }
        }
        Ok(())
    }
    walk(dir, dir, &mut files)?;
    files.0.push(FileEntry {
        path: "manifest.json".into(),
        data: std::fs::read(&manifest_path)?,
    });

    let payload = zstd::stream::encode_all(bincode::serialize(&files)?.as_slice(), 6)?;
    Ok(format_blob(next_id(dir), payload))
}

/// Read manifest without decompressing the payload (stub-only inspection).
pub fn peek(blob: &[u8]) -> anyhow::Result<(u32, u32)> {
    anyhow::ensure!(blob.len() >= STUB_LEN, "blob shorter than stub");
    anyhow::ensure!(&blob[..4] == MAGIC, "bad magic");
    Ok((
        u32::from_be_bytes(blob[4..8].try_into().unwrap()),
        u32::from_be_bytes(blob[8..12].try_into().unwrap()),
    ))
}

pub fn unpack(blob: &[u8]) -> anyhow::Result<Page> {
    let (version, id) = peek(blob)?;
    anyhow::ensure!(version == 1, "unsupported dpage version {version}");
    let payload_len = u64::from_be_bytes(blob[12..20].try_into().unwrap()) as usize;
    anyhow::ensure!(
        blob.len() == STUB_LEN + payload_len,
        "length mismatch: stub says {payload_len}, blob has {}",
        blob.len() - STUB_LEN
    );
    let files: FileMap = bincode::deserialize(&zstd::stream::decode_all(&blob[STUB_LEN..])?)?;
    let manifest_entry = files
        .0
        .iter()
        .find(|f| f.path == "manifest.json")
        .context("package lacks manifest.json")?;
    let manifest: Manifest = serde_json::from_slice(&manifest_entry.data)?;
    let mut map = std::collections::BTreeMap::new();
    for f in files.0 {
        map.insert(f.path, f.data);
    }
    Ok(Page { id, manifest, files: map })
}

fn format_blob(id: u32, payload: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(STUB_LEN + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

fn next_id(dir: &std::path::Path) -> u32 {
    // deterministic-ish id from the dir name so rebuilds are reproducible
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut h: u32 = 0x811c9dc5;
    for b in name.as_bytes() {
        h = h.wrapping_mul(0x01000193).wrapping_add(*b as u32);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_demo_page(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"name":"demo","main_js":"main.js","title":"Demo"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("main.js"), b"wxhost.log('hello from page');").unwrap();
        std::fs::write(dir.join("style.css"), b"body{}").unwrap();
    }

    #[test]
    fn pack_peek_unpack_roundtrip() {
        let tmp = std::env::temp_dir().join("wxlike-dpage-test");
        let page_dir = tmp.join("demo_page");
        let _ = std::fs::remove_dir_all(&page_dir);
        make_demo_page(&page_dir);

        let blob = pack_dir(&page_dir).unwrap();
        let (version, id) = peek(&blob).unwrap();
        assert_eq!(version, 1);

        let page = unpack(&blob).unwrap();
        assert_eq!(page.id, id);
        assert_eq!(page.manifest.name, "demo");
        assert!(page.files.contains_key("style.css"));
        assert!(page.main_js().unwrap().contains("hello from page"));
    }

    #[test]
    fn compressed_size_is_smaller_for_repetitive_input() {
        let payload = vec![b'a'; 10_000];
        let blob = format_blob(1, zstd::stream::encode_all(payload.as_slice(), 6).unwrap());
        assert!(blob.len() < 1_000);
    }
}
