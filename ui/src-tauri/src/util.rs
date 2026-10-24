//! 通用小工具（host:port 解析、路径规范化、ID 编解码）。
//!
//! G00-02 纯移动拆分：内容自 lib.rs 原样搬出，签名零改动；
//! 可见性经 lib.rs 的 pub(crate) 再导出 + `use super::*` 保持。

use super::*;

pub(crate) fn split_host_port(s: &str, default_port: u16) -> (String, u16) {
    if let Some(rest) = s.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            let port = tail
                .strip_prefix(':')
                .and_then(|p| p.parse().ok())
                .unwrap_or(default_port);
            return (host.to_string(), port);
        }
    }
    match s.rsplit_once(':') {
        Some((host, port)) => match port.parse::<u16>() {
            Ok(p) => (host.to_string(), p),
            // 冒号后面不是端口（IPv6 没括号），整体当 host
            Err(_) => (s.to_string(), default_port),
        },
        None => (s.to_string(), default_port),
    }
}

/// 词法归一化路径：消掉 `.`、折叠 `..`，不做符号链接解析。
///
/// 这里防的是"webview 传 `../../x`"这类**字符串级**越界；符号链接穿越不在
/// 威胁模型内（能改应用数据目录的符号链接的人已经拿到了同等权限）。
pub(crate) fn normalize_path(p: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Runs `fut` to completion from sync command code. Commands execute on
/// tauri's blocking pool (no ambient runtime) so plain `block_on` is fine
/// there; tests call the same methods from inside a tokio runtime, where
/// `block_in_place` keeps the worker alive.
pub(crate) fn wait<F: std::future::Future>(fut: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| tauri::async_runtime::block_on(fut))
    } else {
        tauri::async_runtime::block_on(fut)
    }
}

pub(crate) fn hex_str(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// 32-char hex → 16-byte msg id (no panics on odd input).
pub(crate) fn decode_msg_id(s: &str) -> Option<MsgId> {
    let bytes = hex::decode(s).ok()?;
    <[u8; 16]>::try_from(bytes).ok()
}

