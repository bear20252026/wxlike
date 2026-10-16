//! NAT traversal configuration (WP-08a): STUN is built in as a tiny client,
//! TURN is a configuration interface.
//!
//! * `WXLIKE_STUN_URL` — `stun:host:port` (default: Google's public STUN).
//!   The host uses [`stun_bind`] to discover its server-reflexive address and
//!   adds it as a candidate next to its host candidates.
//! * `WXLIKE_TURN_URL` — `turn:host:port` plus `WXLIKE_TURN_USER` /
//!   `WXLIKE_TURN_PASS`. str0m moves bytes sans-I/O, so a TURN
//!   **allocation** (the socket relay) is host work; this module parses and
//!   exposes the configuration, and the allocation itself is left to the
//!   deployment (running coturn is an ops decision, not a library one).
//!   Without TURN, same-segment connections work — that is the acceptance
//!   environment the task book defines for offline verification.

use std::net::{SocketAddr, UdpSocket};

pub const DEFAULT_STUN: &str = "stun:stun.l.google.com:19302";

/// Parsed `WXLIKE_STUN_URL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StunConfig {
    pub host: String,
    pub port: u16,
}

impl StunConfig {
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("WXLIKE_STUN_URL").unwrap_or_else(|_| DEFAULT_STUN.into());
        Self::parse(&raw)
    }

    /// `stun:host:port` (port optional, defaults 3478).
    pub fn parse(raw: &str) -> Option<Self> {
        let rest = raw.strip_prefix("stun:")?;
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().ok()?),
            None => (rest.to_string(), 3478),
        };
        if host.is_empty() {
            return None;
        }
        Some(Self { host, port })
    }
}

/// Parsed `WXLIKE_TURN_URL` + credentials. `None` when TURN is not configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

impl TurnConfig {
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("WXLIKE_TURN_URL").ok()?;
        let rest = raw.strip_prefix("turn:")?;
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), p.parse().ok()?),
            None => (rest.to_string(), 3478),
        };
        if host.is_empty() {
            return None;
        }
        Some(Self {
            host,
            port,
            user: std::env::var("WXLIKE_TURN_USER").ok()?,
            password: std::env::var("WXLIKE_TURN_PASS").ok()?,
        })
    }
}

// ---------------------------------------------------------------------------
// minimal STUN binding client (RFC 5389 §6.2.5, no auth — public STUN)
// ---------------------------------------------------------------------------

const MAGIC_COOKIE: [u8; 4] = [0x21, 0x12, 0xA4, 0x42];
const XOR_MAPPED_ADDRESS: u16 = 0x0020;

fn crc32(data: &[u8]) -> u32 {
    // RFC 5389 §15.4 (same polynomial as the STUN fingerprint)
    let mut crc: u32 = 0;
    for &byte in data {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Builds a STUN Binding Request with the RFC fingerprint attribute.
pub fn binding_request(transaction_id: [u8; 12]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(28);
    msg.extend_from_slice(&0x0001_u16.to_be_bytes()); // BINDING REQUEST
    msg.extend_from_slice(&0u16.to_be_bytes()); // length, patched below
    msg.extend_from_slice(&MAGIC_COOKIE);
    msg.extend_from_slice(&transaction_id);
    // FINGERPRINT attribute (type 0x8028) over everything before it
    let mut attr = vec![0x80, 0x28, 0x00, 0x04];
    attr.extend_from_slice(&crc32(&msg).to_be_bytes());
    msg.extend_from_slice(&attr);
    let len = (msg.len() - 20) as u16;
    msg[2..4].copy_from_slice(&len.to_be_bytes());
    msg
}

/// Parses a Binding Response for XOR-MAPPED-ADDRESS.
pub fn parse_xor_mapped_address(response: &[u8]) -> Option<SocketAddr> {
    if response.len() < 20 {
        return None;
    }
    if response[0..2] != 0x0101_u16.to_be_bytes() {
        return None; // not a binding success response
    }
    if response[4..8] != MAGIC_COOKIE {
        return None;
    }
    let mut offset = 20;
    while offset + 4 <= response.len() {
        let atype = u16::from_be_bytes([response[offset], response[offset + 1]]);
        let alen = u16::from_be_bytes([response[offset + 2], response[offset + 3]]) as usize;
        let body = response.get(offset + 4..offset + 4 + alen)?;
        if atype == XOR_MAPPED_ADDRESS && alen >= 8 {
            let family = body[1];
            let xport = u16::from_be_bytes([body[2], body[3]]) ^ 0x2112;
            let cookie = u32::from_be_bytes(MAGIC_COOKIE);
            match family {
                0x01 => {
                    let xaddr = u32::from_be_bytes([body[4], body[5], body[6], body[7]]) ^ cookie;
                    return Some(SocketAddr::from((
                        std::net::IpAddr::V4(xaddr.into()),
                        xport,
                    )));
                }
                0x02 if alen >= 20 => {
                    let mut octets = [0u8; 16];
                    octets[..4].copy_from_slice(&MAGIC_COOKIE);
                    for (i, b) in body[4..20].iter().enumerate() {
                        octets[i + 4] = *b;
                    }
                    return Some(SocketAddr::from((
                        std::net::IpAddr::V6(octets.into()),
                        xport,
                    )));
                }
                _ => {}
            }
        }
        offset += 4 + alen + (alen % 4); // attributes are padded to 4 bytes
    }
    None
}

/// Performs one binding against `server` from a *new* local socket, returning
/// `(local_addr, reflexive_addr)`. The socket is closed after; callers that
/// need the reflexive candidate keep the mapping alive via keepalives at the
/// media layer (ICE consent covers this once the session runs).
pub fn stun_bind(
    server: SocketAddr,
    timeout: std::time::Duration,
) -> std::io::Result<(SocketAddr, SocketAddr)> {
    let local = if server.is_ipv4() {
        UdpSocket::bind("0.0.0.0:0")?
    } else {
        UdpSocket::bind("[::]:0")?
    };
    local.connect(server)?;
    let mut tid = [0u8; 12];
    {
        use rand::RngCore as _;
        rand::thread_rng().fill_bytes(&mut tid);
    }
    local.send(&binding_request(tid))?;
    let mut buf = [0u8; 576];
    local.set_read_timeout(Some(timeout))?;
    loop {
        let n = local.recv(&mut buf)?;
        // ignore responses for other transactions: match the id
        if n >= 20 && buf[8..20] == tid[..] {
            let reflexive = parse_xor_mapped_address(&buf[..n]).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "no xor-mapped-address")
            })?;
            return Ok((local.local_addr()?, reflexive));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stun_url_parsing() {
        assert_eq!(
            StunConfig::parse("stun:stun.l.google.com:19302"),
            Some(StunConfig {
                host: "stun.l.google.com".into(),
                port: 19302
            })
        );
        assert_eq!(
            StunConfig::parse("stun:stun.example.org"),
            Some(StunConfig {
                host: "stun.example.org".into(),
                port: 3478
            })
        );
        assert_eq!(StunConfig::parse("turn:x"), None, "wrong scheme");
        assert_eq!(StunConfig::parse("stun:"), None, "empty host");
        assert_eq!(StunConfig::parse("stun:h:notaport"), None);
    }

    #[test]
    fn binding_request_well_formed() {
        let msg = binding_request([7; 12]);
        assert_eq!(u16::from_be_bytes([msg[0], msg[1]]), 0x0001);
        assert_eq!(&msg[4..8], &MAGIC_COOKIE);
        assert_eq!(&msg[8..20], &[7; 12]);
        let declared = u16::from_be_bytes([msg[2], msg[3]]) as usize;
        assert_eq!(declared, msg.len() - 20, "length covers the attributes");
        // last attribute is the fingerprint
        assert_eq!(
            &msg[msg.len() - 8..msg.len() - 4],
            &[0x80, 0x28, 0x00, 0x04]
        );
    }

    #[test]
    fn xor_mapped_address_decodes_ipv4() {
        // hand-built success response: header + XOR-MAPPED-ADDRESS
        let mut resp = Vec::new();
        resp.extend_from_slice(&0x0101_u16.to_be_bytes());
        resp.extend_from_slice(&12u16.to_be_bytes()); // attr bytes
        resp.extend_from_slice(&MAGIC_COOKIE);
        resp.extend_from_slice(&[1; 12]);
        resp.extend_from_slice(&XOR_MAPPED_ADDRESS.to_be_bytes());
        resp.extend_from_slice(&8u16.to_be_bytes());
        resp.extend_from_slice(&[0]); // reserved
        resp.push(0x01); // IPv4
        let port: u16 = 4444;
        let xport = port ^ 0x2112;
        resp.extend_from_slice(&xport.to_be_bytes());
        let ip = std::net::Ipv4Addr::new(203, 0, 113, 7);
        let xaddr = u32::from(ip) ^ u32::from_be_bytes(MAGIC_COOKIE);
        resp.extend_from_slice(&xaddr.to_be_bytes());

        let got = parse_xor_mapped_address(&resp).expect("decodes");
        assert_eq!(got, SocketAddr::from((ip, port)));
    }

    #[test]
    fn garbage_responses_are_rejected() {
        assert!(parse_xor_mapped_address(&[]).is_none());
        assert!(parse_xor_mapped_address(&[0u8; 30]).is_none());
        // error response class
        let mut resp = vec![0x01, 0x11, 0, 0, 0x21, 0x12, 0xA4, 0x42];
        resp.extend_from_slice(&[0u8; 12]);
        assert!(parse_xor_mapped_address(&resp).is_none());
    }
}
