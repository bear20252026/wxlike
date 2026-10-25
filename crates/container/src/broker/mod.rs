//! Permission broker: the only path from guest code to host capabilities.
//!
//! Every call carries an app id, an origin and a session id. All three are
//! treated as untrusted strings and compared against the launched session:
//! a forged origin, a stale session after navigation, or a replayed request
//! id fails before any port is touched. Scopes are granted at install time
//! from the signed manifest; sensitive scopes still prompt on every use.

pub mod calls;
pub mod grants;
pub mod ports;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::host::budget::BudgetLedger;
use crate::host::profile::IsolationProfile;
use crate::host::HostError;

use calls::{
    validate_amount, validate_currency, validate_network_url, validate_order_id,
    validate_request_id, HttpMethod, TypedCall, MAX_NETWORK_BODY, MAX_RESPONSE_BYTES,
    MAX_VALUE_BYTES,
};
use grants::{AppSession, GuestIdentity, Perm};
use ports::{
    BrokerPorts, Confirmation, ConfirmationRequest, FileHandle, HttpResponse, Location,
    NetworkRequest, PaymentReceipt, PaymentRequest, Photo,
};

pub const MAX_SESSION_ID_LEN: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BrokerError {
    #[error("unknown app {0:?}")]
    UnknownApp(String),
    #[error("origin does not match the launched app")]
    OriginMismatch,
    #[error("session is closed or was revoked")]
    SessionClosed,
    #[error("session does not belong to this guest")]
    SessionMismatch,
    #[error("request id {0:?} was already used")]
    Replayed(String),
    #[error("scope {0:?} is not granted")]
    NotGranted(Perm),
    #[error("user denied {0:?}")]
    Denied(Perm),
    #[error("user cancelled {0:?}")]
    Cancelled(Perm),
    #[error("invalid call: {0}")]
    InvalidCall(String),
    #[error("budget exceeded: {0}")]
    Budget(&'static str),
    #[error("host error: {0}")]
    Host(HostError),
    #[error("{method} failed: {message}")]
    Port {
        method: &'static str,
        message: String,
    },
}

impl From<HostError> for BrokerError {
    fn from(err: HostError) -> Self {
        match err {
            HostError::BudgetExceeded(kind) => BrokerError::Budget(kind),
            other => BrokerError::Host(other),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditOutcome {
    Ok,
    Denied,
    Cancelled,
    Failed,
}

/// One audit line. It names the method and scope but never the value, URL
/// body, amount, file name or location the guest supplied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEntry {
    pub app_id: String,
    pub session_id: String,
    pub request_id: String,
    pub method: &'static str,
    pub scope: Perm,
    pub outcome: AuditOutcome,
}

pub struct Broker {
    profile: IsolationProfile,
    sessions: BTreeMap<String, AppSession>,
    grants: BTreeSet<Perm>,
    used_request_ids: BTreeMap<String, BTreeSet<String>>,
    ports: BrokerPorts,
    audit: Vec<AuditEntry>,
}

impl Broker {
    pub fn new(profile: IsolationProfile) -> Self {
        Self::with_ports(profile, BrokerPorts::default())
    }

    pub fn with_ports(profile: IsolationProfile, ports: BrokerPorts) -> Self {
        Self {
            profile,
            sessions: BTreeMap::new(),
            grants: BTreeSet::new(),
            used_request_ids: BTreeMap::new(),
            ports,
            audit: Vec::new(),
        }
    }

    pub fn profile(&self) -> &IsolationProfile {
        &self.profile
    }

    pub fn ports(&self) -> &BrokerPorts {
        &self.ports
    }

    pub fn ports_mut(&mut self) -> &mut BrokerPorts {
        &mut self.ports
    }

    pub fn audit(&self) -> &[AuditEntry] {
        &self.audit
    }

    pub fn open_session(&mut self, session_id: &str) -> Result<(), BrokerError> {
        if session_id.is_empty()
            || session_id.len() > MAX_SESSION_ID_LEN
            || !session_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(BrokerError::InvalidCall("session id".into()));
        }
        self.sessions.insert(
            session_id.to_string(),
            AppSession::new(&self.profile.app_id, &self.profile.origin, session_id),
        );
        Ok(())
    }

    pub fn close_session(&mut self, session_id: &str) {
        self.sessions.remove(session_id);
        self.used_request_ids.remove(session_id);
    }

    /// A navigation is only allowed inside the app's own origin. Anything
    /// else closes every session, so the guest cannot keep calling from a
    /// page it navigated to.
    pub fn navigate(&mut self, url: &str) -> Result<(), BrokerError> {
        if self.profile.navigation_allowed(url) {
            return Ok(());
        }
        self.sessions.clear();
        Err(BrokerError::OriginMismatch)
    }

    pub fn grant(&mut self, perms: &[Perm]) {
        self.grants.extend(perms.iter().copied());
    }

    /// Grant from the signed manifest's capability names. Unknown names are
    /// refused rather than silently ignored.
    pub fn grant_capabilities(&mut self, capabilities: &[String]) -> Result<(), BrokerError> {
        for capability in capabilities {
            let perm = Perm::from_capability(capability).ok_or_else(|| {
                BrokerError::InvalidCall(format!("unknown capability {capability:?}"))
            })?;
            self.grants.insert(perm);
        }
        Ok(())
    }

    pub fn revoke_scope(&mut self, perm: Perm) {
        self.grants.remove(&perm);
    }

    /// Revoke everything: grants, sessions and in-flight request ids.
    pub fn revoke_app(&mut self) {
        self.grants.clear();
        self.sessions.clear();
        self.used_request_ids.clear();
    }

    pub fn request(
        &mut self,
        budgets: &mut BudgetLedger,
        guest: &GuestIdentity,
        request_id: &str,
        call: TypedCall,
    ) -> Result<Value, BrokerError> {
        self.authenticate(guest)?;
        validate_request_id(request_id)?;
        if self
            .used_request_ids
            .get(&guest.session_id)
            .is_some_and(|ids| ids.contains(request_id))
        {
            return Err(BrokerError::Replayed(request_id.to_string()));
        }
        budgets.charge_bridge_call()?;
        let perm = call.perm();
        let method = call.method();
        if !self.grants.contains(&perm) {
            self.record(guest, request_id, method, perm, AuditOutcome::Denied);
            return Err(BrokerError::NotGranted(perm));
        }
        if perm.needs_confirmation() {
            let prompt = ConfirmationRequest {
                app_id: self.profile.app_id.clone(),
                scope: perm,
                summary: self.summary_for(&call),
            };
            match self.ports.confirmer.confirm(&prompt) {
                Confirmation::Allow => {}
                Confirmation::Deny => {
                    self.record(guest, request_id, method, perm, AuditOutcome::Denied);
                    return Err(BrokerError::Denied(perm));
                }
                Confirmation::Cancel => {
                    self.record(guest, request_id, method, perm, AuditOutcome::Cancelled);
                    return Err(BrokerError::Cancelled(perm));
                }
            }
        }
        self.used_request_ids
            .entry(guest.session_id.clone())
            .or_default()
            .insert(request_id.to_string());
        let result = self.execute(budgets, call);
        let outcome = match &result {
            Ok(_) => AuditOutcome::Ok,
            Err(_) => AuditOutcome::Failed,
        };
        self.record(guest, request_id, method, perm, outcome);
        result
    }

    fn authenticate(&self, guest: &GuestIdentity) -> Result<(), BrokerError> {
        if guest.app_id != self.profile.app_id {
            return Err(BrokerError::UnknownApp(guest.app_id.clone()));
        }
        if guest.origin != self.profile.origin {
            return Err(BrokerError::OriginMismatch);
        }
        let session = self
            .sessions
            .get(&guest.session_id)
            .ok_or(BrokerError::SessionClosed)?;
        if session.app_id != guest.app_id || session.origin != guest.origin {
            return Err(BrokerError::SessionMismatch);
        }
        Ok(())
    }

    fn record(
        &mut self,
        guest: &GuestIdentity,
        request_id: &str,
        method: &'static str,
        scope: Perm,
        outcome: AuditOutcome,
    ) {
        self.audit.push(AuditEntry {
            app_id: self.profile.app_id.clone(),
            session_id: guest.session_id.clone(),
            request_id: request_id.to_string(),
            method,
            scope,
            outcome,
        });
    }

    fn summary_for(&self, call: &TypedCall) -> String {
        match call {
            TypedCall::PayRequest {
                amount, currency, ..
            } => format!("{} wants to pay {amount} {currency}", self.profile.app_id),
            _ => format!(
                "{} wants to use {}",
                self.profile.app_id,
                call.perm().as_str()
            ),
        }
    }

    fn execute(
        &mut self,
        budgets: &mut BudgetLedger,
        call: TypedCall,
    ) -> Result<Value, BrokerError> {
        match call {
            TypedCall::StorageGet { key } => self.storage_get(&key),
            TypedCall::StorageSet { key, value } => self.storage_set(budgets, &key, &value),
            TypedCall::StorageRemove { key } => self.storage_remove(budgets, &key),
            TypedCall::NetworkRequest { url, method, body } => {
                self.network_request(budgets, url, method, body)
            }
            TypedCall::CameraCapture {} => self.camera_capture(),
            TypedCall::LocationGet {} => self.location_get(),
            TypedCall::FilePick { purpose } => self.file_pick(purpose),
            TypedCall::PayRequest {
                amount,
                currency,
                order_id,
            } => self.pay_request(&amount, &currency, &order_id),
        }
    }

    fn storage_namespace(&self) -> String {
        self.profile.storage_partition.clone()
    }

    fn check_storage_key(&self, key: &str) -> Result<(), BrokerError> {
        self.profile
            .storage_key(key)
            .map(|_| ())
            .map_err(|_| BrokerError::InvalidCall("storage key".into()))
    }

    fn storage_get(&mut self, key: &str) -> Result<Value, BrokerError> {
        self.check_storage_key(key)?;
        let namespace = self.storage_namespace();
        let value = self
            .ports
            .storage
            .get(&namespace, key)
            .map_err(|message| port_error("storage.get", message))?;
        Ok(value.map(Value::String).unwrap_or(Value::Null))
    }

    fn storage_set(
        &mut self,
        budgets: &mut BudgetLedger,
        key: &str,
        value: &str,
    ) -> Result<Value, BrokerError> {
        self.check_storage_key(key)?;
        if value.len() > MAX_VALUE_BYTES {
            return Err(BrokerError::InvalidCall("storage value too large".into()));
        }
        let namespace = self.storage_namespace();
        let bytes = value.len() as u64;
        budgets.charge_storage(bytes)?;
        let previous = match self.ports.storage.set(&namespace, key, value) {
            Ok(previous) => previous,
            Err(message) => {
                budgets.release_storage(bytes);
                return Err(port_error("storage.set", message));
            }
        };
        if let Some(previous) = previous {
            budgets.release_storage(previous as u64);
        }
        Ok(json!(true))
    }

    fn storage_remove(
        &mut self,
        budgets: &mut BudgetLedger,
        key: &str,
    ) -> Result<Value, BrokerError> {
        self.check_storage_key(key)?;
        let namespace = self.storage_namespace();
        let previous = self
            .ports
            .storage
            .remove(&namespace, key)
            .map_err(|message| port_error("storage.remove", message))?;
        if let Some(previous) = previous {
            budgets.release_storage(previous as u64);
        }
        Ok(json!(previous.is_some()))
    }

    fn network_request(
        &mut self,
        budgets: &mut BudgetLedger,
        url: String,
        method: HttpMethod,
        body: Option<String>,
    ) -> Result<Value, BrokerError> {
        let host = validate_network_url(&url)?;
        let body = body.unwrap_or_default();
        if body.len() > MAX_NETWORK_BODY {
            return Err(BrokerError::InvalidCall("network body too large".into()));
        }
        budgets.charge_network((url.len() + body.len()) as u64)?;
        let response = self
            .ports
            .network
            .request(&NetworkRequest {
                host,
                url,
                method,
                body,
            })
            .map_err(|message| port_error("network.request", message))?;
        if response.body.len() > MAX_RESPONSE_BYTES {
            return Err(port_error(
                "network.request",
                "response body too large".into(),
            ));
        }
        budgets.charge_network(response.body.len() as u64)?;
        Ok(network_value(response))
    }

    fn camera_capture(&mut self) -> Result<Value, BrokerError> {
        let photo: Photo = self
            .ports
            .devices
            .capture_photo()
            .map_err(|message| port_error("camera.capture", message))?;
        Ok(json!({ "reference": photo.reference }))
    }

    fn location_get(&mut self) -> Result<Value, BrokerError> {
        let location: Location = self
            .ports
            .devices
            .location()
            .map_err(|message| port_error("location.get", message))?;
        Ok(json!({
            "latitude": coarse(location.latitude),
            "longitude": coarse(location.longitude),
        }))
    }

    fn file_pick(&mut self, purpose: calls::FilePurpose) -> Result<Value, BrokerError> {
        let handle: FileHandle = self
            .ports
            .devices
            .pick_file(purpose)
            .map_err(|message| port_error("file.pick", message))?;
        Ok(json!({
            "name": handle.name,
            "size": handle.size,
            "reference": handle.reference,
        }))
    }

    fn pay_request(
        &mut self,
        amount: &str,
        currency: &str,
        order_id: &str,
    ) -> Result<Value, BrokerError> {
        validate_amount(amount)?;
        validate_currency(currency)?;
        validate_order_id(order_id)?;
        let request = PaymentRequest {
            app_id: self.profile.app_id.clone(),
            amount: amount.to_string(),
            currency: currency.to_string(),
            order_id: order_id.to_string(),
        };
        let receipt: PaymentReceipt = self
            .ports
            .payments
            .pay(&request)
            .map_err(|message| port_error("pay.request", message))?;
        Ok(json!({
            "reference": receipt.reference,
            "status": receipt.status.as_str(),
        }))
    }
}

fn network_value(response: HttpResponse) -> Value {
    let body = serde_json::from_str(&response.body).unwrap_or(Value::String(response.body));
    json!({ "status": response.status, "body": body })
}

/// Coarser than the host's own location: two decimals is roughly a
/// neighbourhood, not a doorstep.
fn coarse(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn port_error(method: &'static str, message: String) -> BrokerError {
    BrokerError::Port { method, message }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::budget::BudgetLimits;
    use crate::host::profile::DeviceTarget;

    fn profile() -> IsolationProfile {
        IsolationProfile::new(DeviceTarget::Desktop, "com.demo.app").unwrap()
    }

    #[test]
    fn coarsening_rounds_to_two_decimals() {
        assert!((coarse(31.230416) - 31.23).abs() < f64::EPSILON);
        assert!((coarse(121.473701) - 121.47).abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_capability_is_refused() {
        let mut broker = Broker::new(profile());
        assert!(broker
            .grant_capabilities(&["storage".into(), "root".into()])
            .is_err());
        assert!(broker.grants.contains(&Perm::Storage));
    }

    #[test]
    fn budget_errors_keep_their_kind() {
        let err = BrokerError::from(HostError::BudgetExceeded("network"));
        assert_eq!(err, BrokerError::Budget("network"));
        assert_eq!(BudgetLimits::desktop().bridge_calls, 512);
    }
}
