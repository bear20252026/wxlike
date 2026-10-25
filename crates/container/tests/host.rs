//! G08-02 acceptance: host lifecycle, isolation policy and budgets.

use std::collections::BTreeMap;

use wxlike_container::host::budget::BudgetLimits;
use wxlike_container::host::profile::{wasm_decision, DeviceTarget, WasmDecision};
use wxlike_container::host::{HostError, HostState, MiniHost};
use wxlike_dpage::{Manifest, Page};

fn page(script: &str) -> Page {
    let manifest = Manifest {
        name: "demo".into(),
        main_js: "index.js".into(),
        title: String::new(),
    };
    let files = BTreeMap::from([("index.js".to_string(), script.as_bytes().to_vec())]);
    Page {
        id: 1,
        manifest,
        files,
    }
}

#[test]
fn runs_a_page_and_returns_the_guest_title() {
    let mut host = MiniHost::launch_default(DeviceTarget::Desktop, "com.wxlike.notes").unwrap();
    let script = "globalThis.onLoad = function(){ wxhost.setTitle('Notes'); }; \
                  globalThis.onShow = function(){};";
    assert_eq!(host.run(&page(script), "{}").unwrap(), "Notes");
    assert_eq!(host.state(), HostState::Running);
}

#[test]
fn infinite_loop_is_terminated_as_a_cpu_budget_failure() {
    let mut host = MiniHost::launch_default(DeviceTarget::Desktop, "com.wxlike.loop").unwrap();
    let err = host
        .run(&page("let x = 0; while (true) { x += 1; }"), "{}")
        .unwrap_err();
    assert_eq!(err, HostError::BudgetExceeded("cpu"));
    assert_eq!(host.state(), HostState::Terminated);
}

#[test]
fn oversized_payload_is_refused_before_evaluation() {
    let mut limits = BudgetLimits::desktop();
    limits.memory_bytes = 8;
    let mut host = MiniHost::launch(DeviceTarget::Desktop, "com.wxlike.big", limits).unwrap();
    let err = host.run(&page("wxhost.log('hello');"), "{}").unwrap_err();
    assert_eq!(err, HostError::BudgetExceeded("memory"));
    assert_eq!(host.state(), HostState::Terminated);
}

#[test]
fn cpu_budget_terminates_after_the_call() {
    let mut limits = BudgetLimits::desktop();
    limits.cpu_millis = 0;
    let mut host = MiniHost::launch(DeviceTarget::Desktop, "com.wxlike.cpu", limits).unwrap();
    let err = host.run(&page("wxhost.log('x');"), "{}").unwrap_err();
    assert_eq!(err, HostError::BudgetExceeded("cpu"));
    assert_eq!(host.state(), HostState::Terminated);
}

#[test]
fn guest_script_errors_do_not_kill_the_host() {
    let mut host = MiniHost::launch_default(DeviceTarget::Desktop, "com.wxlike.syntax").unwrap();
    let err = host.run(&page("this is not javascript"), "{}").unwrap_err();
    assert!(matches!(err, HostError::Guest(_)));
    assert_eq!(host.state(), HostState::Running);
}

#[test]
fn paused_or_terminated_hosts_refuse_to_run() {
    let mut host = MiniHost::launch_default(DeviceTarget::Web, "com.wxlike.pause").unwrap();
    host.pause().unwrap();
    assert_eq!(
        host.run(&page("wxhost.log('x');"), "{}").unwrap_err(),
        HostError::InvalidState("paused", "running")
    );
    host.resume().unwrap();
    host.terminate().unwrap();
    assert_eq!(
        host.run(&page("wxhost.log('x');"), "{}").unwrap_err(),
        HostError::InvalidState("terminated", "running")
    );
}

#[test]
fn wasm_policy_is_target_and_allowlist_driven() {
    assert!(matches!(
        wasm_decision(DeviceTarget::Ios, Some("24.0.4"), &["24.0.4"]).unwrap(),
        WasmDecision::Disabled("ios")
    ));
    assert!(matches!(
        wasm_decision(DeviceTarget::Android, Some("24.0.4"), &["24.0.4"]).unwrap(),
        WasmDecision::Allowed
    ));
    assert!(matches!(
        wasm_decision(DeviceTarget::Desktop, Some("22.0.0"), &["24.0.4"]),
        Err(HostError::WasmVersionNotAllowed(_))
    ));
}

#[test]
fn profiles_keep_each_target_isolated() {
    let desktop = MiniHost::launch_default(DeviceTarget::Desktop, "app.one").unwrap();
    let web = MiniHost::launch_default(DeviceTarget::Web, "app.one").unwrap();
    let ios = MiniHost::launch_default(DeviceTarget::Ios, "app.one").unwrap();

    assert!(!desktop.profile().allow_main_ipc);
    assert!(!desktop.profile().bridge_allowed("tauri.invoke"));
    assert_ne!(
        desktop.profile().storage_partition,
        web.profile().storage_partition
    );
    assert!(desktop.profile().webview_data_dir().is_some());
    assert!(ios.profile().webview_data_dir().is_some());
    assert!(web.profile().webview_data_dir().is_none());
    assert!(web.profile().iframe_sandbox.is_some());
    assert!(!ios.profile().cookie_visible("wxlike.app"));
}
