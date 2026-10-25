//! G08-05 acceptance: one signed example package runs and saves data on all
//! four targets, and the shop example's payment flow stays sandbox-only when
//! driven through the real broker.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ed25519_dalek::SigningKey;
use wxlike_container::broker::calls::TypedCall;
use wxlike_container::broker::grants::{GuestIdentity, Perm};
use wxlike_container::broker::ports::{fixtures, BrokerPorts};
use wxlike_container::broker::{Broker, BrokerError};
use wxlike_container::host::budget::{BudgetLedger, BudgetLimits};
use wxlike_container::host::profile::{DeviceTarget, IsolationProfile, MINI_CSP};
use wxlike_container::host::{HostState, MiniHost};
use wxlike_dpage::Page;
use wxlike_miniapps::packages::manifest::digest_hex;
use wxlike_miniapps::packages::{
    build_signed, generate_key, issue_cert, InstallOutcome, InstalledApp, Installer, Manifest,
    PublisherCert, TrustStore,
};

const NOW: u64 = 1_800_000_000;
const HOST: &str = "0.1.0";
const PUBLISHER: &str = "wxlike-labs";
const KEY_ID: &str = "pub-1";

const EXAMPLES: [&str; 4] = ["notes", "shop", "game", "weather"];
const TARGETS: [DeviceTarget; 4] = [
    DeviceTarget::Desktop,
    DeviceTarget::Web,
    DeviceTarget::Android,
    DeviceTarget::Ios,
];

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../packages/mini-sdk/examples")
}

fn unique(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "wxlike-g08-ex-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn fixture() -> (TrustStore, SigningKey) {
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
    let mut store = TrustStore::new("root-1", root.verifying_key());
    store.enroll(&signed, NOW).unwrap();
    (store, signer)
}

fn installed(outcome: InstallOutcome) -> InstalledApp {
    match outcome {
        InstallOutcome::Installed(app) | InstallOutcome::AlreadyInstalled(app) => app,
    }
}

fn page_for(app: &InstalledApp, name: &str, entry: &str) -> Page {
    let files = read_installed(&app.path);
    Page {
        id: 1,
        manifest: wxlike_dpage::Manifest {
            name: name.to_string(),
            main_js: entry.to_string(),
            title: name.to_string(),
        },
        files,
    }
}

fn read_installed(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    walk(dir, dir, &mut files);
    files
}

fn walk(dir: &Path, base: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(&path, base, files);
            continue;
        }
        let rel = path
            .strip_prefix(base)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        files.insert(rel, std::fs::read(&path).unwrap());
    }
}

#[test]
fn one_signed_package_runs_and_persists_on_every_target() {
    let (store, signer) = fixture();
    for app in EXAMPLES {
        let text = std::fs::read_to_string(examples_dir().join(app).join("mini.template.json")).unwrap();
        let manifest: Manifest = serde_json::from_str(&text).unwrap();
        let package = build_signed(&examples_dir().join(app), &manifest, HOST, &signer, KEY_ID)
            .expect("example package builds");
        let app_id = manifest.app_id.clone();
        let name = manifest.name.clone();
        let entry = manifest.entry.clone();

        for target in TARGETS {
            let base = unique(&format!("{app}-{}", target.as_str()));
            let mut installer = Installer::new(&base, manifest.channel, store.clone(), HOST);
            let record = installed(
                installer
                    .install(&package.blob, &package.signature, NOW)
                    .expect("install on every target"),
            );
            assert_eq!(record.app_id, app_id);
            assert_eq!(record.version, manifest.version);
            assert_eq!(record.digest, digest_hex(&package.manifest_bytes));

            let host = MiniHost::launch_default(target, &app_id).unwrap();
            let profile = host.profile();
            assert!(!profile.allow_main_ipc);
            assert_eq!(profile.csp, MINI_CSP);
            match target {
                DeviceTarget::Web => {
                    assert!(profile.iframe_sandbox.is_some());
                    assert!(profile.webview_data_dir.is_none());
                }
                _ => assert!(profile.webview_data_dir.is_some()),
            }
            drop(host);

            let mut saved = BTreeMap::new();
            let page = page_for(&record, &name, &entry);
            let mut host = MiniHost::launch_default(target, &app_id).unwrap();
            let title = host.run_persisted(&page, "{}", &mut saved).unwrap();
            assert!(!title.is_empty(), "{app}/{} must set a title", target.as_str());
            let key = manifest_app_key(app);
            assert!(saved.contains_key(key), "{app}/{} must save data", target.as_str());
            host.terminate().unwrap();
            assert_eq!(host.state(), HostState::Terminated);

            let mut host = MiniHost::launch_default(target, &app_id).unwrap();
            host.run_persisted(&page, "{}", &mut saved).unwrap();
            assert!(
                saved.contains_key(key),
                "{app}/{}: data must survive a relaunch",
                target.as_str()
            );
            assert_persisted(app, &saved);
        }
    }
}

fn manifest_app_key(app: &str) -> &'static str {
    match app {
        "notes" => "notes.v1",
        "shop" => "shop.cart.v1",
        "game" => "game.score.v1",
        "weather" => "weather.city.v1",
        _ => unreachable!(),
    }
}

fn assert_persisted(app: &str, saved: &BTreeMap<String, String>) {
    match app {
        "notes" => assert!(saved["notes.v1"].contains("t1")),
        "shop" => assert!(saved["shop.cart.v1"].contains("coffee")),
        "game" => assert!(saved["game.score.v1"].parse::<u64>().is_ok()),
        "weather" => assert_eq!(saved["weather.city.v1"], "杭州"),
        _ => unreachable!(),
    }
}

/// The shop flow driven through the real broker with the sandbox payment
/// port: the receipt is sandbox-only, denial keeps the cart, and a repeated
/// order never reaches the payment port twice.
#[test]
fn shop_checkout_is_sandboxed_and_stops_on_denial() {
    let shop_id = "com.wxlike.demo.shop";
    for target in TARGETS {
        let profile = IsolationProfile::new(target, shop_id).unwrap();
        let (sandbox, log) = fixtures::SandboxPayments::new();
        let mut broker = Broker::with_ports(
            profile,
            BrokerPorts {
                confirmer: Box::new(fixtures::AllowAll),
                payments: Box::new(sandbox),
                ..BrokerPorts::default()
            },
        );
        broker.open_session("sess-shop").unwrap();
        let guest = GuestIdentity {
            app_id: shop_id.into(),
            origin: broker.profile().origin.clone(),
            session_id: "sess-shop".into(),
        };
        broker
            .grant_capabilities(&["storage".into(), "network".into(), "payment".into()])
            .unwrap();
        let mut budget = BudgetLedger::new(BudgetLimits::desktop());

        let receipt = broker
            .request(
                &mut budget,
                &guest,
                "pay-1",
                TypedCall::PayRequest {
                    amount: "18.00".into(),
                    currency: "CNY".into(),
                    order_id: "demo-order-0001".into(),
                },
            )
            .unwrap();
        assert_eq!(receipt["status"], "sandbox");
        assert!(receipt["reference"].as_str().unwrap().starts_with("sandbox-"));
        assert!(receipt.get("token").is_none());

        broker.revoke_scope(Perm::Payment);
        let denied = broker.request(
            &mut budget,
            &guest,
            "pay-2",
            TypedCall::PayRequest {
                amount: "18.00".into(),
                currency: "CNY".into(),
                order_id: "demo-order-0002".into(),
            },
        );
        assert!(matches!(denied, Err(BrokerError::NotGranted(_))));
        assert_eq!(log.entries().len(), 1, "revoked scope must not reach the port");
    }
}
