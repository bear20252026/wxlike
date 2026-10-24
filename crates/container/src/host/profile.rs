//! Isolation profiles: what a guest app is allowed to reach on each target.
//!
//! The profile is the host's side of ADR-06. It never grants the main Tauri
//! IPC, the host session token, cookies from the main domain, or direct
//! network access; guest code reaches capabilities only through the typed
//! broker (G08-03).

use super::HostError;

pub const MAX_APP_ID_LEN: usize = 64;
pub const MAX_STORAGE_KEY_LEN: usize = 128;

/// Typed bridge surface. Anything not in this list (notably `tauri.*`) is
/// not callable, regardless of what the guest asks for.
pub const BRIDGE_METHODS: [&str; 8] = [
    "storage.get",
    "storage.set",
    "storage.remove",
    "network.request",
    "camera.capture",
    "location.get",
    "file.pick",
    "pay.request",
];

/// No inline script, no eval, no direct fetch, no framing, no navigation.
/// Network capability is mediated by the broker, so `connect-src 'none'`
/// is intentional rather than a placeholder.
pub const MINI_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
img-src 'self' data:; media-src 'self'; font-src 'self'; connect-src 'none'; \
worker-src 'none'; object-src 'none'; frame-src 'none'; frame-ancestors 'none'; \
base-uri 'none'; form-action 'none'";

pub const WEB_HOST_SUFFIX: &str = ".mini.wxlike.app";
pub const NATIVE_SCHEME: &str = "wxmini";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceTarget {
    Desktop,
    Web,
    Ios,
    Android,
}

impl DeviceTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceTarget::Desktop => "desktop",
            DeviceTarget::Web => "web",
            DeviceTarget::Ios => "ios",
            DeviceTarget::Android => "android",
        }
    }

    /// Targets that may load a Wasmtime module at all. iOS must not depend
    /// on JIT, and the web host has no Wasmtime engine by design.
    pub fn wasm_capable(self) -> bool {
        matches!(self, DeviceTarget::Desktop | DeviceTarget::Android)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IsolationProfile {
    pub target: DeviceTarget,
    pub app_id: String,
    pub origin: String,
    pub csp: String,
    pub storage_partition: String,
    pub webview_data_dir: Option<String>,
    pub iframe_sandbox: Option<String>,
    /// Always false; kept explicit so call sites cannot "assume true".
    pub allow_main_ipc: bool,
}

impl IsolationProfile {
    pub fn new(target: DeviceTarget, app_id: &str) -> Result<Self, HostError> {
        validate_app_id(app_id)?;
        let (origin, webview_data_dir, iframe_sandbox) = match target {
            DeviceTarget::Web => (
                format!("https://{app_id}{WEB_HOST_SUFFIX}"),
                None,
                Some("allow-scripts".to_string()),
            ),
            _ => (
                format!("{NATIVE_SCHEME}://{app_id}"),
                Some(format!("mini-{app_id}")),
                None,
            ),
        };
        Ok(Self {
            target,
            app_id: app_id.to_string(),
            origin,
            csp: MINI_CSP.to_string(),
            storage_partition: format!("mini:{target_str}:{app_id}", target_str = target.as_str()),
            webview_data_dir,
            iframe_sandbox,
            allow_main_ipc: false,
        })
    }

    pub fn bridge_allowed(&self, method: &str) -> bool {
        BRIDGE_METHODS.contains(&method)
    }

    /// Only the app's own origin may be navigated to. A guest that changes
    /// navigation loses its session in the broker (G08-03).
    pub fn navigation_allowed(&self, url: &str) -> bool {
        url == self.origin || url.starts_with(&format!("{}/", self.origin))
    }

    /// Cookies are partitioned per WebView origin; no parent-domain cookie
    /// of the main app is visible from here.
    pub fn cookie_visible(&self, domain: &str) -> bool {
        match self.target {
            DeviceTarget::Web => domain == format!("{}{}", self.app_id, WEB_HOST_SUFFIX),
            _ => false,
        }
    }

    /// Storage keys are namespaced by app so one guest cannot read or clear
    /// another guest's data.
    pub fn storage_key(&self, key: &str) -> Result<String, HostError> {
        if key.is_empty() || key.len() > MAX_STORAGE_KEY_LEN || key.contains('\0') {
            return Err(HostError::BadStorageKey);
        }
        Ok(format!("{}::{key}", self.storage_partition))
    }

    pub fn webview_data_dir(&self) -> Option<&str> {
        self.webview_data_dir.as_deref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WasmDecision {
    Disabled(&'static str),
    Allowed,
}

/// Wasmtime is optional and only a version explicitly on the host allowlist
/// may be enabled. The allowlist is supplied by the build/deployment, not by
/// the guest or the package manifest.
pub fn wasm_decision(
    target: DeviceTarget,
    version: Option<&str>,
    allowed_versions: &[&str],
) -> Result<WasmDecision, HostError> {
    if !target.wasm_capable() {
        return Ok(WasmDecision::Disabled(target.as_str()));
    }
    let Some(version) = version else {
        return Ok(WasmDecision::Disabled("not-configured"));
    };
    if allowed_versions.contains(&version) {
        Ok(WasmDecision::Allowed)
    } else {
        Err(HostError::WasmVersionNotAllowed(version.to_string()))
    }
}

fn validate_app_id(app_id: &str) -> Result<(), HostError> {
    if app_id.is_empty() || app_id.len() > MAX_APP_ID_LEN {
        return Err(HostError::BadAppId);
    }
    let valid = app_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-');
    if !valid || app_id.starts_with('.') || app_id.starts_with('-') {
        return Err(HostError::BadAppId);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_cannot_reach_the_main_ipc_or_main_cookies() {
        let profile = IsolationProfile::new(DeviceTarget::Desktop, "com.wxlike.notes").unwrap();
        assert!(!profile.allow_main_ipc);
        assert!(!profile.bridge_allowed("tauri.invoke"));
        assert!(profile.bridge_allowed("storage.get"));
        assert!(!profile.cookie_visible("wxlike.app"));
        assert!(!profile.cookie_visible("mini.wxlike.app"));
    }

    #[test]
    fn web_is_origin_isolated_and_sandboxed() {
        let profile = IsolationProfile::new(DeviceTarget::Web, "com.wxlike.notes").unwrap();
        assert_eq!(profile.origin, "https://com.wxlike.notes.mini.wxlike.app");
        assert_eq!(profile.iframe_sandbox.as_deref(), Some("allow-scripts"));
        assert!(profile.cookie_visible("com.wxlike.notes.mini.wxlike.app"));
        assert!(!profile.cookie_visible("other.mini.wxlike.app"));
        assert!(profile.webview_data_dir().is_none());
    }

    #[test]
    fn navigation_is_limited_to_the_app_origin() {
        let profile = IsolationProfile::new(DeviceTarget::Web, "com.wxlike.notes").unwrap();
        assert!(profile.navigation_allowed("https://com.wxlike.notes.mini.wxlike.app/index"));
        assert!(!profile.navigation_allowed("https://evil.example/"));
        assert!(!profile.navigation_allowed("https://other.mini.wxlike.app/"));
        assert!(!profile.navigation_allowed("javascript:alert(1)"));
        assert!(!profile.navigation_allowed("https://com.wxlike.notes.mini.wxlike.app.evil/"));
    }

    #[test]
    fn storage_is_namespaced_per_app() {
        let a = IsolationProfile::new(DeviceTarget::Desktop, "app.one").unwrap();
        let b = IsolationProfile::new(DeviceTarget::Desktop, "app.two").unwrap();
        assert_ne!(a.storage_key("k").unwrap(), b.storage_key("k").unwrap());
        assert!(a.storage_key("").is_err());
        assert!(a.storage_key(&"k".repeat(MAX_STORAGE_KEY_LEN + 1)).is_err());
    }

    #[test]
    fn csp_forbids_eval_wildcards_and_direct_network() {
        let profile = IsolationProfile::new(DeviceTarget::Desktop, "app.one").unwrap();
        assert!(!profile.csp.contains("unsafe-eval"));
        assert!(!profile.csp.contains("unsafe-inline"));
        assert!(!profile.csp.contains('*'));
        assert!(profile.csp.contains("default-src 'none'"));
        assert!(profile.csp.contains("connect-src 'none'"));
        assert!(profile.csp.contains("frame-ancestors 'none'"));
    }

    #[test]
    fn wasm_is_off_on_ios_and_web_and_allowlisted_elsewhere() {
        assert_eq!(
            wasm_decision(DeviceTarget::Ios, Some("24.0.4"), &["24.0.4"]).unwrap(),
            WasmDecision::Disabled("ios")
        );
        assert_eq!(
            wasm_decision(DeviceTarget::Web, Some("24.0.4"), &["24.0.4"]).unwrap(),
            WasmDecision::Disabled("web")
        );
        assert_eq!(
            wasm_decision(DeviceTarget::Desktop, None, &[]).unwrap(),
            WasmDecision::Disabled("not-configured")
        );
        assert_eq!(
            wasm_decision(DeviceTarget::Desktop, Some("24.0.4"), &["24.0.4"]).unwrap(),
            WasmDecision::Allowed
        );
        assert!(matches!(
            wasm_decision(DeviceTarget::Desktop, Some("23.0.0"), &["24.0.4"]),
            Err(HostError::WasmVersionNotAllowed(_))
        ));
    }
}
