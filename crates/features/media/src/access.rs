//! Per-request read authorization and revocable short-lived links.
//!
//! Rules, in order: owner, a public reference, a direct user reference, or a
//! conversation/group reference confirmed by the injected audience port. The
//! SHA-256 address itself never satisfies any rule.

use std::sync::Arc;

use hmac::{Hmac, Mac as _};
use rusqlite::{params, Connection, OptionalExtension as _};
use sha2::Sha256;

use crate::error::{MediaError, Result};
use crate::ids;
use crate::model::{Audience, ObjectRecord, Purpose, RefType};
use crate::MediaService;

type HmacSha256 = Hmac<Sha256>;
pub const SIGNED_URL_PREFIX: &str = "/api/v1/objects";

/// Membership facts owned by G02/G03. A fixture is acceptable while those
/// cards are in flight, but the production composition root must inject the
/// real checks; this crate never infers membership from an id.
pub trait AudiencePort: Send + Sync {
    fn may_read_conversation(&self, user_id: &str, conv_id: &str) -> bool;
    fn may_read_group(&self, user_id: &str, group_id: &str) -> bool;

    /// Conversations whose content the user may read. Used to project the file
    /// center without scanning names. The default returns nothing: an empty
    /// projection hides shared files rather than leaking them.
    fn visible_conversations(&self, _user_id: &str) -> Vec<String> {
        Vec::new()
    }

    fn visible_groups(&self, _user_id: &str) -> Vec<String> {
        Vec::new()
    }
}

pub struct AccessGrant {
    pub user_id: String,
    pub object: ObjectRecord,
    pub audience: Option<Audience>,
}

#[derive(serde::Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SignedAccess {
    pub object_id: String,
    pub url: String,
    pub expires_at: i64,
    pub epoch: i64,
}

impl SignedAccess {
    /// The signature embedded in [`Self::url`], so callers can verify without
    /// parsing query strings themselves.
    pub fn signature(&self) -> String {
        self.url
            .rsplit_once("sig=")
            .map(|(_, sig)| sig.to_string())
            .unwrap_or_default()
    }
}

pub struct RefSpec<'a> {
    pub ref_type: RefType,
    pub ext_id: &'a str,
    pub owner: &'a str,
    pub audience: &'a Audience,
    pub purpose: Purpose,
}

pub fn object_by_id_conn(c: &Connection, object_id: &str) -> Result<Option<ObjectRecord>> {
    Ok(c.query_row(
        "SELECT object_id, owner, size, digest, mime, status, access_epoch, created_at
         FROM media_object WHERE object_id = ?1",
        params![object_id],
        |row| {
            Ok(ObjectRecord {
                object_id: row.get(0)?,
                owner: row.get(1)?,
                size: row.get::<_, i64>(2)?.max(0) as u64,
                digest: row.get(3)?,
                mime: row.get(4)?,
                status: row.get(5)?,
                access_epoch: row.get(6)?,
                created_at: row.get(7)?,
            })
        },
    )
    .optional()?)
}

/// Insert a logical reference. The partial unique index makes a retried
/// message/file operation a no-op instead of a second retention count.
pub fn add_ref_conn(c: &Connection, object_id: &str, spec: &RefSpec<'_>, now: i64) -> Result<()> {
    let ref_id = ids::opaque_id("ref");
    c.execute(
        "INSERT OR IGNORE INTO media_ref
            (ref_id, object_id, ref_type, ref_id_ext, owner, audience, purpose, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            ref_id,
            object_id,
            spec.ref_type.as_str(),
            spec.ext_id,
            spec.owner,
            spec.audience.key(),
            spec.purpose.as_str(),
            now
        ],
    )?;
    Ok(())
}

pub fn remove_ref_conn(
    c: &Connection,
    object_id: &str,
    ref_type: RefType,
    ext_id: &str,
    now: i64,
) -> Result<usize> {
    Ok(c.execute(
        "UPDATE media_ref SET deleted_at = ?1
         WHERE object_id = ?2 AND ref_type = ?3 AND ref_id_ext = ?4 AND deleted_at IS NULL",
        params![now, object_id, ref_type.as_str(), ext_id],
    )?)
}

fn audiences_allow(port: &dyn AudiencePort, user_id: &str, audiences: &[String]) -> bool {
    audiences.iter().any(|raw| {
        let Ok(audience) = serde_json::from_str::<Audience>(raw) else {
            return false;
        };
        match audience {
            Audience::Public => true,
            Audience::User { id } => id == user_id,
            Audience::Conversation { id } => port.may_read_conversation(user_id, &id),
            Audience::Group { id } => port.may_read_group(user_id, &id),
        }
    })
}

impl MediaService {
    pub fn object(&self, object_id: &str) -> Result<ObjectRecord> {
        ids::validate_object_id(object_id)?;
        object_by_id_conn(&self.db.lock(), object_id)?.ok_or(MediaError::NotFound)
    }

    /// Authorize one read. A missing object, a foreign object and a revoked
    /// reference all surface as `NotFound`/`Forbidden` without leaking which.
    pub fn authorize_read(&self, user_id: &str, object_id: &str) -> Result<AccessGrant> {
        ids::validate_object_id(object_id)?;
        let c = self.db.lock();
        let Some(object) = object_by_id_conn(&c, object_id)? else {
            return Err(MediaError::NotFound);
        };
        if !object.ready() {
            return Err(MediaError::NotFound);
        }
        if object.owner == user_id {
            return Ok(AccessGrant {
                user_id: user_id.to_string(),
                object,
                audience: None,
            });
        }
        let audiences: Vec<String> = {
            let mut stmt = c.prepare(
                "SELECT audience FROM media_ref
                 WHERE object_id = ?1 AND deleted_at IS NULL",
            )?;
            let rows = stmt.query_map(params![object_id], |row| row.get(0))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        drop(c);
        if audiences_allow(self.audience.as_ref(), user_id, &audiences) {
            return Ok(AccessGrant {
                user_id: user_id.to_string(),
                object,
                audience: None,
            });
        }
        Err(MediaError::Forbidden)
    }

    /// Issue a short-lived signature. The epoch makes revocation immediate:
    /// bumping it invalidates every link minted before the change.
    pub fn download_authorize(&self, user_id: &str, object_id: &str) -> Result<SignedAccess> {
        let grant = self.authorize_read(user_id, object_id)?;
        let ttl = self.config.signed_url_ttl_ms.max(1);
        let exp = crate::db::now_ms() + ttl;
        let epoch = grant.object.access_epoch;
        let sig = self.signature(object_id, epoch, exp);
        let url = format!("{SIGNED_URL_PREFIX}/{object_id}?exp={exp}&epoch={epoch}&sig={sig}");
        Ok(SignedAccess {
            object_id: object_id.to_string(),
            url,
            expires_at: exp,
            epoch,
        })
    }

    pub fn verify_signed(
        &self,
        object_id: &str,
        exp: i64,
        epoch: i64,
        sig: &str,
        now: i64,
    ) -> Result<ObjectRecord> {
        let object = self.object(object_id)?;
        if now > exp || epoch != object.access_epoch {
            return Err(MediaError::Gone("access link expired or revoked".into()));
        }
        let expected = self.signature(object_id, epoch, exp);
        if constant_time_eq(sig.as_bytes(), expected.as_bytes()) {
            Ok(object)
        } else {
            Err(MediaError::Forbidden)
        }
    }

    pub fn revoke_object_access(&self, owner_id: &str, object_id: &str) -> Result<i64> {
        let c = self.db.lock();
        let changed = c.execute(
            "UPDATE media_object SET access_epoch = access_epoch + 1
             WHERE object_id = ?1 AND owner = ?2 AND status = 'ready'",
            params![object_id, owner_id],
        )?;
        if changed == 0 {
            return Err(MediaError::NotFound);
        }
        Ok(object_by_id_conn(&c, object_id)?
            .map(|row| row.access_epoch)
            .unwrap_or_default())
    }

    fn signature(&self, object_id: &str, epoch: i64, exp: i64) -> String {
        let key = &self.config.signing_key;
        let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
        mac.update(format!("media-access:v1:{object_id}:{epoch}:{exp}").as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    pub fn rotate_signing_key(&mut self, key: impl AsRef<[u8]>) {
        self.config.signing_key = key.as_ref().to_vec();
    }

    pub fn signing_key_len(&self) -> usize {
        self.config.signing_key.len()
    }

    pub fn audience_port(&self) -> Arc<dyn AudiencePort> {
        Arc::clone(&self.audience)
    }
}

pub(crate) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}
