//! 危险动作的确认：`prepare → confirmation_token → execute`。
//!
//! 为什么不是"在 UI 上点一下就行"：agent 会把**上一步的输出**当成下一步的输入。
//! 一次确认如果只绑"动作名"，那么"踢 A"的确认就能被拿去"踢 B"；
//! 只绑"会话"，换个金额也照样过。所以令牌必须绑死四件事：
//!
//! * **谁**（actor + session）：别人拿到令牌也用不了；
//! * **什么动作**（action_id）：不能拿"撤回消息"的确认去执行"移出群"；
//! * **哪些参数**（规范参数摘要）：改一个字摘要就变，必须重新确认；
//! * **何时过期 + 一次性**：过期不自动续，用过即作废（防重放）。
//!
//! 令牌本身是**不透明的一次性随机串**，权威记录在本地 store 里。
//! 这样即便有人伪造格式，也找不到对应记录；把摘要塞进令牌再自校验那种写法，
//! 在"没有签名密钥"的本地场景里只是自欺。

use std::collections::HashMap;

use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

use crate::auth::ActorContext;
use crate::catalog::{CommandSpec, Confirmation};

/// 确认有效期（秒）。够人读完一段计划再点确认，又不至于挂着半天。
pub const TTL_SECONDS: i64 = 120;

/// 令牌前缀：便于人在日志里一眼认出来这是确认令牌，不是会话令牌。
const TOKEN_PREFIX: &str = "wxc";

/// 确认令牌。`Display` 出的字符串就是交给调用方的东西。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConfirmationToken(String);

impl ConfirmationToken {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ConfirmationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// store 里的一条权威记录。
#[derive(Clone, Debug)]
struct Record {
    actor: String,
    session_id: String,
    action_id: &'static str,
    digest: String,
    expires_at: i64,
}

/// 确认失败的原因。每条都能让调用方知道下一步做什么。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfirmError {
    /// 这条命令不需要确认（调用方多此一举）。
    NotRequired(&'static str),
    /// 令牌不存在：伪造、已被使用，或来自另一个进程/会话。
    UnknownToken,
    /// 令牌过期：**不自动续**，必须重新 prepare。
    Expired { expires_at: i64, now: i64 },
    /// 动作对不上：拿 A 的确认执行 B。
    ActionMismatch {
        expected: &'static str,
        token_for: String,
    },
    /// 参数变了：换收款人/换金额/换消息都落在这里。
    ParamsChanged,
    /// 不是同一个人/同一个会话。
    ActorMismatch,
    /// 缺少确认令牌。
    Missing,
}

impl std::fmt::Display for ConfirmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfirmError::NotRequired(id) => {
                write!(f, "{id} does not require confirmation")
            }
            ConfirmError::UnknownToken => f.write_str(
                "confirmation token unknown or already used; run prepare again \
                 (tokens are single-use)",
            ),
            ConfirmError::Expired { expires_at, now } => write!(
                f,
                "confirmation expired at {expires_at} (now {now}); \
                 re-run prepare to get a fresh token"
            ),
            ConfirmError::ActionMismatch {
                expected,
                token_for,
            } => write!(
                f,
                "confirmation was issued for {token_for:?}, not {expected:?}"
            ),
            ConfirmError::ParamsChanged => f.write_str(
                "parameters changed since the confirmation was prepared; \
                 confirm the new parameters explicitly",
            ),
            ConfirmError::ActorMismatch => {
                f.write_str("confirmation belongs to another actor or session")
            }
            ConfirmError::Missing => write!(
                f,
                "this action requires confirmation: call prepare first, then pass \
                 `confirmation_token`"
            ),
        }
    }
}

impl std::error::Error for ConfirmError {}

/// 待执行的计划：给人和 agent 看的"将要发生什么"，**不含副作用**。
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub action_id: &'static str,
    pub summary: &'static str,
    pub params: Value,
    pub digest: String,
}

impl Plan {
    pub fn to_json(&self) -> Value {
        json!({
            "action_id": self.action_id,
            "summary": self.summary,
            "params": self.params,
            "digest": self.digest,
        })
    }
}

/// 一次 prepare 的结果。
#[derive(Clone, Debug)]
pub struct Prepared {
    pub plan: Plan,
    pub token: ConfirmationToken,
    pub expires_at: i64,
}

impl Prepared {
    /// 给 agent 的标准响应：`confirmation_required` + 计划 + 令牌。
    pub fn to_json(&self) -> Value {
        json!({
            "confirmation_required": true,
            "token": self.token.as_str(),
            "expires_at": self.expires_at,
            "plan": self.plan.to_json(),
        })
    }
}

/// 一次性确认的存储。进程内即可：令牌跨进程传递本身就没有意义
/// （换进程就是换了会话，见 [`ConfirmError::ActorMismatch`]）。
#[derive(Debug, Default)]
pub struct ConfirmationStore {
    records: HashMap<String, Record>,
}

impl ConfirmationStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 生成计划 + 令牌。**不执行**任何业务动作。
    pub fn prepare(
        &mut self,
        ctx: &ActorContext,
        spec: &CommandSpec,
        params: &Value,
        now: i64,
    ) -> Result<Prepared, ConfirmError> {
        if spec.confirmation != Confirmation::Required {
            return Err(ConfirmError::NotRequired(spec.id));
        }
        let digest = params_digest(params);
        let token = new_token();
        let expires_at = now + TTL_SECONDS;
        self.records.insert(
            token.as_str().to_string(),
            Record {
                actor: ctx.actor.clone(),
                session_id: ctx.session_id.clone(),
                action_id: spec.id,
                digest: digest.clone(),
                expires_at,
            },
        );
        Ok(Prepared {
            plan: Plan {
                action_id: spec.id,
                summary: spec.summary,
                params: strip_token(params),
                digest,
            },
            token,
            expires_at,
        })
    }

    /// 兑换令牌：校验通过就把记录删掉（一次性）。**不执行**业务动作。
    pub fn redeem(
        &mut self,
        ctx: &ActorContext,
        spec: &CommandSpec,
        params: &Value,
        token: &str,
        now: i64,
    ) -> Result<Plan, ConfirmError> {
        if spec.confirmation != Confirmation::Required {
            return Err(ConfirmError::NotRequired(spec.id));
        }
        if token.trim().is_empty() {
            return Err(ConfirmError::Missing);
        }
        let record = self
            .records
            .get(token)
            .cloned()
            .ok_or(ConfirmError::UnknownToken)?;
        self.check(&record, ctx, spec, params, now)?;
        // 过了全部检查才删除：校验失败时令牌仍然有效（可以让调用方改回来重试）
        self.records.remove(token);
        Ok(Plan {
            action_id: spec.id,
            summary: spec.summary,
            params: strip_token(params),
            digest: record.digest,
        })
    }

    fn check(
        &self,
        record: &Record,
        ctx: &ActorContext,
        spec: &CommandSpec,
        params: &Value,
        now: i64,
    ) -> Result<(), ConfirmError> {
        if record.actor_id_matches(ctx).is_err() {
            return Err(ConfirmError::ActorMismatch);
        }
        if record.expires_at <= now {
            return Err(ConfirmError::Expired {
                expires_at: record.expires_at,
                now,
            });
        }
        if record.action_id != spec.id {
            return Err(ConfirmError::ActionMismatch {
                expected: spec.id,
                token_for: record.action_id.to_string(),
            });
        }
        if record.digest != params_digest(params) {
            return Err(ConfirmError::ParamsChanged);
        }
        Ok(())
    }

    /// 当前未过期的令牌数（诊断用；令牌内容不外泄）。
    pub fn outstanding(&self, now: i64) -> usize {
        self.records.values().filter(|r| r.expires_at > now).count()
    }
}

impl Record {
    fn actor_id_matches(&self, ctx: &ActorContext) -> Result<(), ()> {
        if self.actor == ctx.actor && self.session_id == ctx.session_id {
            return Ok(());
        }
        Err(())
    }
}

/// 只出计划，不签发令牌（CLI 场景：人就在终端前，确认就是他按的回车）。
pub fn plan(spec: &CommandSpec, params: &Value) -> Plan {
    Plan {
        action_id: spec.id,
        summary: spec.summary,
        params: strip_token(params),
        digest: params_digest(params),
    }
}

/// 规范参数摘要：键排序 + 去掉确认令牌本身。
///
/// 为什么排序：`{"a":1,"b":2}` 与 `{"b":2,"a":1}` 是同一组参数，
/// 摘要必须相同，否则"原样重发"会被误判成"参数变了"。
pub fn params_digest(params: &Value) -> String {
    let canonical = canonical_json(&strip_token(params));
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 去掉确认令牌字段：令牌是**确认的载体**，不该成为被确认的参数。
fn strip_token(params: &Value) -> Value {
    let mut v = params.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("confirmation_token");
    }
    v
}

/// 键排序后的紧凑 JSON（只对对象递归重排，数组顺序保持不变）。
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", json!(k), canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

fn new_token() -> ConfirmationToken {
    // 128 bit 随机：够一次性，也短到人能在终端里复制
    let raw: [u8; 16] = rand::random();
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    ConfirmationToken(format!("{TOKEN_PREFIX}-{hex}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::context_from_parts;
    use crate::catalog;

    const NOW: i64 = 1_700_000_000;

    fn spec(id: &str) -> &'static CommandSpec {
        catalog::get(id).expect("spec")
    }

    fn kick_params(user: &str) -> Value {
        json!({"conv": "g-1", "user_id": user})
    }

    fn store_and_ctx() -> (ConfirmationStore, ActorContext) {
        (
            ConfirmationStore::new(),
            context_from_parts("u-alice", "s-1", "conv:*"),
        )
    }

    #[test]
    fn read_commands_never_need_confirmation() {
        let (mut store, ctx) = store_and_ctx();
        let err = store
            .prepare(&ctx, spec("conv.unread"), &json!({"conv": "g-1"}), NOW)
            .unwrap_err();
        assert_eq!(err, ConfirmError::NotRequired("conv.unread"));
    }

    #[test]
    fn prepare_then_redeem_executes_once() {
        let (mut store, ctx) = store_and_ctx();
        let prepared = store
            .prepare(&ctx, spec("group.kick"), &kick_params("u-bob"), NOW)
            .expect("prepare");
        assert_eq!(prepared.expires_at, NOW + TTL_SECONDS);
        assert!(prepared.token.as_str().starts_with("wxc-"));

        let plan = store
            .redeem(
                &ctx,
                spec("group.kick"),
                &kick_params("u-bob"),
                prepared.token.as_str(),
                NOW + 1,
            )
            .expect("redeem");
        assert_eq!(plan.action_id, "group.kick");
        assert_eq!(plan.params["user_id"], "u-bob");

        // 一次性：第二次必须失败
        let err = store
            .redeem(
                &ctx,
                spec("group.kick"),
                &kick_params("u-bob"),
                prepared.token.as_str(),
                NOW + 2,
            )
            .unwrap_err();
        assert_eq!(err, ConfirmError::UnknownToken, "重放必须被拒");
    }

    /// 换收款人/换成员/换金额：摘要变了就必须重新确认。
    #[test]
    fn changing_any_parameter_invalidates_the_token() {
        let (mut store, ctx) = store_and_ctx();
        let prepared = store
            .prepare(&ctx, spec("group.kick"), &kick_params("u-bob"), NOW)
            .expect("prepare");
        let err = store
            .redeem(
                &ctx,
                spec("group.kick"),
                &kick_params("u-evil"),
                prepared.token.as_str(),
                NOW + 1,
            )
            .unwrap_err();
        assert_eq!(err, ConfirmError::ParamsChanged);
        assert!(err.to_string().contains("changed"), "{err}");
    }

    #[test]
    fn confirmation_cannot_be_replayed_onto_another_action() {
        let (mut store, ctx) = store_and_ctx();
        let prepared = store
            .prepare(
                &ctx,
                spec("msg.recall"),
                &json!({"conv": "g-1", "msg_id": "m1"}),
                NOW,
            )
            .expect("prepare");
        let err = store
            .redeem(
                &ctx,
                spec("group.kick"),
                &kick_params("u-bob"),
                prepared.token.as_str(),
                NOW + 1,
            )
            .unwrap_err();
        assert!(
            matches!(err, ConfirmError::ActionMismatch { .. }),
            "{err:?}"
        );
    }

    /// 跨用户/跨会话：别人拿到令牌也用不了。
    #[test]
    fn confirmation_is_bound_to_actor_and_session() {
        let (mut store, ctx) = store_and_ctx();
        let prepared = store
            .prepare(&ctx, spec("group.kick"), &kick_params("u-bob"), NOW)
            .expect("prepare");

        let other_actor = context_from_parts("u-mallory", "s-1", "conv:*");
        assert_eq!(
            store
                .redeem(
                    &other_actor,
                    spec("group.kick"),
                    &kick_params("u-bob"),
                    prepared.token.as_str(),
                    NOW + 1
                )
                .unwrap_err(),
            ConfirmError::ActorMismatch
        );

        let other_session = context_from_parts("u-alice", "s-2", "conv:*");
        assert_eq!(
            store
                .redeem(
                    &other_session,
                    spec("group.kick"),
                    &kick_params("u-bob"),
                    prepared.token.as_str(),
                    NOW + 1
                )
                .unwrap_err(),
            ConfirmError::ActorMismatch
        );
        // 原主体仍然可用（失败的兑换不该消耗令牌）
        assert!(store
            .redeem(
                &ctx,
                spec("group.kick"),
                &kick_params("u-bob"),
                prepared.token.as_str(),
                NOW + 2
            )
            .is_ok());
    }

    /// 过期不自动继续：必须重新 prepare。
    #[test]
    fn expired_confirmation_never_auto_continues() {
        let (mut store, ctx) = store_and_ctx();
        let prepared = store
            .prepare(&ctx, spec("group.kick"), &kick_params("u-bob"), NOW)
            .expect("prepare");
        let err = store
            .redeem(
                &ctx,
                spec("group.kick"),
                &kick_params("u-bob"),
                prepared.token.as_str(),
                NOW + TTL_SECONDS,
            )
            .unwrap_err();
        assert!(
            matches!(err, ConfirmError::Expired { .. }),
            "到期即失效，不能滑动续期：{err:?}"
        );
        assert_eq!(store.outstanding(NOW + TTL_SECONDS), 0);
        assert_eq!(store.outstanding(NOW), 1);
    }

    #[test]
    fn unknown_and_missing_tokens_are_reported_distinctly() {
        let (mut store, ctx) = store_and_ctx();
        assert_eq!(
            store
                .redeem(&ctx, spec("group.kick"), &kick_params("u"), "", NOW)
                .unwrap_err(),
            ConfirmError::Missing
        );
        assert_eq!(
            store
                .redeem(
                    &ctx,
                    spec("group.kick"),
                    &kick_params("u"),
                    "wxc-forged",
                    NOW
                )
                .unwrap_err(),
            ConfirmError::UnknownToken
        );
    }

    /// 参数顺序不影响摘要：脚本原样重发不该被判成"参数变了"。
    #[test]
    fn digest_ignores_key_order_and_confirmation_token() {
        let a = json!({"conv": "g-1", "user_id": "u-bob"});
        let b = json!({"user_id": "u-bob", "conv": "g-1"});
        assert_eq!(params_digest(&a), params_digest(&b));

        let with_token = json!({
            "conv": "g-1", "user_id": "u-bob", "confirmation_token": "wxc-x"
        });
        assert_eq!(
            params_digest(&a),
            params_digest(&with_token),
            "确认令牌本身不能参与摘要"
        );
        assert_eq!(params_digest(&a).len(), 64, "sha256 十六进制");
    }

    #[test]
    fn plan_json_is_auditable_and_hides_the_token() {
        let (mut store, ctx) = store_and_ctx();
        let prepared = store
            .prepare(&ctx, spec("group.kick"), &kick_params("u-bob"), NOW)
            .expect("prepare");
        let payload = prepared.to_json();
        assert_eq!(payload["confirmation_required"], true);
        assert_eq!(payload["plan"]["action_id"], "group.kick");
        assert_eq!(payload["plan"]["params"]["user_id"], "u-bob");
        assert!(payload["token"]
            .as_str()
            .expect("token")
            .starts_with("wxc-"));
    }
}
