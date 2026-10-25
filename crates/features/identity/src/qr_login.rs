//! G02-07 桌面扫码登录。
//!
//! # 状态机（每一步都能单独失败，且失败是封闭的）
//!
//! ```text
//! pending --scan--> scanned --approve--> approved --exchange--> consumed
//!    |                 |                    |
//!    +--reject/expire--+--------------------+--> rejected / expired
//! ```
//!
//! # 为什么抄不走会话
//!
//! * 二维码里只有**公开 challenge**，没有 session token；
//! * 待登录设备先交 `pkce_challenge = sha256(verifier)`，`verifier` 只在桌面内存里；
//!   轮询与兑换都要出示 `verifier`——**拍到二维码的第三方拿不到它**；
//! * `scanned` 只是"手机看了一眼"，必须用户**明确批准**才到 `approved`；
//! * 兑换用条件 UPDATE 抢占：两台桌面同时兑换只有一台成功；
//! * 刷新二维码会作废旧 challenge，旧码重放直接失败。
//!
//! # 与设备信任的边界
//!
//! 扫码登录**只**建立一条可撤销的设备会话，不改变任何 E2EE 密钥的信任状态
//! （那是 G02-03 的 `verify`）。"这台电脑登录成功了"不等于"它历史密钥可信"。

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::{digest, random_hex, Audience, IdError, Result};

/// 二维码有效期：2 分钟。
pub const QR_TTL_MS: i64 = 2 * 60 * 1000;
/// 轮询建议间隔：2 秒。
pub const POLL_INTERVAL_MS: i64 = 2 * 1000;
/// challenge 随机字节数。
pub const CHALLENGE_BYTES: usize = 24;
/// challenge 行 id 的随机字节数。
pub const CHALLENGE_ID_BYTES: usize = 12;
/// PKCE verifier 长度区间（RFC 7636 §4.1）。
pub const VERIFIER_MIN: usize = 43;
pub const VERIFIER_MAX: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QrState {
    Pending,
    Scanned,
    Approved,
    Rejected,
    Consumed,
    Expired,
}

impl QrState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Scanned => "scanned",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Consumed => "consumed",
            Self::Expired => "expired",
        }
    }

    fn parse(raw: &str) -> Self {
        match raw {
            "scanned" => Self::Scanned,
            "approved" => Self::Approved,
            "rejected" => Self::Rejected,
            "consumed" => Self::Consumed,
            "expired" => Self::Expired,
            _ => Self::Pending,
        }
    }
}

/// 桌面拿到的二维码内容。`secret` 进二维码，`expires_at` 决定什么时候换码。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub id: String,
    pub secret: String,
    pub expires_at: i64,
    pub poll_interval_ms: i64,
}

/// 手机扫码后看到的线索：让用户确认"这确实是我面前这台电脑"。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanView {
    pub challenge_id: String,
    pub platform: String,
    pub label: String,
    pub ip_hint: String,
}

/// 桌面轮询结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollState {
    Pending,
    Scanned,
    Approved,
    Rejected,
    Expired,
}

impl PollState {
    fn of(state: QrState) -> Self {
        match state {
            QrState::Pending => Self::Pending,
            QrState::Scanned => Self::Scanned,
            QrState::Approved => Self::Approved,
            QrState::Rejected => Self::Rejected,
            QrState::Consumed | QrState::Expired => Self::Expired,
        }
    }
}

#[derive(Clone, Debug)]
struct QrRow {
    id: String,
    state: QrState,
    pkce_hash: String,
    device_id: String,
    platform: String,
    label: String,
    ip_hint: String,
    expires_at: i64,
    scanned_by: String,
}

const QR_COLS: &str = "id, state, pkce_hash, device_id, platform, label, ip_hint, expires_at, \
     scanned_by";

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<QrRow> {
    Ok(QrRow {
        id: r.get(0)?,
        state: QrState::parse(&r.get::<_, String>(1)?),
        pkce_hash: r.get(2)?,
        device_id: r.get(3)?,
        platform: r.get(4)?,
        label: r.get(5)?,
        ip_hint: r.get(6)?,
        expires_at: r.get(7)?,
        scanned_by: r.get(8)?,
    })
}

fn load(conn: &Connection, id: &str, now: i64) -> Result<QrRow> {
    let sql = format!("SELECT {QR_COLS} FROM identity_qr WHERE id = ?1");
    let row = conn
        .query_row(&sql, params![id], row_of)
        .optional()?
        .ok_or(IdError::NotFound("challenge"))?;
    if row.expires_at <= now
        && matches!(
            row.state,
            QrState::Pending | QrState::Scanned | QrState::Approved
        )
    {
        conn.execute(
            "UPDATE identity_qr SET state = 'expired' WHERE id = ?1 AND state IN \
             ('pending','scanned','approved')",
            params![id],
        )?;
        return Err(IdError::Expired);
    }
    Ok(row)
}

fn check_verifier(verifier: &str) -> Result<()> {
    let n = verifier.len();
    let ok = (VERIFIER_MIN..=VERIFIER_MAX).contains(&n)
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
    if ok {
        Ok(())
    } else {
        Err(IdError::BadInput("pkce verifier"))
    }
}

/// 桌面第一步：生成二维码内容。此时还没有绑定设备。
pub fn create(conn: &Connection, now: i64) -> Result<Ticket> {
    let id = random_hex(CHALLENGE_ID_BYTES);
    let secret = random_hex(CHALLENGE_BYTES);
    let expires_at = now.saturating_add(QR_TTL_MS);
    conn.execute(
        "INSERT INTO identity_qr(id, secret_hash, state, created_at, expires_at) \
         VALUES(?1, ?2, 'pending', ?3, ?4)",
        params![id, digest(&secret), now, expires_at],
    )?;
    Ok(Ticket {
        id,
        secret,
        expires_at,
        poll_interval_ms: POLL_INTERVAL_MS,
    })
}

/// 桌面第二步：把设备与 PKCE 摘要绑到 challenge 上（必须在被扫之前）。
#[allow(clippy::too_many_arguments)]
pub fn bind(
    conn: &Connection,
    id: &str,
    verifier: &str,
    device_id: &str,
    platform: &str,
    label: &str,
    ip_hint: &str,
    now: i64,
) -> Result<()> {
    check_verifier(verifier)?;
    let row = load(conn, id, now)?;
    if row.state != QrState::Pending {
        return Err(IdError::Conflict("challenge already used"));
    }
    if !crate::device::PLATFORMS.contains(&platform) {
        return Err(IdError::BadInput("platform"));
    }
    let label = crate::check_label(label, "label")?;
    let n = conn.execute(
        "UPDATE identity_qr SET pkce_hash = ?2, device_id = ?3, platform = ?4, label = ?5, \
         ip_hint = ?6 WHERE id = ?1 AND state = 'pending' AND pkce_hash = ''",
        params![id, digest(verifier), device_id, platform, label, ip_hint],
    )?;
    if n != 1 {
        return Err(IdError::Conflict("challenge already bound"));
    }
    Ok(())
}

/// 手机扫码：只前进到 `scanned`，并把桌面线索回给手机。
pub fn scan(conn: &Connection, secret: &str, scanner_user: &str, now: i64) -> Result<ScanView> {
    let sql = format!("SELECT {QR_COLS} FROM identity_qr WHERE secret_hash = ?1");
    let row = conn
        .query_row(&sql, params![digest(secret)], row_of)
        .optional()?
        .ok_or(IdError::NotFound("challenge"))?;
    let row = load(conn, &row.id, now)?;
    if row.pkce_hash.is_empty() {
        return Err(IdError::Conflict("challenge not bound to a device"));
    }
    if matches!(row.state, QrState::Expired) {
        return Err(IdError::Expired);
    }
    if row.state != QrState::Pending {
        return Err(IdError::Conflict("challenge not scannable"));
    }
    let n = conn.execute(
        "UPDATE identity_qr SET state = 'scanned', scanned_by = ?2, scanned_at = ?3 \
         WHERE id = ?1 AND state = 'pending'",
        params![row.id, scanner_user, now],
    )?;
    if n != 1 {
        return Err(IdError::Conflict("challenge not scannable"));
    }
    Ok(ScanView {
        challenge_id: row.id,
        platform: row.platform,
        label: row.label,
        ip_hint: row.ip_hint,
    })
}

/// 手机明确批准。只有扫过的那台手机能批准，且必须处在 `scanned`。
pub fn approve(conn: &Connection, id: &str, scanner_user: &str, now: i64) -> Result<()> {
    decide(conn, id, scanner_user, QrState::Approved, now)
}

/// 手机拒绝。
pub fn reject(conn: &Connection, id: &str, scanner_user: &str, now: i64) -> Result<()> {
    decide(conn, id, scanner_user, QrState::Rejected, now)
}

fn decide(
    conn: &Connection,
    id: &str,
    scanner_user: &str,
    target: QrState,
    now: i64,
) -> Result<()> {
    let row = load(conn, id, now)?;
    if row.state != QrState::Scanned {
        return Err(IdError::Conflict("challenge not scanned"));
    }
    if row.scanned_by != scanner_user {
        return Err(IdError::Forbidden("not the scanning account"));
    }
    let n = conn.execute(
        "UPDATE identity_qr SET state = ?3, decided_at = ?4 \
         WHERE id = ?1 AND state = 'scanned' AND scanned_by = ?2",
        params![id, scanner_user, target.as_str(), now],
    )?;
    if n != 1 {
        return Err(IdError::Conflict("challenge already decided"));
    }
    Ok(())
}

/// 桌面轮询：必须出示 verifier，否则拍到二维码的人就能偷看状态。
pub fn poll(conn: &Connection, id: &str, verifier: &str, now: i64) -> Result<PollState> {
    check_verifier(verifier)?;
    let row = match load(conn, id, now) {
        Ok(row) => row,
        Err(IdError::Expired) => return Ok(PollState::Expired),
        Err(other) => return Err(other),
    };
    if !crate::ct_eq(row.pkce_hash.as_bytes(), digest(verifier).as_bytes()) {
        return Err(IdError::Unauthorized("wrong verifier"));
    }
    Ok(PollState::of(row.state))
}

/// 兑换：`approved` → `consumed` 的抢占是原子的，两台桌面只有一台能成。
pub fn exchange(
    conn: &Connection,
    id: &str,
    verifier: &str,
    audience: Audience,
    ip: &str,
    now: i64,
) -> Result<crate::session::SessionGrant> {
    check_verifier(verifier)?;
    let row = load(conn, id, now)?;
    if row.state != QrState::Approved {
        return Err(IdError::Conflict("challenge not approved"));
    }
    if !crate::ct_eq(row.pkce_hash.as_bytes(), digest(verifier).as_bytes()) {
        return Err(IdError::Unauthorized("wrong verifier"));
    }
    if row.scanned_by.is_empty() {
        return Err(IdError::Conflict("challenge not approved"));
    }
    let n = conn.execute(
        "UPDATE identity_qr SET state = 'consumed', consumed_at = ?2 \
         WHERE id = ?1 AND state = 'approved'",
        params![id, now],
    )?;
    if n != 1 {
        return Err(IdError::Conflict("challenge already consumed"));
    }
    crate::session::open_default(conn, &row.scanned_by, &row.device_id, audience, ip, now)
}

/// 换码：旧 challenge 立刻作废，新码沿用同一设备与 PKCE 绑定。
pub fn refresh(conn: &Connection, old_id: &str, verifier: &str, now: i64) -> Result<Ticket> {
    check_verifier(verifier)?;
    let row = load(conn, old_id, now)?;
    if row.pkce_hash.is_empty()
        || !crate::ct_eq(row.pkce_hash.as_bytes(), digest(verifier).as_bytes())
    {
        return Err(IdError::Unauthorized("wrong verifier"));
    }
    if matches!(
        row.state,
        QrState::Consumed | QrState::Rejected | QrState::Expired
    ) {
        return Err(IdError::Conflict("challenge finished"));
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE identity_qr SET state = 'expired' WHERE id = ?1",
        params![old_id],
    )?;
    let id = random_hex(CHALLENGE_ID_BYTES);
    let secret = random_hex(CHALLENGE_BYTES);
    let expires_at = now.saturating_add(QR_TTL_MS);
    tx.execute(
        "INSERT INTO identity_qr(id, secret_hash, state, pkce_hash, device_id, platform, label, \
         ip_hint, created_at, expires_at) \
         VALUES(?1, ?2, 'pending', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            digest(&secret),
            row.pkce_hash,
            row.device_id,
            row.platform,
            row.label,
            row.ip_hint,
            now,
            expires_at
        ],
    )?;
    tx.commit()?;
    Ok(Ticket {
        id,
        secret,
        expires_at,
        poll_interval_ms: POLL_INTERVAL_MS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;

    const VERIFIER: &str = "0123456789abcdef0123456789abcdef0123456789ab";

    fn desktop(conn: &Connection, now: i64) -> (Ticket, String) {
        let reg = crate::device::Registration {
            platform: "desktop".into(),
            label: "MacBook Pro".into(),
            ..Default::default()
        };
        crate::device::register(conn, "u1", "dev-desk0001", &reg, now).unwrap();
        let ticket = create(conn, now).unwrap();
        bind(
            conn,
            &ticket.id,
            VERIFIER,
            "dev-desk0001",
            "desktop",
            "MacBook Pro",
            "192.0.2.10",
            now,
        )
        .unwrap();
        (ticket, VERIFIER.to_string())
    }

    #[test]
    fn happy_path_grants_a_session() {
        let conn = db();
        let (ticket, verifier) = desktop(&conn, 1_000);
        assert_eq!(
            poll(&conn, &ticket.id, &verifier, 1_100).unwrap(),
            PollState::Pending
        );
        let view = scan(&conn, &ticket.secret, "u1", 1_200).unwrap();
        assert_eq!(view.label, "MacBook Pro");
        assert_eq!(
            poll(&conn, &ticket.id, &verifier, 1_300).unwrap(),
            PollState::Scanned
        );
        approve(&conn, &ticket.id, "u1", 1_400).unwrap();
        let grant = exchange(
            &conn,
            &ticket.id,
            &verifier,
            Audience::User,
            "192.0.2.10",
            1_500,
        )
        .unwrap();
        let ctx = crate::session::authenticate(&conn, &grant.access_token, Audience::User, 1_600)
            .unwrap();
        assert_eq!(ctx.user_id, "u1");
        assert_eq!(ctx.device_id, "dev-desk0001");
    }

    #[test]
    fn qr_copier_cannot_steal_the_session() {
        let conn = db();
        let (ticket, _verifier) = desktop(&conn, 1_000);
        // 第三方拿到二维码内容（secret），能扫，但没有 verifier。
        scan(&conn, &ticket.secret, "u1", 1_100).unwrap();
        approve(&conn, &ticket.id, "u1", 1_200).unwrap();
        assert!(matches!(
            poll(
                &conn,
                &ticket.id,
                "attacker-verifier-012345678901234567890123456789",
                1_300
            ),
            Err(IdError::Unauthorized(_))
        ));
        assert!(matches!(
            exchange(
                &conn,
                &ticket.id,
                "attacker-verifier-012345678901234567890123456789",
                Audience::User,
                "",
                1_300
            ),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn scan_alone_is_not_approval() {
        let conn = db();
        let (ticket, verifier) = desktop(&conn, 1_000);
        scan(&conn, &ticket.secret, "u1", 1_100).unwrap();
        assert!(
            matches!(
                exchange(&conn, &ticket.id, &verifier, Audience::User, "", 1_200),
                Err(IdError::Conflict(_))
            ),
            "已扫码 ≠ 已授权"
        );
    }

    #[test]
    fn reject_and_expiry_are_terminal() {
        let conn = db();
        let (ticket, verifier) = desktop(&conn, 1_000);
        scan(&conn, &ticket.secret, "u1", 1_100).unwrap();
        reject(&conn, &ticket.id, "u1", 1_200).unwrap();
        assert_eq!(
            poll(&conn, &ticket.id, &verifier, 1_300).unwrap(),
            PollState::Rejected
        );
        assert!(exchange(&conn, &ticket.id, &verifier, Audience::User, "", 1_400).is_err());

        let (stale, v2) = desktop(&conn, 2_000);
        assert_eq!(
            poll(&conn, &stale.id, &v2, 2_000 + QR_TTL_MS + 1).unwrap(),
            PollState::Expired
        );
    }

    #[test]
    fn two_desktops_race_only_one_wins() {
        let conn = db();
        let (ticket, verifier) = desktop(&conn, 1_000);
        scan(&conn, &ticket.secret, "u1", 1_100).unwrap();
        approve(&conn, &ticket.id, "u1", 1_200).unwrap();
        exchange(&conn, &ticket.id, &verifier, Audience::User, "", 1_300).unwrap();
        assert!(matches!(
            exchange(&conn, &ticket.id, &verifier, Audience::User, "", 1_400),
            Err(IdError::Conflict(_))
        ));
    }

    #[test]
    fn refresh_kills_the_old_code() {
        let conn = db();
        let (ticket, verifier) = desktop(&conn, 1_000);
        let fresh = refresh(&conn, &ticket.id, &verifier, 1_100).unwrap();
        assert_ne!(fresh.id, ticket.id);
        assert!(matches!(
            scan(&conn, &ticket.secret, "u1", 1_200),
            Err(IdError::Expired) | Err(IdError::NotFound(_))
        ));
        scan(&conn, &fresh.secret, "u1", 1_300).unwrap();
    }

    #[test]
    fn unbound_and_wrong_scanner_are_rejected() {
        let conn = db();
        let ticket = create(&conn, 1_000).unwrap();
        assert!(
            matches!(
                scan(&conn, &ticket.secret, "u1", 1_100),
                Err(IdError::Conflict(_))
            ),
            "没绑设备的码不能扫"
        );
        let (bound, _v) = desktop(&conn, 2_000);
        scan(&conn, &bound.secret, "u1", 2_100).unwrap();
        assert!(matches!(
            approve(&conn, &bound.id, "u2", 2_200),
            Err(IdError::Forbidden(_))
        ));
    }

    #[test]
    fn bad_verifier_shape() {
        assert!(matches!(
            check_verifier("short"),
            Err(IdError::BadInput("pkce verifier"))
        ));
        assert!(check_verifier(VERIFIER).is_ok());
    }
}
