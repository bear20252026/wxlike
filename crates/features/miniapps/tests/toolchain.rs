//! G08-05 acceptance: `mini pack/sign/inspect/dev-serve` round-trips a real
//! package and refuses what it must.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use wxlike_miniapps::packages::{generate_key, issue_cert, PublisherCert};
use wxlike_miniapps::toolchain::{
    self, load_template, pack, sign, signing_key, trust_store, DevServer, TrustSnapshot,
};

const HOST: &str = "0.1.0";
const PUBLISHER: &str = "wxlike-labs";
const KEY_ID: &str = "pub-1";
const EXAMPLE: &str = "notes";
const SEED_HEX: &str = "3d1e0b0a9c8f7e6d5c4b3a29180f2e1d0c9b8a7968574635241302f1e0d9c8b7";

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../packages/mini-sdk/examples")
}

fn unique(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "wxlike-g08-cli-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Root trust snapshot plus the publisher public key (the cert subject).
fn snapshot() -> (TrustSnapshot, String) {
    let root = generate_key();
    let signer = signing_key(SEED_HEX).unwrap();
    let publisher_key = hex::encode(signer.verifying_key().as_bytes());
    let cert = PublisherCert {
        publisher: PUBLISHER.into(),
        key_id: KEY_ID.into(),
        public_key: publisher_key.clone(),
        not_before: 0,
        not_after: u64::MAX,
    };
    let signed = issue_cert("root-1", &root, cert).unwrap();
    let snapshot = TrustSnapshot {
        root_key_id: "root-1".into(),
        root_public_key: hex::encode(root.verifying_key().as_bytes()),
        certs: vec![signed],
        revoked_keys: vec![],
        revoked_publishers: vec![],
    };
    (snapshot, publisher_key)
}

#[test]
fn pack_sign_inspect_roundtrip_verifies_and_reports_unverified() {
    let dir = unique("roundtrip");
    std::fs::create_dir_all(&dir).unwrap();
    let blob = dir.join("notes.dpage");
    let sig = dir.join("notes.sig");
    let (snapshot, publisher_key) = snapshot();

    let report = pack(&examples_dir().join(EXAMPLE), None, Some(&blob), HOST).unwrap();
    assert_eq!(report.app_id, "com.wxlike.demo.notes");
    assert_eq!(report.channel, "dev");
    assert!(report.files >= 1);
    assert!(blob.is_file());

    let key = signing_key(SEED_HEX).unwrap();
    let signed = sign(&blob, &key, Some(KEY_ID), Some(&blob), Some(&sig), HOST).unwrap();
    assert_eq!(signed.digest, report.digest);
    assert!(sig.is_file());

    let store = trust_store(&snapshot).unwrap();
    let verified = toolchain::inspect(&blob, Some(&sig), Some(&store), None, HOST).unwrap();
    assert_eq!(verified.signature, "verified");
    assert_eq!(verified.signer.as_deref(), Some("wxlike-labs/pub-1"));
    assert_eq!(verified.capabilities, vec!["storage".to_string()]);

    let bare = toolchain::inspect(&blob, None, None, None, HOST).unwrap();
    assert_eq!(bare.signature, "unverified");

    let keyed = toolchain::inspect(&blob, Some(&sig), None, Some(&publisher_key), HOST).unwrap();
    assert_eq!(keyed.signature, "verified");
}

#[test]
fn foreign_signature_and_broken_templates_are_rejected() {
    let dir = unique("reject");
    std::fs::create_dir_all(&dir).unwrap();
    let (_, publisher_key) = snapshot();

    let notes_blob = dir.join("notes.dpage");
    let notes_sig = dir.join("notes.sig");
    pack(&examples_dir().join(EXAMPLE), None, Some(&notes_blob), HOST).unwrap();
    let key = signing_key(SEED_HEX).unwrap();
    sign(
        &notes_blob,
        &key,
        Some(KEY_ID),
        Some(&notes_blob),
        Some(&notes_sig),
        HOST,
    )
    .unwrap();

    // The notes signature over a different package must not verify.
    let game_blob = dir.join("game.dpage");
    pack(&examples_dir().join("game"), None, Some(&game_blob), HOST).unwrap();
    let forged = toolchain::inspect(
        &game_blob,
        Some(&notes_sig),
        None,
        Some(&publisher_key),
        HOST,
    );
    assert!(forged.is_err());

    // A tampered template (unknown capability) must fail at pack time.
    let broken = dir.join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::copy(
        examples_dir().join(EXAMPLE).join("index.js"),
        broken.join("index.js"),
    )
    .unwrap();
    let mut bad_template = load_template(&examples_dir().join(EXAMPLE), None).unwrap();
    bad_template.capabilities.push("root".into());
    std::fs::write(
        broken.join("mini.template.json"),
        serde_json::to_vec(&bad_template).unwrap(),
    )
    .unwrap();
    assert!(pack(&broken, None, Some(&dir.join("broken.dpage")), HOST).is_err());

    assert!(signing_key("nothex").is_err());
    assert!(signing_key(&"ab".repeat(31)).is_err());
}

#[test]
fn dev_serve_only_serves_sealed_files() {
    let server = DevServer::bind(&examples_dir().join(EXAMPLE), None, HOST, 0).unwrap();
    assert!(server.url().starts_with("http://127.0.0.1:"));
    assert_eq!(server.manifest().channel.as_str(), "dev");

    let entry = fetch(&server, server.manifest().entry.clone());
    assert!(entry.starts_with("HTTP/1.1 200 OK"));
    assert!(entry.contains("text/javascript"));
    assert!(entry.contains("notes.v1"));

    let manifest_bytes = fetch(&server, "mini.json".into());
    assert!(manifest_bytes.contains("com.wxlike.demo.notes"));

    let missing = fetch(&server, "../../Cargo.toml".into());
    assert!(missing.starts_with("HTTP/1.1 404 Not Found"));

    let bad = fetch(&server, "missing.js".into());
    assert!(bad.starts_with("HTTP/1.1 404 Not Found"));
}

fn fetch(server: &DevServer, path: String) -> String {
    let url = server.url().to_string();
    let body = std::thread::scope(|scope| {
        let worker = scope.spawn(|| server.serve_once());
        std::thread::sleep(std::time::Duration::from_millis(120));
        let mut stream = std::net::TcpStream::connect(url.trim_start_matches("http://")).unwrap();
        stream
            .write_all(format!("GET /{path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .unwrap();
        let mut body = String::new();
        Read::read_to_string(&mut stream, &mut body).unwrap();
        worker.join().unwrap().unwrap();
        body
    });
    body
}

#[test]
fn cli_binary_pack_sign_inspect_and_dev_serve() {
    let exe = env!("CARGO_BIN_EXE_mini");
    let dir = unique("cli");
    std::fs::create_dir_all(&dir).unwrap();
    let blob = dir.join("notes.dpage");
    let sig = dir.join("notes.sig");
    let trust = write_trust(&dir);

    let blob_arg = blob.to_string_lossy().into_owned();
    let sig_arg = sig.to_string_lossy().into_owned();
    let dir_arg = examples_dir().join(EXAMPLE).to_string_lossy().into_owned();
    let trust_arg = trust.to_string_lossy().into_owned();

    let (ok, stdout, stderr) = run(exe, &["pack", &dir_arg, "--out", &blob_arg]);
    assert!(ok, "pack failed: {stderr}");
    assert!(stdout.contains("com.wxlike.demo.notes"));

    let (ok, _stdout, stderr) = run(
        exe,
        &[
            "sign", &blob_arg, "--key", SEED_HEX, "--key-id", KEY_ID, "--sig", &sig_arg,
        ],
    );
    assert!(ok, "sign failed: {stderr}");
    assert!(sig.is_file());

    let (ok, stdout, stderr) = run(
        exe,
        &[
            "inspect", &blob_arg, "--sig", &sig_arg, "--trust", &trust_arg,
        ],
    );
    assert!(ok, "inspect failed: {stderr}");
    assert!(stdout.contains("\"signature\": \"verified\""), "{stdout}");

    let (ok, _stdout, stderr) = run(exe, &["inspect", &blob_arg, "--sig", &sig_arg]);
    assert!(!ok, "a signature without trust material must fail");
    assert!(stderr.contains("needs --trust or --public-key"));

    let (ok, stdout, _stderr) = run(exe, &["--help"]);
    assert!(ok && stdout.contains("mini pack"));

    let (ok, _stdout, stderr) = run(exe, &["bogus"]);
    assert!(!ok && stderr.contains("unknown command"));

    // `dev-serve --once` prints its URL, serves one request and exits.
    let mut child = Command::new(exe)
        .args(["dev-serve", &dir_arg, "--once"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let info: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    let url = info["url"].as_str().unwrap().to_string();
    // `--once` serves exactly one request before exiting, so give it one.
    let mut stream = std::net::TcpStream::connect(url.trim_start_matches("http://")).unwrap();
    stream
        .write_all(b"GET /index.js HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut served = String::new();
    Read::read_to_string(&mut stream, &mut served).unwrap();
    assert!(served.starts_with("HTTP/1.1 200 OK"));
    let status = child.wait().unwrap();
    assert!(status.success(), "dev-serve did not exit cleanly");
    assert!(url.starts_with("http://127.0.0.1:"));
}

fn run(exe: &str, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(exe).args(args).output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn write_trust(dir: &Path) -> PathBuf {
    let (snapshot, _) = snapshot();
    let path = dir.join("trust.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&snapshot).unwrap()).unwrap();
    path
}
