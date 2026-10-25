//! G08-04 acceptance: reviewed catalog, cross-device recents, upgrade
//! consent, takedown and confirmed uninstall.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::SigningKey;
use wxlike_dpage::{pack_files, FileEntry, FileMap};
use wxlike_miniapps::catalog::data::{InstallLedger, MemoryData, MemoryInstallLedger};
use wxlike_miniapps::catalog::recent::{MemoryRecents, SharedRecents, SyncError, SyncState};
use wxlike_miniapps::catalog::source::{
    CatalogEntry, Category, ReviewedCatalog, SearchQuery, SourceError, SourceFailure,
};
use wxlike_miniapps::catalog::takedown::{RecordingAuthority, Takedown};
use wxlike_miniapps::catalog::{Catalog, CatalogError, UserDecision};
use wxlike_miniapps::packages::install::Installer;
use wxlike_miniapps::packages::manifest::digest_hex;
use wxlike_miniapps::packages::{
    generate_key, issue_cert, sign_manifest, Channel, InstallOutcome, InstalledApp, Manifest,
    PackageSignature, PublisherCert, TrustStore,
};

const NOW: u64 = 1_800_000_000;
const HOST: &str = "0.1.0";
const PUBLISHER: &str = "wxlike-labs";
const KEY_ID: &str = "pub-1";
const APP: &str = "com.wxlike.notes";
const OTHER: &str = "com.wxlike.shop";

struct Signer {
    base: PathBuf,
    key: SigningKey,
    trust: TrustStore,
}

impl Signer {
    fn new(name: &str) -> Self {
        let root = generate_key();
        let key = generate_key();
        let cert = PublisherCert {
            publisher: PUBLISHER.into(),
            key_id: KEY_ID.into(),
            public_key: hex::encode(key.verifying_key().as_bytes()),
            not_before: 0,
            not_after: u64::MAX,
        };
        let signed = issue_cert("root-1", &root, cert).unwrap();
        let mut trust = TrustStore::new("root-1", root.verifying_key());
        trust.enroll(&signed, NOW).unwrap();
        Self {
            base: unique(name),
            key,
            trust,
        }
    }

    fn package(
        &self,
        app_id: &str,
        version: &str,
        capabilities: &[&str],
    ) -> (Vec<u8>, PackageSignature) {
        let script = format!("onLoad(function() {{ wxhost.log('{version}'); }});");
        let mut resources = BTreeMap::new();
        resources.insert("index.js".to_string(), digest_hex(script.as_bytes()));
        let manifest = Manifest {
            app_id: app_id.into(),
            name: format!("App {app_id}"),
            version: version.into(),
            min_host: HOST.into(),
            entry: "index.js".into(),
            channel: Channel::Prod,
            publisher: PUBLISHER.into(),
            capabilities: capabilities.iter().map(|c| (*c).to_string()).collect(),
            resources,
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let files = FileMap(vec![
            FileEntry {
                path: "index.js".into(),
                data: script.into_bytes(),
            },
            FileEntry {
                path: "mini.json".into(),
                data: manifest_bytes.clone(),
            },
        ]);
        let blob = pack_files(files).unwrap();
        let sig = sign_manifest(&manifest_bytes, &self.key, KEY_ID);
        (blob, sig)
    }

    fn installer(&self) -> Installer {
        self.installer_at("device-a")
    }

    fn installer_at(&self, slot: &str) -> Installer {
        Installer::new(
            &self.base.join(slot),
            Channel::Prod,
            self.trust.clone(),
            HOST,
        )
    }
}

fn unique(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "wxlike-g08-catalog-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn entry(
    app_id: &str,
    name: &str,
    version: &str,
    published: &[&str],
    capabilities: &[&str],
    category: Category,
    featured: bool,
) -> CatalogEntry {
    CatalogEntry {
        app_id: app_id.into(),
        name: name.into(),
        version: version.into(),
        published_versions: published.iter().map(|v| (*v).to_string()).collect(),
        category,
        summary: format!("{name} summary"),
        publisher: PUBLISHER.into(),
        capabilities: capabilities.iter().map(|c| (*c).to_string()).collect(),
        featured,
    }
}

fn notes_entry() -> CatalogEntry {
    entry(
        APP,
        "笔记",
        "1.0.0",
        &["1.0.0"],
        &["storage"],
        Category::Productivity,
        false,
    )
}

fn shop_entry() -> CatalogEntry {
    entry(
        OTHER,
        "附近好物",
        "1.0.0",
        &["1.0.0"],
        &["storage", "network", "payment"],
        Category::Shopping,
        true,
    )
}

fn untitled(entries: Vec<CatalogEntry>) -> Catalog {
    Catalog::new(
        Box::new(ReviewedCatalog::new(entries)),
        Box::new(MemoryRecents::default()),
    )
}

fn installed(outcome: InstallOutcome) -> InstalledApp {
    match outcome {
        InstallOutcome::Installed(app) | InstallOutcome::AlreadyInstalled(app) => app,
    }
}

#[test]
fn search_and_recommend_only_return_reviewed_cards() {
    let mut catalog = untitled(vec![notes_entry(), shop_entry()]);

    let hits = catalog.search(&SearchQuery::new("笔记")).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].app_id, APP);

    let mut query = SearchQuery::new("");
    query.category = Some(Category::Shopping);
    assert_eq!(catalog.search(&query).unwrap()[0].app_id, OTHER);

    let recommended = catalog.recommend(2).unwrap();
    assert_eq!(recommended.len(), 2);
    assert!(recommended[0].featured, "featured cards come first");
}

#[test]
fn empty_result_is_success_not_an_error() {
    let mut catalog = untitled(vec![notes_entry()]);
    assert!(catalog
        .search(&SearchQuery::new("nothing-matches"))
        .unwrap()
        .is_empty());
}

#[test]
fn bad_limits_and_oversized_queries_are_refused() {
    let mut catalog = untitled(vec![notes_entry()]);
    let mut query = SearchQuery::new("");
    query.limit = 0;
    assert!(matches!(
        catalog.search(&query),
        Err(CatalogError::BadQuery("limit"))
    ));
    assert!(matches!(
        catalog.recommend(51),
        Err(CatalogError::BadQuery("limit"))
    ));
    assert!(matches!(
        catalog.search(&SearchQuery::new("x".repeat(65))),
        Err(CatalogError::BadQuery("search text"))
    ));
}

#[test]
fn source_offline_timeout_and_unauthorized_are_distinct() {
    let cases = [
        (SourceFailure::Offline, "offline"),
        (SourceFailure::Timeout, "timeout"),
        (SourceFailure::Unauthorized, "unauthorized"),
    ];
    for (failure, label) in cases {
        let mut catalog = Catalog::new(
            Box::new(ReviewedCatalog::broken(vec![notes_entry()], failure)),
            Box::new(MemoryRecents::default()),
        );
        let err = catalog.search(&SearchQuery::new("")).unwrap_err();
        let matched = match err {
            CatalogError::Source(SourceError::Offline) => "offline",
            CatalogError::Source(SourceError::Timeout) => "timeout",
            CatalogError::Source(SourceError::Unauthorized) => "unauthorized",
            other => panic!("unexpected error: {other}"),
        };
        assert_eq!(matched, label);
    }
}

#[test]
fn install_is_recorded_and_repeat_is_idempotent() {
    let signer = Signer::new("install");
    let mut installer = signer.installer();
    let mut ledger = MemoryInstallLedger::new();
    let mut catalog = untitled(vec![notes_entry()]);
    let (blob, sig) = signer.package(APP, "1.0.0", &["storage"]);

    let first = catalog
        .install(&mut installer, &mut ledger, "device-a", &blob, &sig, NOW)
        .unwrap();
    assert_eq!(installed(first).version, "1.0.0");

    let second = catalog
        .install(&mut installer, &mut ledger, "device-a", &blob, &sig, NOW)
        .unwrap();
    assert!(matches!(second, InstallOutcome::AlreadyInstalled(_)));
    assert_eq!(ledger.devices(APP).unwrap(), vec!["device-a".to_string()]);
}

#[test]
fn unpublished_version_and_unreviewed_capability_are_refused() {
    let signer = Signer::new("review");
    let mut installer = signer.installer();
    let mut ledger = MemoryInstallLedger::new();
    let mut catalog = untitled(vec![notes_entry()]);

    let (blob, sig) = signer.package(APP, "1.1.0", &["storage"]);
    let err = catalog
        .install(&mut installer, &mut ledger, "device-a", &blob, &sig, NOW)
        .unwrap_err();
    assert!(matches!(err, CatalogError::NotPublished(_, _)));

    let (blob, sig) = signer.package(APP, "1.0.0", &["storage", "camera"]);
    let err = catalog
        .install(&mut installer, &mut ledger, "device-a", &blob, &sig, NOW)
        .unwrap_err();
    assert!(matches!(err, CatalogError::CapabilityNotReviewed(_, _)));
    assert!(installer.current(APP).is_err(), "nothing was activated");
}

#[test]
fn another_device_shows_the_same_install_record() {
    let signer = Signer::new("devices");
    let mut device_a = signer.installer_at("device-a");
    let mut device_b = signer.installer_at("device-b");
    let mut ledger = MemoryInstallLedger::new();
    let mut catalog = untitled(vec![notes_entry()]);
    let (blob, sig) = signer.package(APP, "1.0.0", &["storage"]);

    catalog
        .install(&mut device_a, &mut ledger, "device-a", &blob, &sig, NOW)
        .unwrap();
    catalog
        .install(&mut device_b, &mut ledger, "device-b", &blob, &sig, NOW)
        .unwrap();

    assert_eq!(
        ledger.devices(APP).unwrap(),
        vec!["device-a".to_string(), "device-b".to_string()]
    );
    assert_eq!(ledger.version_on(APP, "device-b").as_deref(), Some("1.0.0"));
}

#[test]
fn upgrade_adding_location_and_payment_needs_confirmation() {
    let signer = Signer::new("upgrade");
    let mut installer = signer.installer();
    let mut ledger = MemoryInstallLedger::new();
    let mut catalog = untitled(vec![entry(
        APP,
        "笔记",
        "2.0.0",
        &["2.0.0", "1.0.0"],
        &["storage", "location", "payment"],
        Category::Productivity,
        false,
    )]);
    let (v1, sig1) = signer.package(APP, "1.0.0", &["storage"]);
    let (v2, sig2) = signer.package(APP, "2.0.0", &["storage", "location", "payment"]);
    catalog
        .install(&mut installer, &mut ledger, "device-a", &v1, &sig1, NOW)
        .unwrap();

    let cancelled = catalog
        .upgrade(
            &mut installer,
            &mut ledger,
            "device-a",
            &v2,
            &sig2,
            NOW,
            UserDecision::Cancel,
        )
        .unwrap_err();
    assert!(matches!(cancelled, CatalogError::Cancelled(_)));
    assert_eq!(
        installer.current(APP).unwrap().version,
        "1.0.0",
        "cancelled upgrade keeps the old version"
    );

    let outcome = catalog
        .upgrade(
            &mut installer,
            &mut ledger,
            "device-a",
            &v2,
            &sig2,
            NOW,
            UserDecision::Confirm,
        )
        .unwrap();
    assert_eq!(outcome.installed.version, "2.0.0");
    assert_eq!(
        outcome.plan.added_capabilities,
        vec!["location".to_string(), "payment".to_string()]
    );
    assert_eq!(
        outcome.plan.sensitive_added,
        vec!["location".to_string(), "payment".to_string()]
    );
}

#[test]
fn upgrade_without_new_capabilities_does_not_prompt() {
    let signer = Signer::new("upgrade-plain");
    let mut installer = signer.installer();
    let mut ledger = MemoryInstallLedger::new();
    let mut catalog = untitled(vec![entry(
        APP,
        "笔记",
        "2.0.0",
        &["2.0.0", "1.0.0"],
        &["storage"],
        Category::Productivity,
        false,
    )]);
    let (v1, sig1) = signer.package(APP, "1.0.0", &["storage"]);
    let (v2, sig2) = signer.package(APP, "2.0.0", &["storage"]);
    catalog
        .install(&mut installer, &mut ledger, "device-a", &v1, &sig1, NOW)
        .unwrap();

    let outcome = catalog
        .upgrade(
            &mut installer,
            &mut ledger,
            "device-a",
            &v2,
            &sig2,
            NOW,
            UserDecision::Cancel,
        )
        .unwrap();
    assert!(!outcome.plan.requires_confirmation());
    assert_eq!(outcome.installed.version, "2.0.0");
}

#[test]
fn takedown_revokes_runtime_and_blocks_launch_and_update() {
    let signer = Signer::new("takedown");
    let mut installer = signer.installer();
    let mut ledger = MemoryInstallLedger::new();
    let mut catalog = untitled(vec![entry(
        APP,
        "笔记",
        "2.0.0",
        &["2.0.0", "1.0.0"],
        &["storage"],
        Category::Productivity,
        false,
    )]);
    let (v1, sig1) = signer.package(APP, "1.0.0", &["storage"]);
    let installed = installed(
        catalog
            .install(&mut installer, &mut ledger, "device-a", &v1, &sig1, NOW)
            .unwrap(),
    );
    catalog.set_favorite(APP, true).unwrap();
    catalog.record_launch(&installed, NOW).unwrap();

    let mut authority = RecordingAuthority::new();
    let report = catalog
        .apply_takedown(
            Takedown {
                app_id: APP.into(),
                version: None,
                reason: "ip complaint".into(),
            },
            &mut authority,
        )
        .unwrap();
    assert_eq!(report.sessions_revoked, 1);
    assert_eq!(authority.revoked(), &[APP.to_string()]);

    assert!(matches!(
        catalog.authorize_launch(&installed),
        Err(CatalogError::Revoked(_))
    ));
    assert!(matches!(
        catalog.restore(&installer, APP, NOW + 1),
        Err(CatalogError::Revoked(_))
    ));
    let (v2, sig2) = signer.package(APP, "2.0.0", &["storage"]);
    assert!(matches!(
        catalog.install(&mut installer, &mut ledger, "device-a", &v2, &sig2, NOW),
        Err(CatalogError::Revoked(_))
    ));
    assert!(catalog.recents(10).unwrap().entries.is_empty());
    assert!(catalog.favorites().is_empty());
}

#[test]
fn version_scoped_takedown_leaves_other_versions_installable() {
    let signer = Signer::new("takedown-version");
    let mut installer = signer.installer();
    let mut ledger = MemoryInstallLedger::new();
    let mut catalog = untitled(vec![entry(
        APP,
        "笔记",
        "2.0.0",
        &["2.0.0", "1.0.0"],
        &["storage"],
        Category::Productivity,
        false,
    )]);
    let (v1, sig1) = signer.package(APP, "1.0.0", &["storage"]);
    let installed = installed(
        catalog
            .install(&mut installer, &mut ledger, "device-a", &v1, &sig1, NOW)
            .unwrap(),
    );
    let mut authority = RecordingAuthority::new();
    catalog
        .apply_takedown(
            Takedown {
                app_id: APP.into(),
                version: Some("1.0.0".into()),
                reason: "bad build".into(),
            },
            &mut authority,
        )
        .unwrap();

    assert!(matches!(
        catalog.authorize_launch(&installed),
        Err(CatalogError::Revoked(_))
    ));
    let (v2, sig2) = signer.package(APP, "2.0.0", &["storage"]);
    catalog
        .install(&mut installer, &mut ledger, "device-a", &v2, &sig2, NOW)
        .unwrap();
    assert_eq!(installer.current(APP).unwrap().version, "2.0.0");
}

#[test]
fn clicking_a_recent_entry_restores_the_installed_app() {
    let signer = Signer::new("recent");
    let mut installer = signer.installer();
    let mut ledger = MemoryInstallLedger::new();
    let mut catalog = untitled(vec![notes_entry()]);
    let (blob, sig) = signer.package(APP, "1.0.0", &["storage"]);
    let app = installed(
        catalog
            .install(&mut installer, &mut ledger, "device-a", &blob, &sig, NOW)
            .unwrap(),
    );
    catalog.record_launch(&app, NOW).unwrap();

    let view = catalog.recents(10).unwrap();
    assert_eq!(view.sync, SyncState::LocalOnly);
    assert_eq!(view.entries[0].app_id, APP);
    assert_eq!(view.entries[0].launches, 1);

    let ticket = catalog.restore(&installer, APP, NOW + 5).unwrap();
    assert_eq!(ticket.version, "1.0.0");
    assert_eq!(ticket.digest, app.digest);
    assert_eq!(catalog.recents(10).unwrap().entries[0].launches, 2);
}

#[test]
fn recents_sync_across_devices_and_degrade_offline() {
    let cloud = SharedRecents::new();
    let mut phone = untitled(vec![notes_entry()]);
    phone
        .enable_sync("acct-1", Box::new(cloud.clone()))
        .unwrap();
    let mut laptop = untitled(vec![notes_entry()]);
    laptop
        .enable_sync("acct-1", Box::new(cloud.clone()))
        .unwrap();

    let app = InstalledApp {
        app_id: APP.into(),
        version: "1.0.0".into(),
        digest: "digest".into(),
        path: PathBuf::from("x"),
        capabilities: vec!["storage".into()],
    };
    assert_eq!(phone.record_launch(&app, NOW).unwrap(), SyncState::Synced);

    let view = laptop.recents(10).unwrap();
    assert_eq!(view.sync, SyncState::Synced);
    assert_eq!(view.entries[0].app_id, APP);

    let mut offline = untitled(vec![notes_entry()]);
    offline
        .enable_sync(
            "acct-1",
            Box::new(SharedRecents::with_failure(SyncError::Offline)),
        )
        .unwrap();
    offline.record_launch(&app, NOW).unwrap();
    let view = offline.recents(10).unwrap();
    assert_eq!(view.sync, SyncState::Degraded("offline"));
    assert_eq!(view.entries.len(), 1, "local recents survive sync failure");
}

#[test]
fn uninstall_requires_confirmation_and_wipes_only_that_app() {
    let signer = Signer::new("uninstall");
    let mut installer = signer.installer();
    let mut ledger = MemoryInstallLedger::new();
    let mut catalog = untitled(vec![notes_entry()]);
    let mut data = MemoryData::new();
    data.put(APP, "draft", 10);
    data.put(APP, "photo", 5);
    data.put(OTHER, "cart", 3);

    let (blob, sig) = signer.package(APP, "1.0.0", &["storage"]);
    let app = installed(
        catalog
            .install(&mut installer, &mut ledger, "device-a", &blob, &sig, NOW)
            .unwrap(),
    );
    catalog.set_favorite(APP, true).unwrap();
    catalog.record_launch(&app, NOW).unwrap();

    let cancelled = catalog
        .uninstall(
            &mut installer,
            &mut ledger,
            &mut data,
            "device-a",
            APP,
            UserDecision::Cancel,
        )
        .unwrap_err();
    assert!(matches!(cancelled, CatalogError::Cancelled(_)));
    assert!(installer.current(APP).is_ok());
    assert_eq!(data.keys(APP).len(), 2, "cancel must not wipe data");

    let report = catalog
        .uninstall(
            &mut installer,
            &mut ledger,
            &mut data,
            "device-a",
            APP,
            UserDecision::Confirm,
        )
        .unwrap();
    assert_eq!(report.wiped.storage_keys, 2);
    assert_eq!(report.wiped.bytes, 15);
    assert!(installer.current(APP).is_err());
    assert!(data.keys(APP).is_empty());
    assert_eq!(data.keys(OTHER), vec!["cart".to_string()]);
    assert!(ledger.devices(APP).unwrap().is_empty());
    assert!(catalog.recents(10).unwrap().entries.is_empty());
    assert!(catalog.favorites().is_empty());

    let again = catalog
        .uninstall(
            &mut installer,
            &mut ledger,
            &mut data,
            "device-a",
            APP,
            UserDecision::Confirm,
        )
        .unwrap_err();
    assert!(matches!(again, CatalogError::NotInstalled(_)));
}

#[test]
fn favorites_are_idempotent_and_app_ids_are_validated() {
    let mut catalog = untitled(vec![notes_entry()]);
    assert!(catalog.set_favorite(APP, true).unwrap());
    assert!(!catalog.set_favorite(APP, true).unwrap());
    assert!(matches!(
        catalog.set_favorite("../evil", true),
        Err(CatalogError::BadQuery(_))
    ));
}

#[test]
fn audit_lines_carry_no_search_text_or_takedown_reason() {
    let mut catalog = untitled(vec![notes_entry()]);
    catalog.search(&SearchQuery::new("秘密计划")).unwrap();
    let mut authority = RecordingAuthority::new();
    catalog
        .apply_takedown(
            Takedown {
                app_id: APP.into(),
                version: None,
                reason: "internal-report-42".into(),
            },
            &mut authority,
        )
        .unwrap();

    let dump = format!("{:?}", catalog.audit());
    assert!(!dump.contains("秘密计划"));
    assert!(!dump.contains("internal-report-42"));
    assert!(dump.contains(APP), "app ids stay for accountability");
}
