//! `wxlike-discovery` — G07 的扫码、位置、匹配与位置共享域。
//!
//! 域内只依赖端口与纯策略：相机、相册、地图、POI、好友关系由平台/其他域
//! 适配器注入；本 crate 不直接持有摄像头驱动或第三方供应商 SDK。

pub mod clock;
pub mod contact_qr;
pub mod crypto;
pub mod error;
pub mod geo;
pub mod id;
pub mod location_share;
pub mod matching;
pub mod qr;

pub use error::{DiscoveryError, Result};
