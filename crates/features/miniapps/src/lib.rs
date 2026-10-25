//! Mini-app packaging, trust, installation and catalog (G08).
//!
//! The crate owns everything between "a directory of web assets" and
//! "a host can launch version X of app Y": signed manifests with per-resource
//! digests, publisher certificates with revocation, crash-safe installation
//! with a version watermark, and the reviewed catalog that decides what may
//! be installed at all.

pub mod catalog;
pub mod packages;
pub mod toolchain;

pub use packages::error::{MiniError, Result};
