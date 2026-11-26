//! G02-01 刷新令牌链：轮换 + 重放检测。
//!
//! 每次刷新**作废旧令牌、签发新令牌**，形成一条 `generation` 递增的链。旧令牌
//! 被再次使用只有两种解释：客户端重试竞态，或令牌已泄露。两者都按重放处理——
//! 整条链立刻作废（含当前有效令牌），用户必须重新登录。宁可多一次登录，也不
//! 让攻击者与被盗设备共享一条长期有效的链。
//!
//! 库里只存 `sha256(token)`：备份/快照泄露不产生可用凭据。

use hmac::Mac as _;
use rusqlite::Connection;

use crate::session::Features;
use crate::sqlport::{ColKind, SqlExec, SqlParam, SqlValue, SqliteExec};
use crate::{digest, random_hex, Audience, IdError, Result, REFRESH_TTL_MS, TOKEN_BYTES};

/// 链上一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainRow {
    pub token_hash: String,
    pub sid: String,
    pub user_id: String,
    pub device_id: String,
    pub audience: Audience,
    pub generation: i64,
    pub created_at: i64,
    pub expires_at: i64,
    pub used_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

/// 轮换结果：新令牌 + 该链的归属信息（调用方据此重建访问令牌）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rotated {
    pub sid: String,
    pub user_id: String,
    pub device_id: String,
    pub audience: Audience,
    pub generation: i64,
    pub refresh_token: String,
    pub expires_at: i64,
}

/// 链上一行的**原文**读法（文本列先按字符串取，状态判定留到 [`decode`]）。
struct RawChain {
    token_hash: String,
    sid: String,
    user_id: String,
    device_id: String,
    audience: String,
    generation: i64,
    created_at: i64,
    expires_at: i64,
    used_at: Option<i64>,
    revoked_at: Option<i64>,
}

const RAW_KINDS: [ColKind; 10] = [
    ColKind::Text,
    ColKind::Text,
    ColKind::Text,
    ColKind::Text,
    ColKind::Text,
    ColKind::Int,
    ColKind::Int,
    ColKind::Int,
    ColKind::OptInt,
    ColKind::OptInt,
];

fn chain_text<I: Iterator<Item = SqlValue>>(it: &mut I) -> Result<String> {
    it.next()
        .ok_or(IdError::Corrupt("identity_refresh.row width"))?
        .into_text()
}

fn chain_int<I: Iterator<Item = SqlValue>>(it: &mut I) -> Result<i64> {
    it.next()
        .ok_or(IdError::Corrupt("identity_refresh.row width"))?
        .into_int()
}

fn chain_opt_int<I: Iterator<Item = SqlValue>>(it: &mut I) -> Result<Option<i64>> {
    it.next()
        .ok_or(IdError::Corrupt("identity_refresh.row width"))?
        .into_opt_int()
}

fn raw_values(values: Vec<SqlValue>) -> Result<RawChain> {
    let mut it = values.into_iter();
    let token_hash = chain_text(&mut it)?;
    let sid = chain_text(&mut it)?;
    let user_id = chain_text(&mut it)?;
    let device_id = chain_text(&mut it)?;
    let audience = chain_text(&mut it)?;
    let generation = chain_int(&mut it)?;
    let created_at = chain_int(&mut it)?;
    let expires_at = chain_int(&mut it)?;
    let used_at = chain_opt_int(&mut it)?;
    let revoked_at = chain_opt_int(&mut it)?;
    Ok(RawChain {
        token_hash,
        sid,
        user_id,
        device_id,
        audience,
        generation,
        created_at,
        expires_at,
        used_at,
        revoked_at,
    })
}

/// 原文 → 链上行。`audience` 读不懂**必须报错**：`rotate()` 用这里的 audience
/// 决定轮换后签发哪一面令牌，折成 `User` 会让一条本来属于管理面的刷新链在库里
/// 悄悄降级成用户面——用户面与管理面令牌分开（#103 要求 4）这件事就此失效，
/// 而且失效是静默的。与 `session::authenticate` 同口径，宁可失败。
fn decode(raw: RawChain) -> Result<ChainRow> {
    Ok(ChainRow {
        audience: Audience::parse(&raw.audience).ok_or(IdError::Corrupt("audience"))?,
        token_hash: raw.token_hash,
        sid: raw.sid,
        user_id: raw.user_id,
        device_id: raw.device_id,
        generation: raw.generation,
        created_at: raw.created_at,
        expires_at: raw.expires_at,
        used_at: raw.used_at,
        revoked_at: raw.revoked_at,
    })
}

const SELECT_ROW: &str = "SELECT token_hash, sid, user_id, device_id, audience, generation, \
     created_at, expires_at, used_at, revoked_at FROM identity_refresh";

/// 为一次登录签发首枚刷新令牌（generation = 1）。
pub fn issue(
    conn: &Connection,
    user_id: &str,
    sid: &str,
    device_id: &str,
    audience: Audience,
    ttl_ms: i64,
    now: i64,
) -> Result<String> {
    issue_with(
        &SqliteExec(conn),
        user_id,
        sid,
        device_id,
        audience,
        ttl_ms,
        now,
    )
}

/// 端口版 `issue`。
pub fn issue_with<E: SqlExec + ?Sized>(
    exec: &E,
    user_id: &str,
    sid: &str,
    device_id: &str,
    audience: Audience,
    ttl_ms: i64,
    now: i64,
) -> Result<String> {
    // 上游 `GenSecret` 的 lifetime 规则（与 `session::open_with` 同一条）：
    // 0 = 没表态 → 用缺省 30 天；负数 = 已经死了 → 拒签。
    // 少了这条，`issue(.., -1, ..)` 会写出一行 `expires_at < created_at` 的链上记录，
    // 表面看是「用户拿到的令牌一用就过期」，实际是服务端签了一枚废令牌，
    // 排查方向完全错开。
    let ttl_ms = match ttl_ms {
        0 => REFRESH_TTL_MS,
        n if n < 0 => return Err(IdError::Expired),
        n => n,
    };
    let raw = random_hex(TOKEN_BYTES);
    exec.exec(
        "INSERT INTO identity_refresh(token_hash, sid, user_id, device_id, audience, \
         generation, created_at, expires_at, used_at, revoked_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, NULL, NULL)",
        &[
            SqlParam::Text(digest(&raw)),
            SqlParam::Text(sid.to_string()),
            SqlParam::Text(user_id.to_string()),
            SqlParam::Text(device_id.to_string()),
            SqlParam::Text(audience.as_str().to_string()),
            SqlParam::Int(now),
            SqlParam::Int(now.saturating_add(ttl_ms)),
        ],
    )?;
    Ok(raw)
}

/// 默认有效期签发的便捷入口。
pub fn issue_default(
    conn: &Connection,
    user_id: &str,
    sid: &str,
    device_id: &str,
    audience: Audience,
    now: i64,
) -> Result<String> {
    issue(conn, user_id, sid, device_id, audience, REFRESH_TTL_MS, now)
}

/// 端口版 `issue_default`。
pub fn issue_default_with<E: SqlExec + ?Sized>(
    exec: &E,
    user_id: &str,
    sid: &str,
    device_id: &str,
    audience: Audience,
    now: i64,
) -> Result<String> {
    issue_with(exec, user_id, sid, device_id, audience, REFRESH_TTL_MS, now)
}

/// 单行查询（含已用/已撤销）。
pub fn find(conn: &Connection, raw: &str) -> Result<Option<ChainRow>> {
    find_with(&SqliteExec(conn), raw)
}

/// 端口版 `find`。
pub fn find_with<E: SqlExec + ?Sized>(exec: &E, raw: &str) -> Result<Option<ChainRow>> {
    let sql = format!("{SELECT_ROW} WHERE token_hash = ?1");
    exec.query(&sql, &[SqlParam::Text(digest(raw))], &RAW_KINDS)?
        .map(raw_values)
        .transpose()?
        .map(decode)
        .transpose()
}

/// 轮换。失败即封闭：任何异常状态都会顺手作废整条链。
pub fn rotate(conn: &Connection, raw: &str, now: i64) -> Result<Rotated> {
    let tx = conn.unchecked_transaction()?;
    let outcome = rotate_outcome_with(&SqliteExec(&tx), raw, now)?;
    tx.commit()?;
    outcome.into_result()
}

/// 轮换结果的二值形态：被拒绝也已“在事务内完成撤销”，提交后再转错误。
#[derive(Clone, Debug)]
pub enum RotateOutcome {
    Rotated(Rotated),
    Rejected(RotateReject),
}

/// 拒绝原因；带 `sid` 表示链已被就地撤销。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RotateReject {
    Unknown,
    Revoked,
    Replay { sid: String },
    Expired { sid: String },
    SessionGone { sid: String },
    TempAuth { sid: String },
}

impl RotateOutcome {
    pub fn into_result(self) -> Result<Rotated> {
        match self {
            Self::Rotated(rotated) => Ok(rotated),
            Self::Rejected(reason) => Err(reason.into()),
        }
    }
}

impl From<RotateReject> for IdError {
    fn from(reason: RotateReject) -> Self {
        match reason {
            RotateReject::Unknown => IdError::Unauthorized("unknown refresh token"),
            RotateReject::Revoked => IdError::Unauthorized("refresh token revoked"),
            RotateReject::Replay { .. } => IdError::Unauthorized("refresh token replay detected"),
            RotateReject::Expired { .. } => IdError::Expired,
            RotateReject::SessionGone { .. } => IdError::Unauthorized("session revoked"),
            RotateReject::TempAuth { .. } => {
                IdError::Unauthorized("temp auth cannot mint a session")
            }
        }
    }
}

/// 端口版 `rotate`：返回**可提交的结果**。
///
/// 重放 / 过期 / 临时认证 / 会话已撤销都已在事务内完成撤销，调用方应提交事务，
/// 再把 [`RotateOutcome::into_result`] 的错误交给上层；只有真正的存储错误才回滚。
pub fn rotate_outcome_with<E: SqlExec + ?Sized>(
    exec: &E,
    raw: &str,
    now: i64,
) -> Result<RotateOutcome> {
    let Some(row) = find_with(exec, raw)? else {
        return Ok(RotateOutcome::Rejected(RotateReject::Unknown));
    };
    if row.revoked_at.is_some() {
        return Ok(RotateOutcome::Rejected(RotateReject::Revoked));
    }
    if row.used_at.is_some() {
        revoke_sid_with(exec, &row.sid, now)?;
        return Ok(RotateOutcome::Rejected(RotateReject::Replay {
            sid: row.sid,
        }));
    }
    if row.expires_at <= now {
        revoke_sid_with(exec, &row.sid, now)?;
        return Ok(RotateOutcome::Rejected(RotateReject::Expired {
            sid: row.sid,
        }));
    }
    if !access_alive_with(exec, &row.sid, now)? {
        revoke_sid_with(exec, &row.sid, now)?;
        return Ok(RotateOutcome::Rejected(RotateReject::SessionGone {
            sid: row.sid,
        }));
    }
    if is_temp_auth_with(exec, &row.sid)? {
        revoke_sid_with(exec, &row.sid, now)?;
        return Ok(RotateOutcome::Rejected(RotateReject::TempAuth {
            sid: row.sid,
        }));
    }
    match mint_successor_with(exec, &row, now)? {
        Some(rotated) => Ok(RotateOutcome::Rotated(rotated)),
        None => {
            // 并发轮换：另一个请求抢先用了这枚令牌，本次按重放处理。
            revoke_sid_with(exec, &row.sid, now)?;
            Ok(RotateOutcome::Rejected(RotateReject::Replay {
                sid: row.sid,
            }))
        }
    }
}

/// `FeatureNoLogin`（`L`）：临时认证**不得**换出永久会话。
///
/// 命中时由调用方顺手作废整条链：一个带 `L` 的会话出现在刷新链上，
/// 只可能是有人拿错了令牌在试。
fn is_temp_auth_with<E: SqlExec + ?Sized>(exec: &E, sid: &str) -> Result<bool> {
    let features = exec.query(
        "SELECT features FROM identity_session_token WHERE sid = ?1",
        &[SqlParam::Text(sid.to_string())],
        &[ColKind::Text],
    )?;
    let Some(values) = features else {
        return Ok(false);
    };
    let raw = values
        .into_iter()
        .next()
        .ok_or(IdError::Corrupt("identity_session_token.row width"))?
        .into_text()?;
    Ok(Features::parse(&raw)?.contains(Features::NO_LOGIN))
}

/// 访问会话已被删除（设备被移除 / 已登出）时，刷新令牌一并失效。
fn access_alive_with<E: SqlExec + ?Sized>(exec: &E, sid: &str, now: i64) -> Result<bool> {
    let alive = exec.query(
        "SELECT expires_at FROM sessions WHERE sid = ?1",
        &[SqlParam::Text(sid.to_string())],
        &[ColKind::Int],
    )?;
    let expires_at = alive
        .map(|values| {
            values
                .into_iter()
                .next()
                .ok_or(IdError::Corrupt("sessions.row width"))?
                .into_int()
        })
        .transpose()?;
    Ok(matches!(expires_at, Some(at) if at > now))
}

/// 标记旧令牌已用 + 落新令牌；条件更新输给并发时返回 `None`。
fn mint_successor_with<E: SqlExec + ?Sized>(
    exec: &E,
    row: &ChainRow,
    now: i64,
) -> Result<Option<Rotated>> {
    let raw = random_hex(TOKEN_BYTES);
    let n = exec.exec(
        "UPDATE identity_refresh SET used_at = ?2 WHERE token_hash = ?1 AND used_at IS NULL",
        &[SqlParam::Text(row.token_hash.clone()), SqlParam::Int(now)],
    )?;
    if n != 1 {
        return Ok(None);
    }
    exec.exec(
        "INSERT INTO identity_refresh(token_hash, sid, user_id, device_id, audience, \
         generation, created_at, expires_at, used_at, revoked_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL)",
        &[
            SqlParam::Text(digest(&raw)),
            SqlParam::Text(row.sid.clone()),
            SqlParam::Text(row.user_id.clone()),
            SqlParam::Text(row.device_id.clone()),
            SqlParam::Text(row.audience.as_str().to_string()),
            SqlParam::Int(row.generation + 1),
            SqlParam::Int(now),
            SqlParam::Int(row.expires_at),
        ],
    )?;
    Ok(Some(Rotated {
        sid: row.sid.clone(),
        user_id: row.user_id.clone(),
        device_id: row.device_id.clone(),
        audience: row.audience,
        generation: row.generation + 1,
        refresh_token: raw,
        expires_at: row.expires_at,
    }))
}

/// 票据载荷（上游 `tokenLayout` 的等价字段集）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    pub user_id: String,
    pub sid: String,
    pub generation: i64,
    /// 到期时刻，**unix 秒**（上游同样用秒：`Expires uint32`）。
    pub expires_at: i64,
    pub level: crate::session::AuthLevel,
    pub features: crate::session::Features,
    pub serial: i64,
}

/// 密钥最短长度：SHA-256 的分组长度，与上游同一条下限。
pub const MIN_TICKET_KEY_BYTES: usize = 32;
/// 上游 `expire_in <= 0` 视为配置错误；这里同样要求正数秒。
pub const MIN_TICKET_LIFETIME_SECS: i64 = 1;

/// 签名器：一把密钥 + 当前序列号 + 缺省寿命（上游 config 的三件套）。
#[derive(Clone)]
pub struct TicketSigner {
    key: Vec<u8>,
    serial: i64,
    lifetime_secs: i64,
}

impl TicketSigner {
    /// `Init`：把上游两条启动期检查原样搬过来。
    pub fn new(key: &[u8], serial: i64, lifetime_secs: i64) -> Result<Self> {
        if key.len() < MIN_TICKET_KEY_BYTES {
            return Err(IdError::BadInput("ticket key missing or too short"));
        }
        if lifetime_secs < MIN_TICKET_LIFETIME_SECS {
            return Err(IdError::BadInput("ticket expiration value"));
        }
        Ok(Self {
            key: key.to_vec(),
            serial,
            lifetime_secs,
        })
    }

    pub fn serial(&self) -> i64 {
        self.serial
    }

    /// `GenSecret`：签一枚票据，返回 `(令牌, 到期秒)`。
    ///
    /// `lifetime_secs` 走上游同一条三态：`0` = 用配置缺省，负数 = `Expired`
    /// （拒签一枚生下来就死的令牌），正数 = 自定义寿命。
    #[allow(clippy::too_many_arguments)]
    pub fn mint(
        &self,
        user_id: &str,
        sid: &str,
        generation: i64,
        level: crate::session::AuthLevel,
        features: crate::session::Features,
        lifetime_secs: i64,
        now_secs: i64,
    ) -> Result<(String, i64)> {
        let lifetime = match lifetime_secs {
            0 => self.lifetime_secs,
            n if n < 0 => return Err(IdError::Expired),
            n => n,
        };
        let expires_at = now_secs.saturating_add(lifetime);
        let body = encode_body(
            user_id,
            sid,
            generation,
            expires_at,
            level,
            features,
            self.serial,
        );
        let mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&self.key)
            .map_err(|_| IdError::BadInput("ticket key"))?;
        let mut mac = mac;
        mac.update(&body);
        let tag = mac.finalize().into_bytes();
        let mut out = hex::encode(&body);
        out.push('.');
        out.push_str(&hex::encode(tag));
        Ok((out, expires_at))
    }

    /// `Authenticate`：**按上游的分支顺序**校验，且错误类别一一对应。
    ///
    /// 定长/十六进制的解析失败一律 `BadInput`（= 上游 `ErrMalformed`）；
    /// 签名与序列号一律 `Unauthorized`（= 上游 `ErrFailed`，同形）；
    /// 过期是 `Expired`（= 上游 `ErrExpired`），判定点前移 `EXPIRY_SKEW_SECS`。
    pub fn verify(&self, token: &str, now_secs: i64) -> Result<Ticket> {
        let (body_hex, tag_hex) = token
            .split_once('.')
            .ok_or(IdError::BadInput("malformed ticket"))?;
        // 上游的"令牌太短"这一条：签名段必须正好是 SHA-256 的长度。
        if tag_hex.len() != TAG_BYTES * 2 {
            return Err(IdError::BadInput("malformed ticket"));
        }
        let body = hex::decode(body_hex).map_err(|_| IdError::BadInput("malformed ticket"))?;
        let tag = hex::decode(tag_hex).map_err(|_| IdError::BadInput("malformed ticket"))?;
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&self.key)
            .map_err(|_| IdError::BadInput("ticket key"))?;
        mac.update(&body);
        if mac.verify_slice(&tag).is_err() {
            return Err(IdError::Unauthorized("invalid ticket"));
        }
        let ticket = decode_body(&body)?;
        // 级别合法性（上游：`auth.Level(tl.AuthLevel) > auth.LevelRoot` → Malformed）。
        if !matches!(
            ticket.level,
            crate::session::AuthLevel::Auth
                | crate::session::AuthLevel::Root
                | crate::session::AuthLevel::Anon
        ) {
            return Err(IdError::BadInput("ticket auth level"));
        }
        if ticket.serial != self.serial {
            // 与签名不符**同一条消息**：见函数头。
            return Err(IdError::Unauthorized("invalid ticket"));
        }
        if ticket.expires_at <= now_secs.saturating_add(crate::session::EXPIRY_SKEW_SECS) {
            return Err(IdError::Expired);
        }
        Ok(ticket)
    }
}

/// 规范化编码：每个字段都带长度前缀，杜绝 `"a|b"` 与 `"a|b"` 拼接歧义。
fn encode_body(
    user_id: &str,
    sid: &str,
    generation: i64,
    expires_at: i64,
    level: crate::session::AuthLevel,
    features: crate::session::Features,
    serial: i64,
) -> Vec<u8> {
    let mut out = Vec::new();
    for field in [user_id, sid] {
        out.extend_from_slice(&(field.len() as u64).to_be_bytes());
        out.extend_from_slice(field.as_bytes());
    }
    for number in [generation, expires_at, serial] {
        out.extend_from_slice(&number.to_be_bytes());
    }
    out.extend_from_slice(&[level_code(level), features.bits() as u8]);
    out
}

fn decode_body(body: &[u8]) -> Result<Ticket> {
    // 长度前缀全部要先验证再切片：宁可报 Malformed，也不要 panic。
    let need = |offset: usize, take: usize| {
        offset
            .checked_add(take)
            .is_some_and(|end| end <= body.len())
    };
    let mut i = 0usize;
    let mut strings = Vec::with_capacity(2);
    for _ in 0..2 {
        if !need(i, 8) {
            return Err(IdError::BadInput("malformed ticket"));
        }
        let mut len = [0u8; 8];
        len.copy_from_slice(&body[i..i + 8]);
        let len = usize::try_from(u64::from_be_bytes(len))
            .map_err(|_| IdError::BadInput("malformed ticket"))?;
        i += 8;
        if !need(i, len) {
            return Err(IdError::BadInput("malformed ticket"));
        }
        strings.push(
            String::from_utf8(body[i..i + len].to_vec())
                .map_err(|_| IdError::BadInput("malformed ticket"))?,
        );
        i += len;
    }
    let mut numbers = Vec::with_capacity(3);
    for _ in 0..3 {
        if !need(i, 8) {
            return Err(IdError::BadInput("malformed ticket"));
        }
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&body[i..i + 8]);
        numbers.push(i64::from_be_bytes(buf));
        i += 8;
    }
    if !need(i, 2) {
        return Err(IdError::BadInput("malformed ticket"));
    }
    let level = level_from_code(body[i]);
    let features = crate::session::Features::from_bits(body[i + 1] as u16);
    Ok(Ticket {
        user_id: strings.remove(0),
        sid: strings.remove(0),
        generation: numbers[0],
        expires_at: numbers[1],
        serial: numbers[2],
        level,
        features,
    })
}

fn level_code(level: crate::session::AuthLevel) -> u8 {
    match level {
        crate::session::AuthLevel::None => 0,
        crate::session::AuthLevel::Anon => 10,
        crate::session::AuthLevel::Auth => 20,
        crate::session::AuthLevel::Root => 30,
    }
}

fn level_from_code(code: u8) -> crate::session::AuthLevel {
    match code {
        10 => crate::session::AuthLevel::Anon,
        20 => crate::session::AuthLevel::Auth,
        30 => crate::session::AuthLevel::Root,
        // 上游用 `LevelNone` 承载"没认证"，未知字节在这里同样落到它，
        // 由 `verify` 的级别白名单当场拒掉——不在解码阶段猜。
        _ => crate::session::AuthLevel::None,
    }
}

/// SHA-256 摘要长度：上游 `tokenLayout` 末尾那 32 字节签名段的等价物。
const TAG_BYTES: usize = 32;

/// 作废一个会话下的整条链。
pub fn revoke_sid(conn: &Connection, sid: &str, now: i64) -> Result<usize> {
    revoke_sid_with(&SqliteExec(conn), sid, now)
}

/// 端口版 `revoke_sid`。
pub fn revoke_sid_with<E: SqlExec + ?Sized>(exec: &E, sid: &str, now: i64) -> Result<usize> {
    exec.exec(
        "UPDATE identity_refresh SET revoked_at = ?2 WHERE sid = ?1 AND revoked_at IS NULL",
        &[SqlParam::Text(sid.to_string()), SqlParam::Int(now)],
    )
}

/// 作废一台设备的全部链（设备移除 / 远程下线）。
pub fn revoke_device(conn: &Connection, user_id: &str, device_id: &str, now: i64) -> Result<usize> {
    revoke_device_with(&SqliteExec(conn), user_id, device_id, now)
}

/// 端口版 `revoke_device`。
pub fn revoke_device_with<E: SqlExec + ?Sized>(
    exec: &E,
    user_id: &str,
    device_id: &str,
    now: i64,
) -> Result<usize> {
    exec.exec(
        "UPDATE identity_refresh SET revoked_at = ?3 \
         WHERE user_id = ?1 AND device_id = ?2 AND revoked_at IS NULL",
        &[
            SqlParam::Text(user_id.to_string()),
            SqlParam::Text(device_id.to_string()),
            SqlParam::Int(now),
        ],
    )
}

/// 作废一个账号的全部链（注销 / 改密后强制全端重登）。
pub fn revoke_user(conn: &Connection, user_id: &str, now: i64) -> Result<usize> {
    revoke_user_with(&SqliteExec(conn), user_id, now)
}

/// 端口版 `revoke_user`。
pub fn revoke_user_with<E: SqlExec + ?Sized>(exec: &E, user_id: &str, now: i64) -> Result<usize> {
    exec.exec(
        "UPDATE identity_refresh SET revoked_at = ?2 WHERE user_id = ?1 AND revoked_at IS NULL",
        &[SqlParam::Text(user_id.to_string()), SqlParam::Int(now)],
    )
}

/// 一个会话的完整链（运维视图 / 测试断言用）。
pub fn chain_of(conn: &Connection, sid: &str) -> Result<Vec<ChainRow>> {
    chain_of_with(&SqliteExec(conn), sid)
}

/// 端口版 `chain_of`。
pub fn chain_of_with<E: SqlExec + ?Sized>(exec: &E, sid: &str) -> Result<Vec<ChainRow>> {
    let sql = format!("{SELECT_ROW} WHERE sid = ?1 ORDER BY generation ASC");
    exec.query_many(&sql, &[SqlParam::Text(sid.to_string())], &RAW_KINDS)?
        .into_iter()
        .map(raw_values)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .map(decode)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;
    use rusqlite::params;

    fn session_row(conn: &Connection, sid: &str, now: i64) {
        conn.execute(
            "INSERT INTO sessions(token, user_id, created_at, expires_at, sid, device_id, \
             device_name, ip, last_seen, audience) \
             VALUES(?1, 'u1', ?2, ?3, ?4, 'dev-aaaa1111', 'Pixel', '127.0.0.1', ?2, 'user')",
            params![format!("tok-{sid}"), now, now + 1_000, sid],
        )
        .unwrap();
    }

    #[test]
    fn rotation_advances_generation() {
        let conn = db();
        session_row(&conn, "s1", 1_000);
        let first =
            issue_default(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, 1_000).unwrap();
        let second = rotate(&conn, &first, 1_100).unwrap();
        assert_eq!(second.generation, 2);
        assert_ne!(second.refresh_token, first);
        let third = rotate(&conn, &second.refresh_token, 1_200).unwrap();
        assert_eq!(third.generation, 3);
        assert_eq!(chain_of(&conn, "s1").unwrap().len(), 3);
    }

    #[test]
    fn replay_kills_the_whole_chain() {
        let conn = db();
        session_row(&conn, "s1", 1_000);
        let first =
            issue_default(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, 1_000).unwrap();
        let second = rotate(&conn, &first, 1_100).unwrap();
        let err = rotate(&conn, &first, 1_200).unwrap_err();
        assert!(matches!(err, IdError::Unauthorized(m) if m.contains("replay")));
        // 攻击者重放之后，合法客户端手里的新令牌也必须失效。
        assert!(matches!(
            rotate(&conn, &second.refresh_token, 1_300),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn expired_and_unknown_are_rejected() {
        let conn = db();
        session_row(&conn, "s1", 1_000);
        let raw = issue(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, 10, 1_000).unwrap();
        assert!(matches!(rotate(&conn, &raw, 5_000), Err(IdError::Expired)));
        assert!(matches!(
            rotate(&conn, "not-a-token", 1_000),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn signer_refuses_weak_key_and_bad_lifetime() {
        // 上游 `Init`：`len(key) < sha256.Size` 与 `ExpireIn <= 0` 都是**启动期**错误。
        assert!(matches!(
            TicketSigner::new(b"short", 1, 60),
            Err(IdError::BadInput("ticket key missing or too short"))
        ));
        let key = [7u8; MIN_TICKET_KEY_BYTES];
        assert!(matches!(
            TicketSigner::new(&key, 1, 0),
            Err(IdError::BadInput("ticket expiration value"))
        ));
        assert!(TicketSigner::new(&key, 1, 60).is_ok());
    }

    #[test]
    fn ticket_round_trip_carries_every_layout_field() {
        let key = [9u8; MIN_TICKET_KEY_BYTES];
        let signer = TicketSigner::new(&key, 3, 120).unwrap();
        let (token, expires) = signer
            .mint(
                "u_alice",
                "sid0123456789abcdef0123456",
                7,
                crate::session::AuthLevel::Auth,
                crate::session::Features::empty().with(crate::session::Features::VALIDATED),
                0,
                1_000,
            )
            .unwrap();
        assert_eq!(expires, 1_120, "0 = 用配置缺省");
        let got = signer.verify(&token, 1_001).unwrap();
        assert_eq!(got.user_id, "u_alice");
        assert_eq!(got.generation, 7);
        assert_eq!(got.serial, 3);
        assert_eq!(got.level, crate::session::AuthLevel::Auth);
        assert!(got.features.contains(crate::session::Features::VALIDATED));
    }

    #[test]
    fn forged_and_rotated_tickets_are_indistinguishable() {
        let key = [11u8; MIN_TICKET_KEY_BYTES];
        let signer = TicketSigner::new(&key, 1, 60).unwrap();
        let (token, _) = signer
            .mint(
                "u_a",
                "s",
                1,
                crate::session::AuthLevel::Auth,
                crate::session::Features::empty(),
                60,
                1_000,
            )
            .unwrap();
        let other = TicketSigner::new(&[12u8; MIN_TICKET_KEY_BYTES], 1, 60).unwrap();
        let by_wrong_key = other.verify(&token, 1_001).unwrap_err();
        let rotated = TicketSigner::new(&key, 2, 60).unwrap();
        let by_rotated_serial = rotated.verify(&token, 1_001).unwrap_err();
        // 上游同一条：签名不符与序列号不符都是 ErrFailed。
        assert_eq!(format!("{by_wrong_key}"), format!("{by_rotated_serial}"));
        assert!(matches!(by_wrong_key, IdError::Unauthorized(_)));
        // 结构问题才是另一类错（能区分"胡乱输入"与"别人的真令牌"）。
        assert!(matches!(
            signer.verify("deadbeef", 1_001),
            Err(IdError::BadInput(_))
        ));
        assert!(matches!(
            signer.verify(&format!("{token}.."), 1_001),
            Err(IdError::BadInput(_))
        ));
    }

    #[test]
    fn ticket_expiry_uses_the_one_second_skew() {
        let key = [13u8; MIN_TICKET_KEY_BYTES];
        let signer = TicketSigner::new(&key, 1, 60).unwrap();
        let (token, expires) = signer
            .mint(
                "u_a",
                "s",
                1,
                crate::session::AuthLevel::Auth,
                crate::session::Features::empty(),
                60,
                1_000,
            )
            .unwrap();
        assert_eq!(expires, 1_060);
        // 还剩 1 秒 = 上游的 `expires.Before(now+1s)`：按已过期处理。
        assert!(matches!(
            signer.verify(&token, 1_059),
            Err(IdError::Expired)
        ));
        assert!(signer.verify(&token, 1_058).is_ok());
        // 负寿命：上游 `rec.Lifetime < 0 → ErrExpired`，拒签。
        assert!(matches!(
            signer.mint(
                "u_a",
                "s",
                1,
                crate::session::AuthLevel::Auth,
                crate::session::Features::empty(),
                -1,
                1_000
            ),
            Err(IdError::Expired)
        ));
    }

    #[test]
    fn lifetime_zero_uses_default_and_negative_refuses_to_issue() {
        let conn = db();
        session_row(&conn, "s1", 1_000);
        // 0 → 缺省 30 天（上游 GenSecret 的 "rec.Lifetime == 0" 分支）。
        let raw = issue(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, 0, 1_000).unwrap();
        let row = find(&conn, &raw).unwrap().expect("row");
        assert_eq!(row.expires_at, 1_000 + REFRESH_TTL_MS);
        // 负数：拒签，库里不留行。
        assert!(matches!(
            issue(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, -1, 1_000),
            Err(IdError::Expired)
        ));
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM identity_refresh", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn temp_auth_chain_is_killed_instead_of_rotated() {
        let conn = db();
        session_row(&conn, "s1", 1_000);
        let raw = issue_default(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, 1_000).unwrap();
        // 直接落一条带 `L` 的侧车行：等价于 `session::open_temp` 的产物，
        // 但不必把 device 注册那套搬进本模块的测试里。
        conn.execute(
            "INSERT INTO identity_session_token(sid, user_id, auth_level, features, serial, issued_at, expires_at) VALUES('s1', 'u1', 'auth', 'L', 0, 1000, 61000)",
            [],
        )
        .unwrap();
        let err = rotate(&conn, &raw, 1_100).unwrap_err();
        assert!(
            matches!(err, IdError::Unauthorized(m) if m.contains("temp auth")),
            "{err:?}"
        );
        // 整条链作废：再来一次也只能是失败，而不是"又试一次"。
        assert!(chain_of(&conn, "s1")
            .unwrap()
            .iter()
            .all(|row| row.revoked_at.is_some()));
    }

    #[test]
    fn device_revocation_kills_refresh() {
        let conn = db();
        session_row(&conn, "s1", 1_000);
        let raw = issue_default(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, 1_000).unwrap();
        assert_eq!(
            revoke_device(&conn, "u1", "dev-aaaa1111", 1_100).unwrap(),
            1
        );
        assert!(matches!(
            rotate(&conn, &raw, 1_200),
            Err(IdError::Unauthorized(_))
        ));
    }

    #[test]
    fn deleting_the_session_kills_refresh() {
        let conn = db();
        session_row(&conn, "s1", 1_000);
        let raw = issue_default(&conn, "u1", "s1", "dev-aaaa1111", Audience::User, 1_000).unwrap();
        conn.execute("DELETE FROM sessions WHERE sid = 's1'", [])
            .unwrap();
        assert!(matches!(
            rotate(&conn, &raw, 1_200),
            Err(IdError::Unauthorized("session revoked"))
        ));
    }
}
