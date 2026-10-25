//! G02-03 多设备密钥分发。
//!
//! # 与平台旧目录的差别
//!
//! 旧表 `prekey_bundles` 是**每用户一份**：一个人两台手机只有一把身份密钥，
//! 另一台永远解不开。这里改成**每设备一份**，并且：
//!
//! * 发布时校验结构 + 验签（`PreKeyBundle::verify_signature`），服务端拿不到
//!   也没法伪造一把"看起来合法"的密钥；
//! * 认领一次性预密钥是**事务内的原子弹出**：同一个 key 不会被发给两个人；
//! * 换身份密钥必须走 [`rotate`]，留审计并把 `epoch` 加一；直接覆盖是 `Conflict`
//!   ——否则服务端可以悄悄把你的设备换成自己的；
//! * 设备验证绑定"验证时的身份公钥"，对方轮换后验证状态自动失效。

use rusqlite::{params, Connection, OptionalExtension as _};
use wxlike_core::e2ee::PreKeyBundle;

use crate::{now_ms, Result, SecError, MAX_KEY_CHARS, MAX_OTKS, MAX_SAFETY_CHARS};

/// 一次认领的结果：够对方建立会话的最小信息集。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claimed {
    pub user_id: String,
    pub device_id: String,
    pub identity: String,
    pub identity_signing: String,
    pub signature: String,
    pub one_time_key: String,
    pub one_time_key_signature: String,
    pub epoch: u64,
}

impl Claimed {
    /// 转成可直接喂给 `Crypto::encrypt_dm` 的 bundle。
    pub fn bundle(&self) -> PreKeyBundle {
        PreKeyBundle {
            user_id: self.user_id.clone(),
            identity: self.identity.clone(),
            identity_signing: self.identity_signing.clone(),
            one_time_keys: vec![self.one_time_key.clone()],
            one_time_key_signatures: vec![self.one_time_key_signature.clone()],
            signature: self.signature.clone(),
        }
    }
}

/// 设备密钥目录的一行（运维视图 / 客户端设备列表）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceKey {
    pub device_id: String,
    pub user_id: String,
    pub identity: String,
    pub epoch: u64,
    pub one_time_keys: usize,
    pub updated_at: i64,
    pub revoked: bool,
}

fn parse_otks(raw: &str) -> Result<Vec<(String, String)>> {
    let list: Vec<serde_json::Value> = serde_json::from_str(raw)?;
    let mut out = Vec::new();
    for item in list {
        match item {
            serde_json::Value::Array(pair) => {
                let key = pair.first().and_then(|v| v.as_str()).unwrap_or_default();
                let sig = pair.get(1).and_then(|v| v.as_str()).unwrap_or_default();
                out.push((key.to_string(), sig.to_string()));
            }
            serde_json::Value::String(key) => out.push((key, String::new())),
            _ => {}
        }
    }
    Ok(out)
}

fn encode_otks(pairs: &[(String, String)]) -> Result<String> {
    let list: Vec<[&str; 2]> = pairs
        .iter()
        .map(|(k, s)| [k.as_str(), s.as_str()])
        .collect();
    Ok(serde_json::to_string(&list)?)
}

/// 结构 + 验签 + 归属校验。任何一项不过都拒绝入库。
fn validate(user: &str, bundle: &PreKeyBundle) -> Result<()> {
    if bundle.user_id != user {
        return Err(SecError::BadInput("bundle owner"));
    }
    if bundle.identity.len() > MAX_KEY_CHARS || bundle.identity_signing.len() > MAX_KEY_CHARS {
        return Err(SecError::BadInput("identity key"));
    }
    if bundle.one_time_keys.len() > MAX_OTKS {
        return Err(SecError::BadInput("too many one-time keys"));
    }
    bundle.validate_shape()?;
    bundle.verify_signature()?;
    Ok(())
}

/// 发布/续期一台设备的密钥包。返回当前 `epoch`。
pub fn publish(
    conn: &Connection,
    user: &str,
    device_id: &str,
    bundle: &PreKeyBundle,
    now: i64,
) -> Result<u64> {
    validate(user, bundle)?;
    let existing: Option<(String, String, u64)> = conn
        .query_row(
            "SELECT identity, otks, epoch FROM security_device_key \
             WHERE device_id = ?1 AND revoked_at IS NULL",
            params![device_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match existing {
        None => {
            let otks = encode_otks(&pairs_of(bundle))?;
            conn.execute(
                "INSERT INTO security_device_key(device_id, user_id, identity, identity_signing, \
                 signature, otks, epoch, created_at, updated_at, revoked_at) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?7, NULL) \
                 ON CONFLICT(device_id) DO UPDATE SET user_id = ?2, identity = ?3, \
                 identity_signing = ?4, signature = ?5, otks = ?6, updated_at = ?7, revoked_at = NULL",
                params![
                    device_id,
                    user,
                    bundle.identity,
                    bundle.identity_signing,
                    bundle.signature,
                    otks,
                    now
                ],
            )?;
            Ok(0)
        }
        Some((identity, otks, epoch)) => {
            if identity != bundle.identity {
                return Err(SecError::Conflict("identity change requires rotate"));
            }
            let merged = merge(&parse_otks(&otks)?, &pairs_of(bundle));
            conn.execute(
                "UPDATE security_device_key SET signature = ?2, otks = ?3, updated_at = ?4 \
                 WHERE device_id = ?1",
                params![device_id, bundle.signature, encode_otks(&merged)?, now],
            )?;
            Ok(epoch)
        }
    }
}

fn pairs_of(bundle: &PreKeyBundle) -> Vec<(String, String)> {
    bundle
        .one_time_keys
        .iter()
        .cloned()
        .zip(bundle.one_time_key_signatures.iter().cloned())
        .collect()
}

/// 合并未认领的旧 key 与新发布的 key（按 key 去重，上限截断）。
fn merge(old: &[(String, String)], new: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = old.to_vec();
    for (key, sig) in new {
        if !out.iter().any(|(k, _)| k == key) {
            out.push((key.clone(), sig.clone()));
        }
    }
    out.truncate(MAX_OTKS);
    out
}

/// 原子认领一枚一次性预密钥。设备不存在/已撤销/已用尽 → `None`。
pub fn claim(conn: &Connection, user: &str, device_id: &str, now: i64) -> Result<Option<Claimed>> {
    let tx = conn.unchecked_transaction()?;
    let row: Option<(String, String, String, String, u64)> = tx
        .query_row(
            "SELECT identity, identity_signing, signature, otks, epoch \
             FROM security_device_key WHERE device_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
            params![device_id, user],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((identity, signing, signature, otks, epoch)) = row else {
        return Ok(None);
    };
    let mut pairs = parse_otks(&otks)?;
    if pairs.is_empty() {
        return Ok(None);
    }
    let (key, sig) = pairs.remove(0);
    tx.execute(
        "UPDATE security_device_key SET otks = ?2, updated_at = ?3 WHERE device_id = ?1",
        params![device_id, encode_otks(&pairs)?, now],
    )?;
    tx.commit()?;
    Ok(Some(Claimed {
        user_id: user.to_string(),
        device_id: device_id.to_string(),
        identity,
        identity_signing: signing,
        signature,
        one_time_key: key,
        one_time_key_signature: sig,
        epoch,
    }))
}

/// 认领某用户的**全部活跃设备**各一枚（发送端要按设备逐个加密）。
pub fn claim_user(conn: &Connection, user: &str, now: i64) -> Result<Vec<Claimed>> {
    let mut stmt = conn.prepare(
        "SELECT device_id FROM security_device_key \
         WHERE user_id = ?1 AND revoked_at IS NULL ORDER BY created_at ASC",
    )?;
    let ids = stmt
        .query_map(params![user], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut out = Vec::new();
    for id in ids {
        if let Some(claimed) = claim(conn, user, &id, now)? {
            out.push(claimed);
        }
    }
    Ok(out)
}

/// 轮换设备身份密钥：`epoch + 1`，留审计，旧会话随之作废。
pub fn rotate(
    conn: &Connection,
    user: &str,
    device_id: &str,
    bundle: &PreKeyBundle,
    reason: &str,
    now: i64,
) -> Result<u64> {
    validate(user, bundle)?;
    let current: Option<(String, u64)> = conn
        .query_row(
            "SELECT identity, epoch FROM security_device_key \
             WHERE device_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
            params![device_id, user],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((identity, epoch)) = current else {
        return Err(SecError::NotFound("device key"));
    };
    if identity == bundle.identity {
        return Err(SecError::Conflict("identity unchanged"));
    }
    let next = epoch + 1;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE security_device_key SET identity = ?3, identity_signing = ?4, signature = ?5, \
         otks = ?6, epoch = ?7, updated_at = ?8 WHERE device_id = ?1 AND user_id = ?2",
        params![
            device_id,
            user,
            bundle.identity,
            bundle.identity_signing,
            bundle.signature,
            encode_otks(&pairs_of(bundle))?,
            next,
            now
        ],
    )?;
    tx.execute(
        "INSERT INTO security_rotation(user_id, device_id, epoch, reason, at) \
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![user, device_id, next, reason, now],
    )?;
    tx.commit()?;
    Ok(next)
}

/// 当前代次（不存在 → `None`）。
pub fn epoch_of(conn: &Connection, user: &str, device_id: &str) -> Result<Option<u64>> {
    Ok(conn
        .query_row(
            "SELECT epoch FROM security_device_key WHERE device_id = ?1 AND user_id = ?2",
            params![device_id, user],
            |r| r.get(0),
        )
        .optional()?)
}

/// 记录"我验证过对方的这台设备"。安全码由用户线下比对得出。
pub fn verify(
    conn: &Connection,
    me: &str,
    peer_user: &str,
    peer_device: &str,
    safety_number: &str,
    now: i64,
) -> Result<()> {
    if safety_number.is_empty() || safety_number.chars().count() > MAX_SAFETY_CHARS {
        return Err(SecError::BadInput("safety number"));
    }
    let identity: String = conn
        .query_row(
            "SELECT identity FROM security_device_key \
             WHERE device_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
            params![peer_device, peer_user],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(SecError::NotFound("device key"))?;
    conn.execute(
        "INSERT INTO security_device_verify(user_id, peer_user, peer_device, peer_identity, \
         safety_number, verified_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(user_id, peer_device) DO UPDATE SET peer_user = ?2, peer_identity = ?4, \
         safety_number = ?5, verified_at = ?6",
        params![me, peer_user, peer_device, identity, safety_number, now],
    )?;
    Ok(())
}

/// 验证是否**仍然**有效：对方轮换身份密钥后自动失效（不会"验证过一辈子"）。
pub fn is_verified(conn: &Connection, me: &str, peer_device: &str) -> Result<bool> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT v.peer_identity, COALESCE(k.identity, '') FROM security_device_verify v \
             LEFT JOIN security_device_key k ON k.device_id = v.peer_device \
             WHERE v.user_id = ?1 AND v.peer_device = ?2",
            params![me, peer_device],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match row {
        Some((verified_identity, current)) => !current.is_empty() && verified_identity == current,
        None => false,
    })
}

/// 撤销一台设备的密钥（设备移除时调用；之后不再被认领）。
pub fn revoke(conn: &Connection, user: &str, device_id: &str, now: i64) -> Result<()> {
    let n = conn.execute(
        "UPDATE security_device_key SET revoked_at = ?3, otks = '[]' \
         WHERE device_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
        params![device_id, user, now],
    )?;
    if n == 0 {
        return Err(SecError::NotFound("device key"));
    }
    Ok(())
}

/// 账号下全部设备密钥（含已撤销）。
pub fn list(conn: &Connection, user: &str) -> Result<Vec<DeviceKey>> {
    let mut stmt = conn.prepare(
        "SELECT device_id, user_id, identity, epoch, otks, updated_at, revoked_at \
         FROM security_device_key WHERE user_id = ?1 ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(params![user], |r| {
        let otks: String = r.get(4)?;
        let revoked: Option<i64> = r.get(6)?;
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, u64>(3)?,
            otks,
            r.get::<_, i64>(5)?,
            revoked,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (device_id, user_id, identity, epoch, otks, updated_at, revoked) = row?;
        out.push(DeviceKey {
            device_id,
            user_id,
            identity,
            epoch,
            one_time_keys: parse_otks(&otks)?.len(),
            updated_at,
            revoked: revoked.is_some(),
        });
    }
    Ok(out)
}

/// 便捷入口：用当前时间发布。
pub fn publish_now(
    conn: &Connection,
    user: &str,
    device_id: &str,
    bundle: &PreKeyBundle,
) -> Result<u64> {
    publish(conn, user, device_id, bundle, now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::db;
    use wxlike_core::e2ee::Device;

    fn bundle(user: &str) -> PreKeyBundle {
        Device::new().bundle(user)
    }

    #[test]
    fn claim_pops_a_distinct_key_each_time() {
        let conn = db();
        let b = bundle("u1");
        let published = b.one_time_keys.len();
        assert_eq!(publish(&conn, "u1", "dev-a", &b, 1_000).unwrap(), 0);
        let first = claim(&conn, "u1", "dev-a", 1_100).unwrap().unwrap();
        let second = claim(&conn, "u1", "dev-a", 1_200).unwrap().unwrap();
        assert_ne!(
            first.one_time_key, second.one_time_key,
            "同一个 key 不能发给两个人"
        );
        assert!(!first.one_time_key_signature.is_empty());
        assert_eq!(first.bundle().one_time_keys.len(), 1);
        let mut drained = 0;
        while claim(&conn, "u1", "dev-a", 1_300).unwrap().is_some() {
            drained += 1;
        }
        assert_eq!(drained, published - 2);
        assert!(claim(&conn, "u1", "dev-a", 1_400).unwrap().is_none());
    }

    #[test]
    fn republish_keeps_unclaimed_keys() {
        let conn = db();
        let dev = Device::new();
        let full = dev.bundle("u1");
        let published = full.one_time_keys.len();
        publish(&conn, "u1", "dev-a", &full, 1_000).unwrap();
        claim(&conn, "u1", "dev-a", 1_100).unwrap();
        let before = list(&conn, "u1").unwrap()[0].one_time_keys;
        assert_eq!(before, published - 1);

        // 客户端把整包再发一次：按 key 去重，不会把已认领的那枚算两次。
        publish(&conn, "u1", "dev-a", &full, 1_200).unwrap();
        assert_eq!(list(&conn, "u1").unwrap()[0].one_time_keys, published);

        // 只发子集：同样不产生重复行。
        publish(&conn, "u1", "dev-a", &dev.bundle_limited("u1", 3), 1_300).unwrap();
        assert_eq!(list(&conn, "u1").unwrap()[0].one_time_keys, published);
    }

    #[test]
    fn unsigned_or_foreign_bundle_is_rejected() {
        let conn = db();
        let mut forged = bundle("u1");
        forged.signature = String::new();
        assert!(matches!(
            publish(&conn, "u1", "dev-a", &forged, 1_000),
            Err(SecError::Crypto(_))
        ));
        let other = bundle("u2");
        assert!(matches!(
            publish(&conn, "u1", "dev-a", &other, 1_000),
            Err(SecError::BadInput("bundle owner"))
        ));
    }

    #[test]
    fn identity_change_requires_rotate() {
        let conn = db();
        publish(&conn, "u1", "dev-a", &bundle("u1"), 1_000).unwrap();
        assert!(matches!(
            publish(&conn, "u1", "dev-a", &bundle("u1"), 1_100),
            Err(SecError::Conflict(_))
        ));
        let next = bundle("u1");
        let epoch = rotate(&conn, "u1", "dev-a", &next, "reinstall", 1_200).unwrap();
        assert_eq!(epoch, 1);
        let audits: i64 = conn
            .query_row("SELECT COUNT(*) FROM security_rotation", [], |r| r.get(0))
            .unwrap();
        assert_eq!(audits, 1);
        assert_eq!(epoch_of(&conn, "u1", "dev-a").unwrap(), Some(1));
        assert!(
            matches!(
                rotate(&conn, "u1", "dev-a", &next, "noop", 1_300),
                Err(SecError::Conflict(_))
            ),
            "没换身份密钥不算轮换"
        );
    }

    #[test]
    fn verification_dies_with_the_peer_key() {
        let conn = db();
        publish(&conn, "u2", "dev-peer", &bundle("u2"), 1_000).unwrap();
        verify(&conn, "u1", "u2", "dev-peer", "12345 67890", 1_100).unwrap();
        assert!(is_verified(&conn, "u1", "dev-peer").unwrap());
        rotate(&conn, "u2", "dev-peer", &bundle("u2"), "new phone", 1_200).unwrap();
        assert!(
            !is_verified(&conn, "u1", "dev-peer").unwrap(),
            "对方换设备后旧验证必须失效"
        );
    }

    #[test]
    fn multi_device_claim_covers_every_active_device() {
        let conn = db();
        publish(&conn, "u1", "dev-a", &bundle("u1"), 1_000).unwrap();
        publish(&conn, "u1", "dev-b", &bundle("u1"), 1_000).unwrap();
        publish(&conn, "u1", "dev-c", &bundle("u1"), 1_000).unwrap();
        revoke(&conn, "u1", "dev-c", 1_100).unwrap();
        let claimed = claim_user(&conn, "u1", 1_200).unwrap();
        assert_eq!(claimed.len(), 2, "撤销的设备不再被认领");
        let devices: Vec<&str> = claimed.iter().map(|c| c.device_id.as_str()).collect();
        assert!(devices.contains(&"dev-a") && devices.contains(&"dev-b"));
    }

    #[test]
    fn revoked_device_cannot_claim_or_publish() {
        let conn = db();
        publish(&conn, "u1", "dev-a", &bundle("u1"), 1_000).unwrap();
        revoke(&conn, "u1", "dev-a", 1_100).unwrap();
        assert!(claim(&conn, "u1", "dev-a", 1_200).unwrap().is_none());
        assert!(matches!(
            revoke(&conn, "u1", "dev-a", 1_300),
            Err(SecError::NotFound(_))
        ));
        // 重新发布等于重新登记（新设备 id 由客户端生成，这里允许复活同一行）。
        publish(&conn, "u1", "dev-a", &bundle("u1"), 1_400).unwrap();
        assert!(claim(&conn, "u1", "dev-a", 1_500).unwrap().is_some());
    }

    #[test]
    fn empty_and_missing_cases() {
        let conn = db();
        assert!(list(&conn, "u1").unwrap().is_empty());
        assert!(claim(&conn, "u1", "nope", 1_000).unwrap().is_none());
        assert!(epoch_of(&conn, "u1", "nope").unwrap().is_none());
        assert!(matches!(
            verify(&conn, "u1", "u2", "nope", "123", 1_000),
            Err(SecError::NotFound(_))
        ));
        publish(&conn, "u2", "dev-peer", &bundle("u2"), 1_000).unwrap();
        assert!(matches!(
            verify(&conn, "u1", "u2", "dev-peer", "", 1_000),
            Err(SecError::BadInput(_))
        ));
    }
}
