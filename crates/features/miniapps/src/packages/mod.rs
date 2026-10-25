//! Package format, publisher trust and installation.

pub mod error;
pub mod format;
pub mod install;
pub mod manifest;
pub mod trust;

pub use error::{MiniError, Result};
pub use format::{
    build_signed, open, pack_unsigned, sign_blob, verify, PackedPackage, SignedPackage,
    VerifiedPackage,
};
pub use install::{FaultPoint, Faults, InstallOutcome, InstalledApp, Installer, NoFaults};
pub use manifest::{Channel, Manifest, CAPABILITIES, MANIFEST_FILE};
pub use trust::{
    decode_public_key, generate_key, issue_cert, key_id_for, sign_manifest, verify_detached,
    PackageSignature, PublisherCert, SignedCert, TrustStore,
};
