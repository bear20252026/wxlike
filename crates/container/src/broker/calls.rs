//! The typed bridge surface. There is no dynamic dispatch, no shell, no
//! arbitrary URL or file path: every method has a fixed parameter shape and
//! a fixed scope.

use serde::{Deserialize, Serialize};

use super::grants::Perm;
use super::BrokerError;

pub const MAX_VALUE_BYTES: usize = 1024 * 1024;
pub const MAX_NETWORK_BODY: usize = 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_REQUEST_ID_LEN: usize = 128;
pub const MAX_ORDER_ID_LEN: usize = 64;
pub const MAX_AMOUNT_LEN: usize = 12;
pub const AMOUNT_DECIMALS: usize = 2;
pub const CURRENCY_LEN: usize = 3;
pub const NETWORK_ALLOWLIST: [&str; 1] = ["api.wxlike.app"];

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FilePurpose {
    Avatar,
    Attachment,
    Export,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "method", content = "params")]
pub enum TypedCall {
    #[serde(rename = "storage.get")]
    StorageGet { key: String },
    #[serde(rename = "storage.set")]
    StorageSet { key: String, value: String },
    #[serde(rename = "storage.remove")]
    StorageRemove { key: String },
    #[serde(rename = "network.request")]
    NetworkRequest {
        url: String,
        method: HttpMethod,
        #[serde(default)]
        body: Option<String>,
    },
    #[serde(rename = "camera.capture")]
    CameraCapture {},
    #[serde(rename = "location.get")]
    LocationGet {},
    #[serde(rename = "file.pick")]
    FilePick { purpose: FilePurpose },
    #[serde(rename = "pay.request")]
    PayRequest {
        amount: String,
        currency: String,
        order_id: String,
    },
}

impl TypedCall {
    pub fn method(&self) -> &'static str {
        match self {
            TypedCall::StorageGet { .. } => "storage.get",
            TypedCall::StorageSet { .. } => "storage.set",
            TypedCall::StorageRemove { .. } => "storage.remove",
            TypedCall::NetworkRequest { .. } => "network.request",
            TypedCall::CameraCapture {} => "camera.capture",
            TypedCall::LocationGet {} => "location.get",
            TypedCall::FilePick { .. } => "file.pick",
            TypedCall::PayRequest { .. } => "pay.request",
        }
    }

    pub fn perm(&self) -> Perm {
        match self {
            TypedCall::StorageGet { .. }
            | TypedCall::StorageSet { .. }
            | TypedCall::StorageRemove { .. } => Perm::Storage,
            TypedCall::NetworkRequest { .. } => Perm::Network,
            TypedCall::CameraCapture {} => Perm::Camera,
            TypedCall::LocationGet {} => Perm::Location,
            TypedCall::FilePick { .. } => Perm::File,
            TypedCall::PayRequest { .. } => Perm::Payment,
        }
    }
}

pub fn validate_request_id(request_id: &str) -> Result<(), BrokerError> {
    let valid = !request_id.is_empty()
        && request_id.len() <= MAX_REQUEST_ID_LEN
        && request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b));
    if valid {
        Ok(())
    } else {
        Err(BrokerError::InvalidCall("request_id".into()))
    }
}

/// Only HTTPS on an explicitly allowlisted host. Port 443 is implicit; any
/// other port, userinfo, bracketed host or non-HTTPS scheme is refused.
pub fn validate_network_url(url: &str) -> Result<String, BrokerError> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| BrokerError::InvalidCall("network url must be https".into()))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || authority.contains(['[', ']']) {
        return Err(BrokerError::InvalidCall("network authority".into()));
    }
    let host = match authority.split_once(':') {
        Some((host, "443")) => host,
        Some(_) => return Err(BrokerError::InvalidCall("network port must be 443".into())),
        None => authority,
    };
    let host = host.to_ascii_lowercase();
    if !NETWORK_ALLOWLIST.contains(&host.as_str()) {
        return Err(BrokerError::InvalidCall(format!(
            "host {host:?} is not allowed"
        )));
    }
    Ok(host)
}

pub fn validate_amount(amount: &str) -> Result<(), BrokerError> {
    if amount.is_empty() || amount.len() > MAX_AMOUNT_LEN {
        return Err(BrokerError::InvalidCall("amount".into()));
    }
    let (whole, fraction) = amount.split_once('.').unwrap_or((amount, ""));
    let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || fraction.len() > AMOUNT_DECIMALS || !digits(fraction) {
        return Err(BrokerError::InvalidCall("amount".into()));
    }
    if whole == "0" && fraction.bytes().all(|b| b == b'0') {
        return Err(BrokerError::InvalidCall("amount must be positive".into()));
    }
    Ok(())
}

pub fn validate_currency(currency: &str) -> Result<(), BrokerError> {
    let valid = currency.len() == CURRENCY_LEN && currency.bytes().all(|b| b.is_ascii_uppercase());
    if valid {
        Ok(())
    } else {
        Err(BrokerError::InvalidCall("currency".into()))
    }
}

pub fn validate_order_id(order_id: &str) -> Result<(), BrokerError> {
    let valid = !order_id.is_empty()
        && order_id.len() <= MAX_ORDER_ID_LEN
        && order_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b));
    if valid {
        Ok(())
    } else {
        Err(BrokerError::InvalidCall("order_id".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_allowlist_is_exact_and_https_only() {
        assert_eq!(
            validate_network_url("https://api.wxlike.app/v1/quotes?x=1").unwrap(),
            "api.wxlike.app"
        );
        for bad in [
            "http://api.wxlike.app/",
            "https://evil.example/",
            "https://api.wxlike.app.evil/",
            "https://user@api.wxlike.app/",
            "https://api.wxlike.app:8443/",
            "https://[::1]/",
            "https:///",
        ] {
            assert!(validate_network_url(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn amounts_are_validated_without_floats() {
        for ok in ["1", "12.5", "0.01", "999999999.99"] {
            validate_amount(ok).unwrap();
        }
        for bad in ["", "0", "0.00", "-1", "1.234", "1,000", "abc", "1.2.3"] {
            assert!(validate_amount(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn ids_are_restricted_to_safe_characters() {
        validate_request_id("9f8e7d6c-1234").unwrap();
        for bad in ["", "a b", "a\nb", &"x".repeat(MAX_REQUEST_ID_LEN + 1)] {
            assert!(validate_request_id(bad).is_err());
        }
        validate_order_id("order-1").unwrap();
        assert!(validate_order_id("../etc").is_err());
    }

    #[test]
    fn call_methods_map_to_one_scope_each() {
        let call: TypedCall =
            serde_json::from_str(r#"{"method":"camera.capture","params":{}}"#).unwrap();
        assert_eq!(call.method(), "camera.capture");
        assert_eq!(call.perm(), Perm::Camera);
        let smuggled =
            serde_json::from_str::<TypedCall>(r#"{"method":"tauri.invoke","params":{}}"#);
        assert!(smuggled.is_err());
    }
}
