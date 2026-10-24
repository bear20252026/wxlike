//! Platform ports the broker delegates to.
//!
//! The broker owns policy (who may call what, when a prompt is required,
//! what is audited). The platform shell owns the actual side effects: local
//! storage, HTTP, camera/location/file pickers and payments (G06). Ports
//! return plain data; none of them hands a host credential or a raw path
//! back to guest code.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::calls::{FilePurpose, HttpMethod};
use super::grants::Perm;

pub const MAX_PHOTO_REFERENCE: usize = 256;
pub const MAX_FILE_NAME: usize = 200;
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmationRequest {
    pub app_id: String,
    pub scope: Perm,
    /// Human-readable line shown in the OS/browser prompt. Never contains
    /// a token or a raw file path.
    pub summary: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmation {
    Allow,
    Deny,
    Cancel,
}

pub trait Confirmer {
    fn confirm(&mut self, request: &ConfirmationRequest) -> Confirmation;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkRequest {
    pub host: String,
    pub url: String,
    pub method: HttpMethod,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub trait NetworkPort {
    fn request(&mut self, request: &NetworkRequest) -> Result<HttpResponse, String>;
}

/// Storage is keyed by the host-assigned namespace; the guest never sees or
/// chooses the namespace, so two apps cannot address the same row.
pub trait StoragePort {
    fn get(&mut self, namespace: &str, key: &str) -> Result<Option<String>, String>;
    /// Returns the previous value length so the broker can refund its budget.
    fn set(&mut self, namespace: &str, key: &str, value: &str) -> Result<Option<usize>, String>;
    fn remove(&mut self, namespace: &str, key: &str) -> Result<Option<usize>, String>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Photo {
    /// Opaque handle the host resolves; no file path and no camera buffer.
    pub reference: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHandle {
    pub name: String,
    pub size: u64,
    /// Opaque handle. The chosen path stays in the platform picker.
    pub reference: String,
}

pub trait DevicePort {
    fn capture_photo(&mut self) -> Result<Photo, String>;
    fn location(&mut self) -> Result<Location, String>;
    fn pick_file(&mut self, purpose: FilePurpose) -> Result<FileHandle, String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentRequest {
    pub app_id: String,
    pub amount: String,
    pub currency: String,
    pub order_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaymentStatus {
    Settled,
    Pending,
}

impl PaymentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PaymentStatus::Settled => "settled",
            PaymentStatus::Pending => "pending",
        }
    }
}

/// What a guest gets back from a payment. Deliberately no order token, no
/// credential, no internal transaction id — those stay inside G06.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentReceipt {
    pub reference: String,
    pub status: PaymentStatus,
}

pub trait PaymentPort {
    fn pay(&mut self, request: &PaymentRequest) -> Result<PaymentReceipt, String>;
}

#[derive(Default)]
pub struct MemoryStorage {
    rows: HashMap<(String, String), String>,
}

impl StoragePort for MemoryStorage {
    fn get(&mut self, namespace: &str, key: &str) -> Result<Option<String>, String> {
        Ok(self
            .rows
            .get(&(namespace.to_string(), key.to_string()))
            .cloned())
    }

    fn set(&mut self, namespace: &str, key: &str, value: &str) -> Result<Option<usize>, String> {
        let previous = self
            .rows
            .insert((namespace.to_string(), key.to_string()), value.to_string())
            .map(|old| old.len());
        Ok(previous)
    }

    fn remove(&mut self, namespace: &str, key: &str) -> Result<Option<usize>, String> {
        Ok(self
            .rows
            .remove(&(namespace.to_string(), key.to_string()))
            .map(|old| old.len()))
    }
}

pub struct NoNetwork;

impl NetworkPort for NoNetwork {
    fn request(&mut self, _request: &NetworkRequest) -> Result<HttpResponse, String> {
        Err("network port is not configured".into())
    }
}

pub struct NoDevices;

impl DevicePort for NoDevices {
    fn capture_photo(&mut self) -> Result<Photo, String> {
        Err("camera is not available on this host".into())
    }

    fn location(&mut self) -> Result<Location, String> {
        Err("location is not available on this host".into())
    }

    fn pick_file(&mut self, _purpose: FilePurpose) -> Result<FileHandle, String> {
        Err("file picker is not available on this host".into())
    }
}

pub struct NoPayments;

impl PaymentPort for NoPayments {
    fn pay(&mut self, _request: &PaymentRequest) -> Result<PaymentReceipt, String> {
        Err("payments are not configured (G06 port missing)".into())
    }
}

pub struct DenyAll;

impl Confirmer for DenyAll {
    fn confirm(&mut self, _request: &ConfirmationRequest) -> Confirmation {
        Confirmation::Deny
    }
}

pub struct BrokerPorts {
    pub confirmer: Box<dyn Confirmer>,
    pub storage: Box<dyn StoragePort>,
    pub network: Box<dyn NetworkPort>,
    pub devices: Box<dyn DevicePort>,
    pub payments: Box<dyn PaymentPort>,
}

impl Default for BrokerPorts {
    fn default() -> Self {
        Self {
            confirmer: Box::new(DenyAll),
            storage: Box::new(MemoryStorage::default()),
            network: Box::new(NoNetwork),
            devices: Box::new(NoDevices),
            payments: Box::new(NoPayments),
        }
    }
}

/// In-memory doubles for tests and the demo app. They record calls so a test
/// can assert what crossed the bridge; they never emulate a real vendor.
pub mod fixtures {
    use super::*;

    pub struct AllowAll;

    impl Confirmer for AllowAll {
        fn confirm(&mut self, _request: &ConfirmationRequest) -> Confirmation {
            Confirmation::Allow
        }
    }

    pub struct CancelAll;

    impl Confirmer for CancelAll {
        fn confirm(&mut self, _request: &ConfirmationRequest) -> Confirmation {
            Confirmation::Cancel
        }
    }

    /// Answers every allowlisted request with a JSON echo of method and URL.
    pub struct EchoNetwork;

    impl NetworkPort for EchoNetwork {
        fn request(&mut self, request: &NetworkRequest) -> Result<HttpResponse, String> {
            let body = serde_json::json!({
                "host": request.host,
                "method": request.method,
                "url": request.url,
            });
            Ok(HttpResponse {
                status: 200,
                body: body.to_string(),
            })
        }
    }

    pub struct RecordingPayments {
        calls: Arc<Mutex<Vec<PaymentRequest>>>,
    }

    impl RecordingPayments {
        pub fn new() -> (Self, PaymentLog) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    calls: Arc::clone(&calls),
                },
                PaymentLog { calls },
            )
        }
    }

    impl PaymentPort for RecordingPayments {
        fn pay(&mut self, request: &PaymentRequest) -> Result<PaymentReceipt, String> {
            let mut calls = self.calls.lock().unwrap();
            calls.push(request.clone());
            Ok(PaymentReceipt {
                reference: format!("receipt-{}", calls.len()),
                status: PaymentStatus::Settled,
            })
        }
    }

    pub struct PaymentLog {
        calls: Arc<Mutex<Vec<PaymentRequest>>>,
    }

    impl PaymentLog {
        pub fn entries(&self) -> Vec<PaymentRequest> {
            self.calls.lock().unwrap().clone()
        }
    }

    /// Fixed, deterministic data so assertions do not depend on a device.
    pub struct RecordingDevices {
        prompts: Arc<Mutex<Vec<FilePurpose>>>,
    }

    impl RecordingDevices {
        pub fn new() -> (Self, DeviceLog) {
            let prompts = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    prompts: Arc::clone(&prompts),
                },
                DeviceLog { prompts },
            )
        }
    }

    impl DevicePort for RecordingDevices {
        fn capture_photo(&mut self) -> Result<Photo, String> {
            Ok(Photo {
                reference: "photo-1".into(),
            })
        }

        fn location(&mut self) -> Result<Location, String> {
            Ok(Location {
                latitude: 31.230416,
                longitude: 121.473701,
            })
        }

        fn pick_file(&mut self, purpose: FilePurpose) -> Result<FileHandle, String> {
            self.prompts.lock().unwrap().push(purpose);
            Ok(FileHandle {
                name: "lease.pdf".into(),
                size: 2048,
                reference: "file-1".into(),
            })
        }
    }

    pub struct DeviceLog {
        prompts: Arc<Mutex<Vec<FilePurpose>>>,
    }

    impl DeviceLog {
        pub fn prompts(&self) -> Vec<FilePurpose> {
            self.prompts.lock().unwrap().clone()
        }
    }
}
