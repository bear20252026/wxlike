//! G17-04（解码面）：不可信输入的尺寸/深度/结构边界 + 变异 fuzz。
//!
//! 全部打真实实现：`wxlike-dpage` 的包解析、平台 HTTP 的 JSON 提取。
//! 恶意样本在这里**手工构造**（不依赖 `pack_dir` 的良性路径），因为攻击者
//! 不会帮我们按规范打包。
//!
//! 断言的口径统一是"**拒绝**，而不是崩溃/截断/静默成功"：
//! * `Err` 是拒绝；
//! * 4xx 是拒绝；
//! * 200 + 部分数据、panic、进程 abort 都不是。
//!
//! 已知崩溃样本（`corpus/manifest.json` 里 `verdict = blocked`）不参与"必须 Err"
//! 的断言，但必须带 blocker + issue —— 让"藏一个崩溃样本"变成一件做不到的事。

mod common;

use axum::http::StatusCode;
use serde_json::json;
use std::panic::{catch_unwind, AssertUnwindSafe};
use wxlike_dpage::{pack_dir, safe_rel_path, unpack, FileEntry, FileMap, MAGIC, STUB_LEN};

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-sec-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_page(dir: &std::path::Path, main_js: &str) {
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"name":"sec","main_js":"main.js","title":""}"#,
    )
    .unwrap();
    std::fs::write(dir.join("main.js"), main_js).unwrap();
}

/// FUZZ-001：截断/坏魔数的包必须拒绝，不能靠"读越界"碰运气。
#[test]
fn truncated_and_bad_magic_packages_are_rejected() {
    assert!(unpack(b"").is_err(), "空 blob 不应解出页面");
    assert!(unpack(&[0u8; 8]).is_err(), "短于 stub 的 blob 应当拒绝");

    let mut bad_magic = vec![0u8; STUB_LEN];
    bad_magic[..4].copy_from_slice(b"XXXX");
    assert!(unpack(&bad_magic).is_err(), "坏魔数应当拒绝");
    assert_ne!(&bad_magic[..4], MAGIC);
}

/// FUZZ-002：stub 声明的 payload 长度与实际不符时必须拒绝。
///
/// 用的是**不回绕**的小数字（7 vs 4），这样断言的是长度校验本身；
/// `u64::MAX` 那种会先撞上整数溢出的形态见 FUZZ-002b。
#[test]
fn payload_length_mismatch_is_rejected() {
    let mut blob = Vec::new();
    blob.extend_from_slice(MAGIC);
    blob.extend_from_slice(&1u32.to_be_bytes());
    blob.extend_from_slice(&7u32.to_be_bytes());
    blob.extend_from_slice(&7u64.to_be_bytes()); // 声称 7 字节
    blob.extend_from_slice(b"tiny"); // 实际 4 字节
    let err = unpack(&blob).expect_err("长度不符应当拒绝");
    assert!(
        format!("{err}").contains("length mismatch"),
        "错误信息应明确指出长度不符，实际：{err}"
    );
}

/// FUZZ-002b：#229 修复后的独立回归，恶意长度必须返回 Err，不能 panic。
#[test]
fn overflowing_payload_len_does_not_panic() {
    let mut blob = Vec::new();
    blob.extend_from_slice(MAGIC);
    blob.extend_from_slice(&1u32.to_be_bytes());
    blob.extend_from_slice(&7u32.to_be_bytes());
    blob.extend_from_slice(&u64::MAX.to_be_bytes());
    blob.extend_from_slice(b"tiny");
    let outcome = catch_unwind(AssertUnwindSafe(|| unpack(&blob)));
    match outcome {
        Ok(Err(_)) => {}
        Ok(Ok(_)) => panic!("声称 18 EB 的包居然解成功了"),
        Err(_) => panic!("20 字节的包把解析器打崩了（整数溢出），应为 Err"),
    }
}

/// FUZZ-003：解压炸弹。80 MiB 的可压缩数据必须被 64 MiB 上限挡住。
#[test]
fn decompression_bomb_is_rejected() {
    let dir = tmpdir("bomb");
    write_page(&dir, "console.log('ok')");
    // 80 MiB 的零：压缩后只有几十 KB，解压后超过 MAX_DECOMPRESSED
    std::fs::write(dir.join("bomb.bin"), vec![0u8; 80 * 1024 * 1024]).unwrap();
    let blob = pack_dir(&dir).expect("打包良性目录");
    let err = unpack(&blob).expect_err("超过 64 MiB 的包必须拒绝");
    assert!(
        format!("{err}").contains("decompression bomb") || format!("{err}").contains("exceeds"),
        "错误信息应指出解压上限，实际：{err}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// FUZZ-004：包内路径穿越（`../`、绝对路径、盘符、反斜杠）整包拒绝。
///
/// 手工构造 `FileMap` 再压成合法 blob——攻击者能造，测试也要能造。
#[test]
fn package_with_traversal_entry_is_rejected() {
    for evil in [
        "../evil.js",
        "/etc/passwd",
        "C:/windows/x",
        "a\\b.js",
        "a/../../b",
    ] {
        let files = FileMap(vec![
            FileEntry {
                path: "manifest.json".into(),
                data: br#"{"name":"x","main_js":"main.js","title":""}"#.to_vec(),
            },
            FileEntry {
                path: "main.js".into(),
                data: b"console.log(1)".to_vec(),
            },
            FileEntry {
                path: evil.into(),
                data: b"pwn".to_vec(),
            },
        ]);
        let blob = craft_blob(&files);
        assert!(
            unpack(&blob).is_err(),
            "含穿越路径 {evil:?} 的包必须整包拒绝（不能只跳过这一条）"
        );
    }
}

/// FUZZ-005：`safe_rel_path` 的判定表（与包解析共用同一套标准）。
#[test]
fn safe_rel_path_matrix() {
    let ok = [
        "main.js",
        "a/b.js",
        "deep/nested/path/file.bin",
        "a-b_c.1.js",
    ];
    let bad = [
        "", ".", "..", "../x", "a/../b", "/abs", "C:/x", "a\\b", "a//b", "a/./b", "./a",
    ];
    for p in ok {
        assert!(safe_rel_path(p), "{p:?} 应当判为安全");
    }
    for p in bad {
        assert!(!safe_rel_path(p), "{p:?} 应当判为不安全");
    }
}

/// FUZZ-006：深度炸弹。10 万层嵌套的 JSON 必须 4xx，且服务仍可用。
///
/// 关键是最后一步：处理完恶意请求后再打一次正常请求，证明没有把进程搞坏
/// （panic 在 axum 里会变成 500 并可能带走 worker）。
#[tokio::test]
async fn deep_json_body_is_rejected_and_server_survives() {
    let app = common::app(common::platform());
    let depth = 100_000;
    let mut body = String::with_capacity(depth * 2 + 64);
    body.push_str(r#"{"name":"#);
    body.push_str(&"[".repeat(depth));
    body.push_str(&"]".repeat(depth));
    body.push_str(r#","password":"pw-123456"}"#);

    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/api/register")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body))
        .unwrap();
    let (status, resp) = common::call(&app, req).await;
    assert!(
        status.is_client_error(),
        "深层嵌套 body 应当是 4xx，实际 {status} {resp}"
    );

    // 同一个 app 实例继续服务
    let (uid, who) = common::signup(&app, "after-depth-bomb").await;
    assert!(!uid.is_empty() && !who.is_empty());
}

/// FUZZ-007：超大字段。2 MiB 的昵称必须 4xx（有上限），不能进库。
#[tokio::test]
async fn oversized_field_is_rejected() {
    let app = common::app(common::platform());
    let big = "a".repeat(2 * 1024 * 1024);
    let (status, resp) = common::call(
        &app,
        common::req(
            "POST",
            "/api/register",
            Some(json!({"name": big, "password": "pw-123456"})),
            None,
        ),
    )
    .await;
    assert!(
        status.is_client_error(),
        "超长昵称应当 4xx，实际 {status} {resp}"
    );
    assert_ne!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

/// FUZZ-008：语料回放。规则由 `corpus/manifest.json` 决定：
///
/// * `reject`  → 必须 `Err`；
/// * `blocked` → 当前会 panic，必须带 blocker + issue 才能留在语料里。
///
/// 这样"样本跑不过就悄悄删掉"或"改样本让它通过"都行不通：manifest 是审查对象。
#[test]
fn replay_minimized_corpus() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("manifest.json")).expect("corpus/manifest.json 必须存在"),
    )
    .expect("manifest 必须是合法 JSON");
    let samples = manifest["samples"].as_array().expect("samples 必须是数组");
    assert!(!samples.is_empty(), "语料为空：这条用例什么都没验");

    let mut replayed = 0;
    for sample in samples {
        let file = sample["file"].as_str().expect("file");
        let verdict = sample["verdict"].as_str().expect("verdict");
        let bytes = decode_hex(&std::fs::read_to_string(dir.join(file)).expect("样本文件必须存在"));
        match verdict {
            "reject" => {
                assert!(
                    unpack(&bytes).is_err(),
                    "语料 {file} 被标为 reject，却解析成功了"
                );
                replayed += 1;
            }
            "blocked" => {
                assert!(
                    sample["blocker"].is_string() && sample["issue"].is_string(),
                    "语料 {file} 被标为 blocked，但没有 blocker + issue：崩溃样本必须可追责"
                );
            }
            other => panic!("语料 {file} 的 verdict 非法：{other}"),
        }
    }
    assert!(
        replayed >= 3,
        "reject 样本太少（{replayed}），语料可能被删空"
    );
}

/// FUZZ-009：固定种子变异 fuzz，任何 panic 都必须失败，不再豁免 overflow。
/// 默认 2000 次；夜间任务用 WXLIKE_FUZZ_ITERS 扩大样本量。
#[test]
fn mutation_fuzz_never_panics() {
    let iterations: usize = std::env::var("WXLIKE_FUZZ_ITERS")
        .map(|value| value.parse().expect("WXLIKE_FUZZ_ITERS 必须是整数"))
        .unwrap_or(2_000);
    assert!(
        (1_000..=200_000).contains(&iterations),
        "fuzz 次数必须在 1000..=200000"
    );
    let dir = tmpdir("mutbase");
    write_page(&dir, "console.log('mutation baseline')");
    let baseline = pack_dir(&dir).expect("打包良性目录");
    std::fs::remove_dir_all(&dir).ok();

    let mut rng = XorShift64::new(0x5EED_1234_ABCD_0001);
    let mut errors = 0usize;
    for iteration in 0..iterations {
        let mut blob = baseline.clone();
        for _ in 0..(1 + rng.next() % 3) {
            let idx = (rng.next() as usize) % blob.len();
            blob[idx] ^= (rng.next() % 256) as u8;
        }
        match catch_unwind(AssertUnwindSafe(|| unpack(&blob))) {
            Ok(Err(_)) => errors += 1,
            Ok(Ok(_)) => {} // 变异未破坏结构，解析成功也正常
            Err(_) => {
                let path = std::env::var_os("WXLIKE_FUZZ_CRASH_DIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| tmpdir("mutation-crash"));
                std::fs::create_dir_all(&path).expect("创建崩溃样本目录");
                let sample = path.join(format!("mutation-{iteration}.hex"));
                std::fs::write(&sample, hex::encode(&blob)).expect("保存可复现崩溃输入");
                panic!(
                    "变异 fuzz 第 {iteration} 次触发 panic，样本：{}",
                    sample.display()
                );
            }
        }
    }
    assert!(
        errors > iterations / 20,
        "仅 {errors}/{iterations} 个样本被拒绝"
    );
}

/// xorshift64*：确定性、无依赖。种子固定 → 失败可复现。
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn decode_hex(text: &str) -> Vec<u8> {
    let digits: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    digits
        .as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

/// 把任意 `FileMap` 压成带合法 stub 的 blob（只给测试造恶意包用）。
fn craft_blob(files: &FileMap) -> Vec<u8> {
    let payload = zstd_compress(&bincode_encode(files));
    let mut out = Vec::with_capacity(STUB_LEN + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

fn bincode_encode(files: &FileMap) -> Vec<u8> {
    // 与 dpage 使用同一套 bincode 1.x 默认配置（fixint + 小端 + u64 长度前缀）。
    // 这里手写序列化，避免为测试给 dpage 加一个 bincode 公开 API。
    let mut out = Vec::new();
    out.extend_from_slice(&(files.0.len() as u64).to_le_bytes());
    for f in &files.0 {
        out.extend_from_slice(&(f.path.len() as u64).to_le_bytes());
        out.extend_from_slice(f.path.as_bytes());
        out.extend_from_slice(&(f.data.len() as u64).to_le_bytes());
        out.extend_from_slice(&f.data);
    }
    out
}

fn zstd_compress(data: &[u8]) -> Vec<u8> {
    zstd::stream::encode_all(data, 6).expect("zstd 压缩")
}
