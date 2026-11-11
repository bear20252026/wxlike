//! G08-01 acceptance: signed package supply chain and crash-safe install.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::SigningKey;
use wxlike_dpage::{pack_files, unpack_raw, FileEntry, FileMap};
use wxlike_miniapps::packages::manifest::{digest_hex, MAX_APP_ID_LEN, MAX_NAME_CHARS};
use wxlike_miniapps::packages::{
    build_signed, generate_key, issue_cert, open, pack_unsigned, sign_blob, sign_manifest, verify,
    Channel, FaultPoint, Faults, InstallOutcome, InstalledApp, Installer, Manifest, MiniError,
    PackageSignature, PublisherCert, SignedCert, SignedPackage, TrustStore, COMPLETE_MARKER,
    MANIFEST_FILE, MAX_MANIFEST_BYTES, MAX_PACKAGE_BYTES, MAX_RESOURCES, MAX_RESOURCE_BYTES,
    PIN_FILE,
};

const NOW: u64 = 1_800_000_000;
const HOST: &str = "0.1.0";
const PUBLISHER: &str = "wxlike-labs";
const KEY_ID: &str = "pub-1";
const APP_ID: &str = "com.wxlike.notes";

struct Fixture {
    base: PathBuf,
    source: PathBuf,
    trust: TrustStore,
    signer: SigningKey,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let base = unique(name);
        let source = base.join("source");
        std::fs::create_dir_all(&source).unwrap();
        let root = generate_key();
        let signer = generate_key();
        let cert = PublisherCert {
            publisher: PUBLISHER.into(),
            key_id: KEY_ID.into(),
            public_key: hex::encode(signer.verifying_key().as_bytes()),
            not_before: 0,
            not_after: u64::MAX,
        };
        let signed = issue_cert("root-1", &root, cert).unwrap();
        let mut trust = TrustStore::new("root-1", root.verifying_key());
        trust.enroll(&signed, NOW).unwrap();
        Self {
            base,
            source,
            trust,
            signer,
        }
    }

    fn source(&self, version: &str, body: &str) {
        let assets = self.source.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(
            self.source.join("index.js"),
            format!("onLoad(function(){{ console.log('{version}'); {body} }});"),
        )
        .unwrap();
        std::fs::write(assets.join("app.js"), "console.log('asset');").unwrap();
    }

    fn build(&self, version: &str, channel: Channel, caps: &[&str]) -> SignedPackage {
        self.build_with(version, channel, caps, &format!("v{version}"))
    }

    fn build_with(
        &self,
        version: &str,
        channel: Channel,
        caps: &[&str],
        body: &str,
    ) -> SignedPackage {
        self.source(version, body);
        let template = Manifest {
            app_id: APP_ID.into(),
            name: "笔记".into(),
            version: version.into(),
            min_host: "0.1.0".into(),
            entry: "index.js".into(),
            channel,
            publisher: PUBLISHER.into(),
            capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
            resources: BTreeMap::new(),
        };
        build_signed(&self.source, &template, HOST, &self.signer, KEY_ID).unwrap()
    }

    fn installer(&self, channel: Channel) -> Installer {
        Installer::new(&self.base, channel, self.trust.clone(), HOST)
    }
}

fn unique(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    use std::time::{SystemTime, UNIX_EPOCH};
    // The nanosecond stamp is load-bearing: installers keep state on disk
    // (watermarks, version slots), and a CI rerun on the same runner can
    // recycle both the pid and the counter. Reusing a previous run's root
    // would inherit its watermarks and fail the downgrade tests for a reason
    // that has nothing to do with the code under test.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "wxlike-g08-{name}-{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    // Belt and braces: never start from a directory somebody else left behind.
    let _ = std::fs::remove_dir_all(&path);
    path
}

fn rebuild(blob: &[u8], mutate: impl FnOnce(&mut BTreeMap<String, Vec<u8>>)) -> Vec<u8> {
    let mut files = unpack_raw(blob).unwrap().files;
    mutate(&mut files);
    let map = FileMap(
        files
            .into_iter()
            .map(|(path, data)| FileEntry { path, data })
            .collect(),
    );
    pack_files(map).unwrap()
}

fn installed(outcome: InstallOutcome) -> InstalledApp {
    match outcome {
        InstallOutcome::Installed(app) | InstallOutcome::AlreadyInstalled(app) => app,
    }
}

#[test]
fn signed_package_installs_and_retries_idempotently() {
    let fx = Fixture::new("install");
    let pkg = fx.build("1.0.0", Channel::Prod, &["storage", "network"]);
    let mut inst = fx.installer(Channel::Prod);

    let app = installed(inst.install(&pkg.blob, &pkg.signature, NOW).unwrap());
    assert_eq!(app.app_id, APP_ID);
    assert_eq!(app.version, "1.0.0");
    assert!(app.path.join("index.js").exists());
    assert_eq!(app.capabilities, vec!["storage", "network"]);
    assert_eq!(inst.watermark(APP_ID).unwrap().as_deref(), Some("1.0.0"));

    let retry = inst.install(&pkg.blob, &pkg.signature, NOW).unwrap();
    assert!(matches!(retry, InstallOutcome::AlreadyInstalled(_)));
}

#[test]
fn tampered_missing_or_extra_resources_are_rejected() {
    let fx = Fixture::new("resources");
    let pkg = fx.build("1.0.0", Channel::Prod, &[]);
    let mut inst = fx.installer(Channel::Prod);

    let tampered = rebuild(&pkg.blob, |files| {
        files
            .get_mut("index.js")
            .unwrap()
            .extend_from_slice(b"\n// injected");
    });
    assert!(matches!(
        inst.install(&tampered, &pkg.signature, NOW).unwrap_err(),
        MiniError::DigestMismatch(path) if path == "index.js"
    ));

    let missing = rebuild(&pkg.blob, |files| {
        files.remove("assets/app.js");
    });
    assert!(matches!(
        inst.install(&missing, &pkg.signature, NOW).unwrap_err(),
        MiniError::MissingResource(path) if path == "assets/app.js"
    ));

    let extra = rebuild(&pkg.blob, |files| {
        files.insert("payload.sh".into(), b"echo pwned".to_vec());
    });
    assert!(matches!(
        inst.install(&extra, &pkg.signature, NOW).unwrap_err(),
        MiniError::ExtraResource(path) if path == "payload.sh"
    ));
}

#[test]
fn tampered_manifest_breaks_the_signature() {
    let fx = Fixture::new("signature");
    let pkg = fx.build("1.0.0", Channel::Prod, &[]);
    let evil = rebuild(&pkg.blob, |files| {
        let mut value: serde_json::Value =
            serde_json::from_slice(files.get("mini.json").unwrap()).unwrap();
        value["name"] = "被篡改的应用".into();
        files.insert("mini.json".into(), serde_json::to_vec(&value).unwrap());
    });
    let mut inst = fx.installer(Channel::Prod);
    assert!(matches!(
        inst.install(&evil, &pkg.signature, NOW).unwrap_err(),
        MiniError::BadSigning
    ));
}

#[test]
fn expired_or_revoked_publisher_keys_are_rejected() {
    let fx = Fixture::new("trust");
    let pkg = fx.build("1.0.0", Channel::Prod, &[]);

    let root = generate_key();
    let cert = PublisherCert {
        publisher: PUBLISHER.into(),
        key_id: KEY_ID.into(),
        public_key: hex::encode(fx.signer.verifying_key().as_bytes()),
        not_before: 0,
        not_after: NOW - 1,
    };
    let mut expired_trust = TrustStore::new("root-1", root.verifying_key());
    expired_trust
        .enroll(&issue_cert("root-1", &root, cert).unwrap(), NOW - 100)
        .unwrap();
    let mut expired = Installer::new(&fx.base, Channel::Prod, expired_trust, HOST);
    assert!(matches!(
        expired.install(&pkg.blob, &pkg.signature, NOW).unwrap_err(),
        MiniError::CertExpired
    ));

    let mut revoked_trust = fx.trust.clone();
    revoked_trust.revoke_key(KEY_ID);
    let mut revoked = Installer::new(&fx.base, Channel::Prod, revoked_trust, HOST);
    assert!(matches!(
        revoked.install(&pkg.blob, &pkg.signature, NOW).unwrap_err(),
        MiniError::Revoked
    ));
}

#[test]
fn certificate_must_match_the_manifest_publisher() {
    let fx = Fixture::new("publisher");
    let pkg = fx.build("1.0.0", Channel::Prod, &[]);
    let root = generate_key();
    let cert = PublisherCert {
        publisher: "someone-else".into(),
        key_id: KEY_ID.into(),
        public_key: hex::encode(fx.signer.verifying_key().as_bytes()),
        not_before: 0,
        not_after: u64::MAX,
    };
    let mut trust = TrustStore::new("root-1", root.verifying_key());
    trust
        .enroll(&issue_cert("root-1", &root, cert).unwrap(), NOW)
        .unwrap();
    let mut inst = Installer::new(&fx.base, Channel::Prod, trust, HOST);
    assert!(matches!(
        inst.install(&pkg.blob, &pkg.signature, NOW).unwrap_err(),
        MiniError::PublisherMismatch
    ));
}

#[test]
fn downgrade_and_same_version_conflict_are_rejected() {
    let fx = Fixture::new("versions");
    let v1 = fx.build("1.0.0", Channel::Prod, &[]);
    let v2 = fx.build("1.1.0", Channel::Prod, &[]);
    let mut inst = fx.installer(Channel::Prod);
    inst.install(&v1.blob, &v1.signature, NOW).unwrap();
    inst.install(&v2.blob, &v2.signature, NOW).unwrap();

    let err = inst.install(&v1.blob, &v1.signature, NOW).unwrap_err();
    assert!(matches!(
        err,
        MiniError::Downgrade(app, version, floor)
            if app == APP_ID && version == "1.0.0" && floor == "1.1.0"
    ));

    let same_version_new_digest = fx.build_with("1.1.0", Channel::Prod, &[], "different body");
    assert!(matches!(
        inst.install(
            &same_version_new_digest.blob,
            &same_version_new_digest.signature,
            NOW
        )
        .unwrap_err(),
        MiniError::VersionConflict(_, _)
    ));
}

#[test]
fn channels_and_min_host_are_enforced() {
    let fx = Fixture::new("channel");
    let dev = fx.build("1.0.0", Channel::Dev, &[]);
    let mut prod = fx.installer(Channel::Prod);
    assert!(matches!(
        prod.install(&dev.blob, &dev.signature, NOW).unwrap_err(),
        MiniError::WrongChannel(_, _)
    ));

    let mut dev_root = fx.installer(Channel::Dev);
    dev_root.install(&dev.blob, &dev.signature, NOW).unwrap();

    let too_new = rebuild(&dev.blob, |files| {
        let mut value: serde_json::Value =
            serde_json::from_slice(files.get("mini.json").unwrap()).unwrap();
        value["min_host"] = "9.0.0".into();
        files.insert("mini.json".into(), serde_json::to_vec(&value).unwrap());
    });
    assert!(matches!(
        sign_blob(&too_new, HOST, &fx.signer, KEY_ID).unwrap_err(),
        MiniError::HostTooOld(_, _)
    ));
}

struct TripAt(FaultPoint);

impl Faults for TripAt {
    fn trip(&mut self, point: FaultPoint) -> Result<(), MiniError> {
        if point == self.0 {
            Err(MiniError::InjectedFault("test"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn interrupted_update_keeps_the_previous_runnable_version() {
    let fx = Fixture::new("fault");
    let v1 = fx.build("1.0.0", Channel::Prod, &[]);
    let v2 = fx.build("1.1.0", Channel::Prod, &[]);
    let mut inst = fx.installer(Channel::Prod);
    inst.install(&v1.blob, &v1.signature, NOW).unwrap();

    let mut interrupted = fx
        .installer(Channel::Prod)
        .with_faults(Box::new(TripAt(FaultPoint::BeforeActivate)));
    assert!(matches!(
        interrupted
            .install(&v2.blob, &v2.signature, NOW)
            .unwrap_err(),
        MiniError::InjectedFault(_)
    ));
    let current = interrupted.current(APP_ID).unwrap();
    assert_eq!(current.version, "1.0.0");
    assert!(current.path.join("index.js").exists());
    assert_eq!(interrupted.recover().unwrap(), 1);
    assert_eq!(interrupted.current(APP_ID).unwrap().version, "1.0.0");
}

#[test]
fn rollback_pins_previous_version_without_lowering_the_watermark() {
    let fx = Fixture::new("rollback");
    let v1 = fx.build("1.0.0", Channel::Prod, &[]);
    let v2 = fx.build("1.1.0", Channel::Prod, &[]);
    let mut inst = fx.installer(Channel::Prod);
    inst.install(&v1.blob, &v1.signature, NOW).unwrap();
    inst.install(&v2.blob, &v2.signature, NOW).unwrap();

    // While v2 is active, replaying the old signed package is a downgrade.
    assert!(matches!(
        inst.install(&v1.blob, &v1.signature, NOW).unwrap_err(),
        MiniError::Downgrade(_, _, _)
    ));

    let rolled = inst.rollback(APP_ID).unwrap();
    assert_eq!(rolled.version, "1.0.0");
    assert_eq!(inst.current(APP_ID).unwrap().version, "1.0.0");
    // The pin makes v1 active, but it is still not a fresh install.
    assert!(matches!(
        inst.install(&v1.blob, &v1.signature, NOW).unwrap(),
        InstallOutcome::AlreadyInstalled(_)
    ));

    // Reinstalling the current newest release clears the pin.
    inst.install(&v2.blob, &v2.signature, NOW).unwrap();
    assert_eq!(inst.current(APP_ID).unwrap().version, "1.1.0");
}

#[test]
fn uninstall_removes_files_and_current_state() {
    let fx = Fixture::new("uninstall");
    let pkg = fx.build("1.0.0", Channel::Prod, &[]);
    let mut inst = fx.installer(Channel::Prod);
    let app = installed(inst.install(&pkg.blob, &pkg.signature, NOW).unwrap());
    let path = app.path;
    assert!(inst.remove(APP_ID).unwrap());
    assert!(!path.exists());
    assert!(matches!(
        inst.current(APP_ID).unwrap_err(),
        MiniError::NotInstalled(_)
    ));
    assert!(!inst.remove(APP_ID).unwrap());
}

#[test]
fn recover_drops_incomplete_version_directories() {
    let fx = Fixture::new("recover");
    let pkg = fx.build("1.0.0", Channel::Prod, &[]);
    let mut inst = fx.installer(Channel::Prod);
    inst.install(&pkg.blob, &pkg.signature, NOW).unwrap();
    let orphan = fx
        .base
        .join("prod")
        .join("apps")
        .join(APP_ID)
        .join("v9.9.9-deadbeef");
    std::fs::create_dir_all(&orphan).unwrap();
    assert_eq!(inst.recover().unwrap(), 1);
    assert!(!orphan.exists());
    assert_eq!(inst.current(APP_ID).unwrap().version, "1.0.0");
}

#[cfg(unix)]
#[test]
fn symlinked_source_files_are_refused() {
    use std::os::unix::fs::symlink;
    let fx = Fixture::new("symlink");
    fx.source("1.0.0", "ok");
    let outside = fx.base.join("outside.txt");
    std::fs::write(&outside, b"secret").unwrap();
    symlink(&outside, fx.source.join("leak.txt")).unwrap();
    let template = Manifest {
        app_id: APP_ID.into(),
        name: "notes".into(),
        version: "1.0.0".into(),
        min_host: "0.1.0".into(),
        entry: "index.js".into(),
        channel: Channel::Prod,
        publisher: PUBLISHER.into(),
        capabilities: vec![],
        resources: BTreeMap::new(),
    };
    assert!(build_signed(&fx.source, &template, HOST, &fx.signer, KEY_ID).is_err());
}

/// Build a `template` manifest without going through `seal_dir`, so a test can
/// describe a package the packer would never produce (hostile digests,
/// over-budget resource maps, reserved names).
fn template(app_id: &str, version: &str, channel: Channel) -> Manifest {
    Manifest {
        app_id: app_id.into(),
        name: "笔记".into(),
        version: version.into(),
        min_host: "0.1.0".into(),
        entry: "index.js".into(),
        channel,
        publisher: PUBLISHER.into(),
        capabilities: vec!["storage".into()],
        resources: BTreeMap::from([("index.js".into(), digest_hex(b"body"))]),
    }
}

/// Seal `files` into a blob and sign the embedded `mini.json` with the
/// publisher key. This is how a test describes a package `mini pack` refuses
/// to build; the real packer is covered by `build_signed`.
fn craft(
    fx: &Fixture,
    manifest: &Manifest,
    files: BTreeMap<String, Vec<u8>>,
) -> (Vec<u8>, PackageSignature) {
    let manifest_bytes = serde_json::to_vec(manifest).unwrap();
    let mut entries: Vec<FileEntry> = files
        .into_iter()
        .map(|(path, data)| FileEntry { path, data })
        .collect();
    entries.push(FileEntry {
        path: MANIFEST_FILE.into(),
        data: manifest_bytes,
    });
    let blob = pack_files(FileMap(entries)).unwrap();
    let signature = sign_manifest(
        &unpack_raw(&blob).unwrap().files[MANIFEST_FILE],
        &fx.signer,
        KEY_ID,
    );
    (blob, signature)
}

#[test]
fn keys_outside_the_trust_store_are_rejected() {
    let fx = Fixture::new("foreign-key");
    let pkg = fx.build("1.0.0", Channel::Prod, &[]);

    // 1. The sidecar names a key that was never enrolled.
    let foreign = PackageSignature {
        key_id: "pub-from-another-world".into(),
        sig: pkg.signature.sig.clone(),
    };
    assert!(matches!(
        verify(&pkg.blob, &foreign, &fx.trust, NOW, HOST).unwrap_err(),
        MiniError::UntrustedPublisher
    ));

    // 2. A certificate signed by a *different* root never enrolls, and a
    //    store anchored on that other root cannot vouch for our package.
    let other_root = generate_key();
    let cert = PublisherCert {
        publisher: PUBLISHER.into(),
        key_id: KEY_ID.into(),
        public_key: hex::encode(fx.signer.verifying_key().as_bytes()),
        not_before: 0,
        not_after: u64::MAX,
    };
    let foreign_cert = issue_cert("root-evil", &other_root, cert.clone()).unwrap();
    let mut strict = TrustStore::new("root-1", other_root.verifying_key());
    assert!(matches!(
        strict.enroll(&foreign_cert, NOW).unwrap_err(),
        MiniError::UntrustedPublisher
    ));
    let nothing_enrolled = TrustStore::new("root-evil", other_root.verifying_key());
    assert!(matches!(
        verify(&pkg.blob, &pkg.signature, &nothing_enrolled, NOW, HOST).unwrap_err(),
        MiniError::UntrustedPublisher
    ));

    // 3. A self-issued cert that copies a real 64-byte signature still fails:
    //    enrollment verifies the *root* signature over the canonical cert.
    let forged = SignedCert {
        root_key_id: "root-1".into(),
        cert,
        sig: pkg.signature.sig.clone(),
    };
    let mut store = fx.trust.clone();
    assert!(matches!(
        store.enroll(&forged, NOW).unwrap_err(),
        MiniError::BadSigning | MiniError::BadSignature
    ));

    // 4. A malformed signature hex is rejected before any crypto runs.
    let mut junk = pkg.signature.clone();
    junk.sig = "abcd".into();
    assert!(matches!(
        verify(&pkg.blob, &junk, &fx.trust, NOW, HOST).unwrap_err(),
        MiniError::BadSignature
    ));
}

#[test]
fn a_verification_proof_is_bound_to_one_trust_store_and_one_clock() {
    let fx = Fixture::new("proof");
    let pkg = fx.build("1.0.0", Channel::Prod, &[]);
    let verified = verify(&pkg.blob, &pkg.signature, &fx.trust, NOW, HOST).unwrap();
    // The proof names who signed it, so a reviewer can audit the install.
    assert_eq!(verified.signer().publisher, PUBLISHER);
    assert_eq!(verified.signer().key_id, KEY_ID);
    assert_eq!(verified.digest, digest_hex(&verified.manifest_bytes));

    // Same key_id, different public key, different root: that store never
    // issued the proof and must refuse to install it.
    let rival_root = generate_key();
    let rival_cert = PublisherCert {
        publisher: PUBLISHER.into(),
        key_id: KEY_ID.into(),
        public_key: hex::encode(generate_key().verifying_key().as_bytes()),
        not_before: 0,
        not_after: u64::MAX,
    };
    let mut rival = TrustStore::new("root-1", rival_root.verifying_key());
    rival
        .enroll(&issue_cert("root-1", &rival_root, rival_cert).unwrap(), NOW)
        .unwrap();
    let mut hijacked = Installer::new(&fx.base, Channel::Prod, rival, HOST);
    assert!(matches!(
        hijacked.install_verified(verified.clone()).unwrap_err(),
        MiniError::UntrustedPublisher
    ));
    // The store that did issue it accepts the install, files and all.
    let mut honest = fx.installer(Channel::Prod);
    assert_eq!(
        installed(honest.install_verified(verified).unwrap()).version,
        "1.0.0"
    );

    // Revoking the key after the review step still stops the install: the
    // proof is re-checked against the live revocation lists.
    let proof = verify(&pkg.blob, &pkg.signature, &fx.trust, NOW, HOST).unwrap();
    let mut revoked_store = fx.trust.clone();
    revoked_store.revoke_key(KEY_ID);
    let mut late = Installer::new(&fx.base, Channel::Prod, revoked_store, HOST);
    assert!(matches!(
        late.install_verified(proof).unwrap_err(),
        MiniError::Revoked
    ));
}

#[test]
fn certificate_window_is_inclusive_and_enforced_on_every_verify() {
    let fx = Fixture::new("window");
    let pkg = fx.build("1.0.0", Channel::Prod, &[]);
    let root = generate_key();
    let cert = PublisherCert {
        publisher: PUBLISHER.into(),
        key_id: KEY_ID.into(),
        public_key: hex::encode(fx.signer.verifying_key().as_bytes()),
        not_before: NOW,
        not_after: NOW + 10,
    };
    let signed = issue_cert("root-1", &root, cert).unwrap();
    let mut trust = TrustStore::new("root-1", root.verifying_key());
    // Enrollment and verification both use the caller-supplied clock.
    trust.enroll(&signed, NOW).unwrap();
    assert!(verify(&pkg.blob, &pkg.signature, &trust, NOW, HOST).is_ok());
    assert!(verify(&pkg.blob, &pkg.signature, &trust, NOW + 10, HOST).is_ok());
    assert!(matches!(
        verify(&pkg.blob, &pkg.signature, &trust, NOW + 11, HOST).unwrap_err(),
        MiniError::CertExpired
    ));
    assert!(matches!(
        verify(&pkg.blob, &pkg.signature, &trust, NOW - 1, HOST).unwrap_err(),
        MiniError::CertExpired
    ));

    // Revoking the publisher (not just the key) blocks an in-window cert.
    let mut revoked = trust.clone();
    revoked.revoke_publisher(PUBLISHER);
    assert!(matches!(
        verify(&pkg.blob, &pkg.signature, &revoked, NOW, HOST).unwrap_err(),
        MiniError::Revoked
    ));
}

#[test]
fn signature_is_bound_to_the_exact_manifest_bytes() {
    let fx = Fixture::new("binding");
    let a = fx.build("1.0.0", Channel::Prod, &[]);
    let b = fx.build_with("1.0.0", Channel::Prod, &[], "other body");
    // Same key, same version, different content: B's signature never covers A.
    assert!(matches!(
        verify(&a.blob, &b.signature, &fx.trust, NOW, HOST).unwrap_err(),
        MiniError::BadSigning
    ));
    // A package with no mini.json at all is not a package.
    let orphan = rebuild(&a.blob, |files| {
        files.remove(MANIFEST_FILE);
    });
    assert!(matches!(
        verify(&orphan, &a.signature, &fx.trust, NOW, HOST).unwrap_err(),
        MiniError::BadPackage(reason) if reason.contains("mini.json")
    ));
    // Truncated / random bytes surface as an error, never a panic.
    assert!(open(&a.blob[..a.blob.len() / 2], HOST).is_err());
    assert!(open(b"not a dpage at all", HOST).is_err());
    assert!(open(&[], HOST).is_err());
    // `open` reports the claims of an unsigned package but installs nothing:
    // its return type carries no proof of verification.
    let inspected = open(&a.blob, HOST).unwrap();
    assert_eq!(inspected.digest, digest_hex(&inspected.manifest_bytes));
    assert_eq!(inspected.manifest.app_id, APP_ID);
}

#[test]
fn oversize_manifest_is_refused_before_it_is_parsed() {
    let fx = Fixture::new("manifest-budget");
    let build = |mini_json: Vec<u8>| {
        pack_files(FileMap(vec![
            FileEntry {
                path: MANIFEST_FILE.into(),
                data: mini_json,
            },
            FileEntry {
                path: "index.js".into(),
                data: b"body".to_vec(),
            },
        ]))
        .unwrap()
    };

    // One byte over the cap: refused without deserializing, so a hostile
    // manifest cannot make the host allocate a parse tree first.
    let blob = build(vec![b' '; MAX_MANIFEST_BYTES + 1]);
    let signature = sign_manifest(b"whatever", &fx.signer, KEY_ID);
    assert!(matches!(
        verify(&blob, &signature, &fx.trust, NOW, HOST).unwrap_err(),
        MiniError::BadPackage(reason) if reason.contains("too large")
    ));
    assert!(matches!(
        open(&blob, HOST).unwrap_err(),
        MiniError::BadPackage(reason) if reason.contains("too large")
    ));

    // Exactly at the cap the reader hands the bytes to the parser, which then
    // rejects them on content - the size gate is `>`, not `>=`.
    let blob = build(vec![b' '; MAX_MANIFEST_BYTES]);
    assert!(matches!(
        open(&blob, HOST).unwrap_err(),
        MiniError::BadJson(_)
    ));
}

#[test]
fn per_file_resource_cap_is_enforced_at_the_boundary() {
    let fx = Fixture::new("resource-budget");
    let big = vec![0u8; MAX_RESOURCE_BYTES];
    let mut files = BTreeMap::new();
    files.insert("index.js".to_string(), b"body".to_vec());
    files.insert("big.bin".to_string(), big.clone());
    let mut manifest = template(APP_ID, "1.0.0", Channel::Prod);
    manifest.resources = BTreeMap::from([
        ("index.js".into(), digest_hex(b"body")),
        ("big.bin".into(), digest_hex(&big)),
    ]);
    let (blob, sig) = craft(&fx, &manifest, files.clone());
    // Exactly at the cap: allowed, and it survives full verification.
    assert!(verify(&blob, &sig, &fx.trust, NOW, HOST).is_ok());

    // One byte over: refused by the verifier and by the packer, and nothing
    // reaches the disk either way.
    let mut over = b"body".to_vec();
    over.extend(std::iter::repeat_n(0u8, MAX_RESOURCE_BYTES));
    let mut oversized = manifest.clone();
    oversized
        .resources
        .insert("index.js".into(), digest_hex(&over));
    let mut over_files = files;
    over_files.insert("index.js".into(), over.clone());
    let (blob, sig) = craft(&fx, &oversized, over_files);
    assert!(matches!(
        verify(&blob, &sig, &fx.trust, NOW, HOST).unwrap_err(),
        MiniError::ResourceTooLarge(path, size, limit)
            if path == "index.js" && size == over.len() && limit == MAX_RESOURCE_BYTES
    ));

    let dir = fx.base.join("fat-source");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("index.js"), &over).unwrap();
    std::fs::write(dir.join("sub/big.bin"), &big).unwrap();
    assert!(matches!(
        build_signed(&dir, &template(APP_ID, "1.0.0", Channel::Prod), HOST, &fx.signer, KEY_ID)
            .unwrap_err(),
        MiniError::ResourceTooLarge(path, _, _) if path == "index.js"
    ));
}

#[test]
fn whole_package_budget_is_enforced() {
    let fx = Fixture::new("package-budget");
    let big = vec![0u8; MAX_RESOURCE_BYTES];
    // Five capped files (40 MiB) stay inside the codec's own decompression
    // cap but cross the install budget, so the package layer must be the one
    // that says no.
    let mut files = BTreeMap::new();
    let mut resources = BTreeMap::new();
    for index in 0..5usize {
        let path = format!("blob-{index}.bin");
        resources.insert(path.clone(), digest_hex(&big));
        files.insert(path, big.clone());
    }
    let mut fat = template(APP_ID, "1.0.0", Channel::Prod);
    fat.entry = "blob-0.bin".into();
    fat.resources.clone_from(&resources);
    let (blob, sig) = craft(&fx, &fat, files);
    assert_eq!(
        5 * MAX_RESOURCE_BYTES,
        MAX_PACKAGE_BYTES + MAX_RESOURCE_BYTES
    );
    assert!(matches!(
        verify(&blob, &sig, &fx.trust, NOW, HOST).unwrap_err(),
        MiniError::PackageTooLarge(bytes, limit)
            if bytes == 5 * MAX_RESOURCE_BYTES && limit == MAX_PACKAGE_BYTES
    ));
}

#[test]
fn manifest_shape_limits_are_checked_at_the_boundary() {
    // Resource count: exactly MAX_RESOURCES is fine, one more is not.
    let mut resources = BTreeMap::new();
    resources.insert("index.js".to_string(), digest_hex(b"body"));
    while resources.len() < MAX_RESOURCES {
        resources.insert(format!("r/r-{:06}.js", resources.len()), digest_hex(b"x"));
    }
    let mut full = template(APP_ID, "1.0.0", Channel::Prod);
    full.resources.clone_from(&resources);
    full.validate(HOST).unwrap();
    full.resources
        .insert("one-too-many.js".into(), digest_hex(b"x"));
    assert!(matches!(
        full.validate(HOST).unwrap_err(),
        MiniError::BadManifest("resources")
    ));

    // Resource path length.
    let long = format!("{}/index.js", "a".repeat(wxlike_dpage::MAX_PATH_LEN));
    assert!(long.len() > wxlike_dpage::MAX_PATH_LEN);
    let mut m = template(APP_ID, "1.0.0", Channel::Prod);
    m.entry = long.clone();
    m.resources = BTreeMap::from([(long, digest_hex(b"body"))]);
    assert!(matches!(
        m.validate(HOST).unwrap_err(),
        MiniError::BadManifest("resource path")
    ));

    // Identity lengths.
    let m = template(&"a".repeat(MAX_APP_ID_LEN), "1.0.0", Channel::Prod);
    m.validate(HOST).unwrap();
    let m = template(&"a".repeat(MAX_APP_ID_LEN + 1), "1.0.0", Channel::Prod);
    assert!(matches!(
        m.validate(HOST).unwrap_err(),
        MiniError::BadManifest("app_id")
    ));
    let mut m = template(APP_ID, "1.0.0", Channel::Prod);
    m.name = "x".repeat(MAX_NAME_CHARS);
    m.validate(HOST).unwrap();
    m.name = "x".repeat(MAX_NAME_CHARS + 1);
    assert!(matches!(
        m.validate(HOST).unwrap_err(),
        MiniError::BadManifest("name")
    ));

    // Traversal / absolute / host-anchored entry paths never reach the disk.
    for entry in [
        "../x.js",
        "/x.js",
        "a/../../x.js",
        "C:/x.js",
        "a\\b.js",
        "",
        ".",
    ] {
        let mut m = template(APP_ID, "1.0.0", Channel::Prod);
        m.entry = entry.into();
        m.resources = BTreeMap::from([(entry.into(), digest_hex(b"body"))]);
        assert!(m.validate(HOST).is_err(), "must reject entry {entry:?}");
    }
}

#[test]
fn installer_metadata_names_cannot_be_claimed_by_a_package() {
    let fx = Fixture::new("reserved");
    for name in [COMPLETE_MARKER, PIN_FILE] {
        let mut m = template(APP_ID, "1.0.0", Channel::Prod);
        m.entry = name.into();
        m.resources = BTreeMap::from([(name.to_string(), digest_hex(b"body"))]);
        assert!(
            matches!(
                m.validate(HOST).unwrap_err(),
                MiniError::ReservedResource(path) if path == name
            ),
            "{name} must stay owned by the installer"
        );

        // The packer rejects it too, so a source tree cannot smuggle state
        // files in and have their digests silently overwritten at install.
        let dir = fx.base.join(format!("src-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.js"), b"body").unwrap();
        std::fs::write(dir.join(name), b"pretend i am install state").unwrap();
        assert!(matches!(
            build_signed(
                &dir,
                &template(APP_ID, "1.0.0", Channel::Prod),
                HOST,
                &fx.signer,
                KEY_ID
            )
            .unwrap_err(),
            MiniError::ReservedResource(_)
        ));
    }
}

#[test]
fn dev_and_prod_roots_stay_separate_on_disk() {
    let fx = Fixture::new("roots");
    let dev = fx.build("1.0.0", Channel::Dev, &[]);
    let prod = fx.build("1.0.0", Channel::Prod, &[]);
    let mut dev_inst = fx.installer(Channel::Dev);
    let app = installed(dev_inst.install(&dev.blob, &dev.signature, NOW).unwrap());
    assert!(app.path.starts_with(fx.base.join("dev")));
    assert!(!fx.base.join("prod").join("apps").exists());

    let mut prod_inst = fx.installer(Channel::Prod);
    assert!(matches!(
        prod_inst
            .install(&dev.blob, &dev.signature, NOW)
            .unwrap_err(),
        MiniError::WrongChannel(_, _)
    ));
    let prod_app = installed(prod_inst.install(&prod.blob, &prod.signature, NOW).unwrap());
    assert!(prod_app.path.starts_with(fx.base.join("prod")));
    // Each root keeps its own watermark, so a dev install cannot raise the
    // production floor (and vice versa).
    assert_eq!(
        dev_inst.watermark(APP_ID).unwrap().as_deref(),
        Some("1.0.0")
    );
    assert_eq!(
        prod_inst.watermark(APP_ID).unwrap().as_deref(),
        Some("1.0.0")
    );

    // Only the declared files exist on disk, plus the installer's own marker.
    let mut names: Vec<String> = std::fs::read_dir(&prod_app.path)
        .unwrap()
        .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            COMPLETE_MARKER.to_string(),
            "assets".to_string(),
            "index.js".to_string(),
            MANIFEST_FILE.to_string()
        ]
    );
}

#[test]
fn unsigned_or_stale_blobs_cannot_be_signed_or_installed() {
    let fx = Fixture::new("sign-gate");
    let pkg = fx.build("1.0.0", Channel::Prod, &[]);
    // `mini sign` refuses to attest a blob whose manifest lies.
    let broken = rebuild(&pkg.blob, |files| {
        files.insert("payload.sh".into(), b"echo pwned".to_vec());
    });
    assert!(matches!(
        sign_blob(&broken, HOST, &fx.signer, KEY_ID).unwrap_err(),
        MiniError::ExtraResource(path) if path == "payload.sh"
    ));
    // A manifest below the host floor cannot be signed either.
    let old_host = rebuild(&pkg.blob, |files| {
        let mut value: serde_json::Value =
            serde_json::from_slice(files.get(MANIFEST_FILE).unwrap()).unwrap();
        value["min_host"] = "9.0.0".into();
        files.insert(MANIFEST_FILE.into(), serde_json::to_vec(&value).unwrap());
    });
    assert!(matches!(
        sign_blob(&old_host, HOST, &fx.signer, KEY_ID).unwrap_err(),
        MiniError::HostTooOld(_, _)
    ));
    // pack_unsigned is not a release artefact: without a signature nothing
    // installs, and the blob itself is byte-identical to the signed one.
    let packed =
        pack_unsigned(&fx.source, &template(APP_ID, "1.0.0", Channel::Prod), HOST).unwrap();
    assert!(matches!(
        verify(&packed.blob, &pkg.signature, &fx.trust, NOW, HOST).unwrap_err(),
        MiniError::BadSigning | MiniError::UntrustedPublisher
    ));
}
