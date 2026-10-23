//! wxlike-dpage: dynamic page packages.
//!
//! Own container design, informed by the CSO analysis of the research corpus:
//! a small stub header (magic + id + flags) followed by a Zstandard-compressed
//! payload. The stub lets a host identify and index packages without
//! decompressing; the payload is a bincode-serialized file map.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
///
/// The stub id is a **content digest**, not the directory name: two packages
/// that differ in any byte get different ids even when their directory names
/// collide (issue #56). Entries are sorted by path before hashing so the id
/// does not depend on filesystem enumeration order.
pub fn pack_dir(dir: &std::path::Path) -> anyhow::Result<Vec<u8>> {
    let manifest_path = dir.join("manifest.json");
    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)
        .with_context(|| format!("parse {}", manifest_path.display()))?;
    let _ = &manifest; // validated here; embedded into the file map below

    let mut files = FileMap::default();
    fn walk(
        dir: &std::path::Path,
        base: &std::path::Path,
        out: &mut FileMap,
    ) -> anyhow::Result<()> {
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
    files.0.sort_by(|a, b| a.path.cmp(&b.path));

    let payload = zstd::stream::encode_all(bincode::serialize(&files)?.as_slice(), 6)?;
    Ok(format_blob(content_id(&files), payload))
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

/// 包内路径是否安全（相对、无 `..`、无盘符、无反斜杠）。
///
/// 小程序包是**不可信输入**：`unpack` 出来的 `files` 会被写盘，所以校验放在
/// 这里——每个消费者都自动安全，而不是指望调用方记得查。规则与
/// `container::package` 的 entry 校验一致，两处不该有两套标准。
pub fn safe_rel_path(path: &str) -> bool {
    // 用 char::from 写控制字符，避免源码里的转义歧义（反斜杠 = 92，冒号 = 58）
    let backslash = char::from(92u8);
    let colon = char::from(58u8);
    if path.is_empty() || path.starts_with('/') || path.contains(backslash) {
        return false;
    }
    // Windows 盘符（`C:...`）与 UNC 前缀
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == colon as u8 {
        return false;
    }
    path.split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// Hard cap on decompressed payload size (AR-4/H-5, audit round 4). A
/// `.dpage` is untrusted input: a small compressed blob can expand to
/// gigabytes ("decompression bomb") unless the reader is capped *before*
/// bincode deserialization allocates.
pub const MAX_DECOMPRESSED: usize = 64 * 1024 * 1024;

/// Decompress with an explicit ceiling: reads one byte past the cap so
/// overflow is an error, not a silent truncation that bincode would then
/// misreport as a deserialization failure.
fn decode_capped(payload: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::io::Read as _;
    let decoder = zstd::stream::read::Decoder::new(payload)?;
    let mut out = Vec::new();
    decoder
        .take(MAX_DECOMPRESSED as u64 + 1)
        .read_to_end(&mut out)?;
    anyhow::ensure!(
        out.len() <= MAX_DECOMPRESSED,
        "decompressed payload exceeds {} bytes (decompression bomb?)",
        MAX_DECOMPRESSED
    );
    Ok(out)
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
    let files: FileMap = bincode::deserialize(&decode_capped(&blob[STUB_LEN..])?)?;
    let manifest_entry = files
        .0
        .iter()
        .find(|f| f.path == "manifest.json")
        .context("package lacks manifest.json")?;
    let manifest: Manifest = serde_json::from_slice(&manifest_entry.data)?;
    let mut map = std::collections::BTreeMap::new();
    for f in files.0 {
        // 一个条目越界就整包拒绝：带 `../` 的包不是"部分合法"，是恶意
        anyhow::ensure!(
            safe_rel_path(&f.path),
            "unsafe path in package: {:?}",
            f.path
        );
        map.insert(f.path, f.data);
    }
    Ok(Page {
        id,
        manifest,
        files: map,
    })
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

/// First 4 bytes of SHA-256 over a canonical encoding of the file map.
///
/// Length prefixes keep the encoding unambiguous (`{"ab","c"}` vs `{"a","bc"}`);
/// 32 bits keep the stub layout. A full digest belongs in the (future) package
/// signature — this id only has to make accidental aliasing implausible.
fn content_id(files: &FileMap) -> u32 {
    let mut hasher = Sha256::new();
    for entry in &files.0 {
        hasher.update((entry.path.len() as u64).to_be_bytes());
        hasher.update(entry.path.as_bytes());
        hasher.update((entry.data.len() as u64).to_be_bytes());
        hasher.update(&entry.data);
    }
    let digest = hasher.finalize();
    u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]])
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

    /// 小程序包是不可信输入：带越界路径的包必须**整包拒绝**。
    /// 这条测试就是那个 blocker 的回归防线。
    #[test]
    fn unpack_rejects_unsafe_paths() {
        let manifest = br#"{"name":"evil","main_js":"main.js","title":"x"}"#.to_vec();
        let bad_paths = [
            "../evil.txt",
            "/etc/passwd",
            "a/../../b",
            "C:/windows/system32/x",
            "sub/./x",
            "sub//x",
            "",
        ];
        for bad in bad_paths {
            let files = FileMap(vec![
                FileEntry {
                    path: "manifest.json".into(),
                    data: manifest.clone(),
                },
                FileEntry {
                    path: bad.to_string(),
                    data: b"pwned".to_vec(),
                },
            ]);
            let payload =
                zstd::stream::encode_all(bincode::serialize(&files).unwrap().as_slice(), 3)
                    .unwrap();
            let blob = format_blob(1, payload);
            assert!(
                unpack(&blob).is_err(),
                "必须拒绝越界路径 {bad:?}，否则能写到包外"
            );
        }
    }

    #[test]
    fn safe_rel_path_accepts_normal_paths() {
        for ok in ["manifest.json", "assets/app.js", "a/b/c.css"] {
            assert!(safe_rel_path(ok), "{ok} 应当合法");
        }
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

    /// issue #56 回归：旧实现用目录名做 FNV-1a，下面这对名字哈希相同。
    /// 现在 id 来自内容摘要，两个**内容不同**的包必须拿到不同 id。
    #[test]
    fn fnv_colliding_directory_names_no_longer_alias_packages() {
        fn legacy_name_id(name: &str) -> u32 {
            let mut h: u32 = 0x811c9dc5;
            for b in name.as_bytes() {
                h = h.wrapping_mul(0x01000193).wrapping_add(*b as u32);
            }
            h
        }
        const A: &str = "2dmh3n7d";
        const B: &str = "a3a779sz";
        assert_eq!(
            legacy_name_id(A),
            legacy_name_id(B),
            "回归用例失效：这对目录名不再碰撞"
        );

        let tmp =
            std::env::temp_dir().join(format!("wxlike-dpage-collision-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dir_a = tmp.join(A);
        let dir_b = tmp.join(B);
        make_demo_page(&dir_a);
        make_demo_page(&dir_b);
        // 内容必须不同，否则"同 id"反而是正确的内容寻址结果
        std::fs::write(dir_b.join("main.js"), b"wxhost.log('different page');").unwrap();

        let id_a = peek(&pack_dir(&dir_a).unwrap()).unwrap().1;
        let id_b = peek(&pack_dir(&dir_b).unwrap()).unwrap().1;
        assert_ne!(id_a, id_b, "目录名 FNV 碰撞不得让两个不同包共用一个 id");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn content_id_is_deterministic_and_name_independent() {
        let tmp = std::env::temp_dir().join(format!("wxlike-dpage-content-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let one = tmp.join("one");
        let two = tmp.join("two");
        make_demo_page(&one);
        make_demo_page(&two);

        let id_one_a = peek(&pack_dir(&one).unwrap()).unwrap().1;
        let id_one_b = peek(&pack_dir(&one).unwrap()).unwrap().1;
        let id_two = peek(&pack_dir(&two).unwrap()).unwrap().1;
        assert_eq!(id_one_a, id_one_b, "同目录重复打包 id 必须稳定");
        assert_eq!(id_one_a, id_two, "内容相同、目录名不同 → 同一个 id");

        std::fs::write(one.join("main.js"), b"wxhost.log('changed');").unwrap();
        let id_changed = peek(&pack_dir(&one).unwrap()).unwrap().1;
        assert_ne!(id_one_a, id_changed, "内容变化必须换 id");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// AR-4/H-5（第四轮审计）：解压炸弹必须被挡在分配之前。200 MiB 的零
    /// 压缩后只有几十 KiB——`unpack` 若不设上限，宿主进程直接 OOM。
    #[test]
    fn decompression_bomb_is_refused_before_allocating() {
        let bomb = vec![0u8; 200 * 1024 * 1024];
        let blob = format_blob(1, zstd::stream::encode_all(bomb.as_slice(), 6).unwrap());
        // the stub is tiny; the refusal is about the *decompressed* size
        let err = unpack(&blob).unwrap_err();
        assert!(
            err.to_string().contains("exceeds"),
            "expected the bomb cap, got: {err}"
        );
    }
}
