//! Package format, publisher trust and installation.

pub mod error;
pub mod format;
pub mod install;
pub mod manifest;
pub mod trust;

pub use error::{MiniError, Result};
pub use format::{build_signed, sign_blob, verify, SignedPackage, VerifiedPackage};
pub use install::{FaultPoint, Faults, InstallOutcome, InstalledApp, Installer, NoFaults};
pub use manifest::{Channel, Manifest, CAPABILITIES, MANIFEST_FILE};
pub use trust::{
    generate_key, issue_cert, key_id_for, sign_manifest, PackageSignature, PublisherCert,
    SignedCert, TrustStore,
};
