//! G08-01 acceptance: signed package supply chain and crash-safe install.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::SigningKey;
use wxlike_dpage::{pack_files, unpack_raw, FileEntry, FileMap};
use wxlike_miniapps::packages::{
    build_signed, generate_key, issue_cert, sign_blob, Channel, FaultPoint, Faults, InstallOutcome,
    InstalledApp, Installer, Manifest, MiniError, PublisherCert, SignedPackage, TrustStore,
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
    std::env::temp_dir().join(format!(
        "wxlike-g08-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
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
