//! 网络地址探测：本机默认出口 IPv4。
//!
//! `connect` 一个 TEST-NET-1 地址（192.0.2.0/24，文档专用、不可路由）不会真的
//! 发包，只是让内核做一次路由选择；`local_addr` 因此是本机主网卡地址。
//! SFU 与桌面端都需要它，实现只留这一份。

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

/// 本机主网卡 IPv4；没有 IPv4 默认路由时返回 `None`。
#[must_use]
pub fn primary_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(_) => None,
    }
}
