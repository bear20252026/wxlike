//! WP-12 pre-key directory: `POST`/`GET /api/keys`.
//!
//! The protocol needs a place where a caller can find a peer's Olm identity
//! key plus a *fresh* one-time key. The one-time part is the whole point of
//! X3DH-style handshakes: every claim must hand out a different key, or two
//! callers starting sessions with the same person silently share secrets. So
//! `GET /api/keys/{user_id}` **claims**: it pops one key from the stored bundle
//! in a transaction and never returns it again. A bundle that runs dry returns
//! an empty `one_time_keys` list — the caller retries after the owner tops up
//! (`Crypto::bundle_refilled` exists for exactly that).
//!
//! **领域层**：本模块只含业务规则与类型，SQL 一律走 [`crate::domain::repo::keys`]。

use crate::error::{PlatformError, Result};
use crate::validate;
use crate::Platform;
use wxlike_core::e2ee::PreKeyBundle;

use super::repo::keys::{bundle_row, identity_for, shares_group, update_otks, upsert_bundle};

/// Sanity cap: nobody needs more one-time keys published than this.
const MAX_PUBLISHED_OTKS: usize = 256;

/// `otks` column format: one JSON array entry per key, stored as
/// `[key, signature]` pairs so a claim pops the two halves atomically.
/// Entries from pre-signature databases are plain strings; they map to
/// `(key, "")` and — being unverifiable — are rejected downstream (fail
/// closed), which is the intended fate of legacy unsigned bundles.
fn serialize_otk_pairs(bundle: &PreKeyBundle) -> Result<String> {
    let pairs: Vec<[String; 2]> = bundle
        .one_time_keys
        .iter()
        .zip(&bundle.one_time_key_signatures)
        .map(|(k, s)| [k.clone(), s.clone()])
        .collect();
    Ok(serde_json::to_string(&pairs)?)
}

fn parse_otk_pairs(json: &str) -> Result<Vec<(String, String)>> {
    let raw: Vec<serde_json::Value> = serde_json::from_str(json)?;
    Ok(raw
        .into_iter()
        .map(|v| match v {
            serde_json::Value::Array(pair) => (
                pair.first()
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
                pair.get(1)
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
            serde_json::Value::String(key) => (key, String::new()),
            _ => (String::new(), String::new()),
        })
        .collect())
}

impl Platform {
    /// Upserts the caller's pre-key bundle. Replaces whatever was there: a
    /// re-publish invalidates all previously stored one-time keys, which is
    /// the right thing when the device lost its account state.
    pub fn publish_keys(&self, user_id: &str, bundle: &PreKeyBundle) -> Result<()> {
        if bundle.user_id != user_id {
            return Err(PlatformError::Forbidden);
        }
        if bundle.identity.is_empty() || bundle.identity.len() > 128 {
            return Err(PlatformError::BadInput("identity key malformed"));
        }
        if bundle.one_time_keys.len() > MAX_PUBLISHED_OTKS {
            return Err(PlatformError::BadInput("too many one-time keys"));
        }
        for key in &bundle.one_time_keys {
            if key.is_empty() || key.len() > 128 {
                return Err(PlatformError::BadInput("one-time key malformed"));
            }
        }
        // Directory hygiene (AR-1/C-1): every stored bundle must be a real,
        // self-signed key set. The server cannot prove *ownership* (that is
        // the client-side `encrypt_dm` verification), but it can refuse to
        // store junk — garbage keys would make the user unreachable, and an
        // unsigned bundle would be dead weight: senders reject it anyway.
        if bundle.validate_shape().is_err() {
            return Err(PlatformError::BadInput(
                "bundle keys are not valid curve points",
            ));
        }
        bundle
            .verify_signature()
            .map_err(|_| PlatformError::BadInput("bundle is not signed by its identity key"))?;
        let otks = serialize_otk_pairs(bundle)?;
        let c = self.db().acquire();
        upsert_bundle(
            &c,
            user_id,
            &bundle.identity,
            &bundle.identity_signing,
            &bundle.signature,
            &otks,
            wxlike_core::types::now_ms() as i64,
        )?;
        Ok(())
    }

    /// 认领关系校验（AR-2/H-4）：本人、好友、或同群成员。
    ///
    /// 预密钥是"能给我发消息的人"才需要的东西——把认领限定在已有关系内，
    /// 陌生账号就不能把库存领空。同群但非好友的成员也要能领：群聊的
    /// sender key 走 1:1 Olm 通道分发。
    pub fn may_claim_keys_of(&self, caller: &str, target: &str) -> Result<bool> {
        if caller == target {
            return Ok(true);
        }
        if self.are_friends(caller, target)? {
            return Ok(true);
        }
        let c = self.db().acquire();
        shares_group(&c, caller, target)
    }

    /// 对方当前发布的身份公钥（秘密聊天安全码用；不消费一次性密钥）。
    pub fn public_identity(&self, user_id: &str) -> Result<Option<String>> {
        let c = self.db().acquire();
        identity_for(&c, user_id)
    }

    /// The caller's own bundle as stored (no claim). Lets a client see how
    /// many one-time keys it still has published and decide to top up.
    pub fn own_keys(&self, user_id: &str) -> Result<PreKeyBundle> {
        let c = self.db().acquire();
        match bundle_row(&c, user_id)? {
            Some(row) => {
                let pairs = parse_otk_pairs(&row.otks)?;
                Ok(PreKeyBundle {
                    user_id: user_id.to_string(),
                    identity: row.identity,
                    identity_signing: row.identity_signing,
                    signature: row.signature,
                    one_time_keys: pairs.iter().map(|(k, _)| k.clone()).collect(),
                    one_time_key_signatures: pairs.iter().map(|(_, s)| s.clone()).collect(),
                })
            }
            None => Ok(PreKeyBundle {
                user_id: user_id.to_string(),
                identity: String::new(),
                identity_signing: String::new(),
                signature: String::new(),
                one_time_keys: Vec::new(),
                one_time_key_signatures: Vec::new(),
            }),
        }
    }

    /// Pops one one-time key for `user_id` and returns it as a single-key
    /// bundle. `None` = the user published nothing; an empty `one_time_keys`
    /// = published but drained (the owner must top up).
    pub fn claim_key(&self, user_id: &str) -> Result<Option<PreKeyBundle>> {
        let user_id = validate::id(user_id)?;
        // The pop-and-update runs under one guard, so two concurrent claims
        // cannot read the same array.
        let c = self.db().acquire();
        let Some(row) = bundle_row(&c, user_id)? else {
            return Ok(None);
        };
        let mut pairs = parse_otk_pairs(&row.otks)?;
        let claimed = pairs.pop().unwrap_or_default();
        update_otks(
            &c,
            user_id,
            &serde_json::to_string(&pairs)?,
            wxlike_core::types::now_ms() as i64,
        )?;
        Ok(Some(PreKeyBundle {
            user_id: user_id.to_string(),
            identity: row.identity,
            identity_signing: row.identity_signing,
            signature: row.signature,
            one_time_keys: if claimed.0.is_empty() {
                Vec::new()
            } else {
                vec![claimed.0]
            },
            one_time_key_signatures: if claimed.1.is_empty() {
                Vec::new()
            } else {
                vec![claimed.1]
            },
        }))
    }
}
