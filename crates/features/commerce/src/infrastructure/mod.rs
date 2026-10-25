//! 基础设施适配器。
//!
//! 生产供应商由组合根注入；本模块只提供确定性的沙箱供应商与故障注入，
//! 让超时/未知状态/重复回执等分支可被真实状态机验证。

pub mod sandbox;

pub use sandbox::{SandboxHandle, SandboxProvider};
