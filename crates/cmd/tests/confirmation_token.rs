//! **确认链路端到端用例**（#315）：`prepare → confirmation_token → execute`。
//!
//! `crates/cmd/src/catalog.rs` 的"写命令的标注规则"点名了这个文件——
//! 注释说"令牌一次性与'改参重确认'的用例在这里"，这里就真的把它们走通。
//! 与 `confirmation.rs` 的单元测试分工：单元测试逐条验**拒绝**形状
//! （过期/换动作/换主体各自的错误码），这里验**恢复路径**——
//! 被拒之后链路还能不能正确地继续走下去，以及全 catalog 的横切形状。
//! "execute" 一侧在本 crate 只有 `redeem` 这个闸（真正的执行在
//! `wxlike-client::Executor`，令牌在它之前就被确认层摘掉，见
//! `param_dto_parity.rs` 的 `CONSUMED_ELSEWHERE`），所以"执行恰好一次"
//! 在这里等价于"兑换恰好成功一次"。

use serde_json::json;
use wxlike_cmd::auth::context_from_parts;
use wxlike_cmd::catalog;
use wxlike_cmd::confirmation::{ConfirmError, ConfirmationStore, TTL_SECONDS};

const NOW: i64 = 1_700_000_000;

fn ctx() -> wxlike_cmd::auth::ActorContext {
    context_from_parts("u-alice", "s-1", "conv:*")
}

fn spec(id: &str) -> &'static catalog::CommandSpec {
    catalog::get(id).expect("spec")
}

fn kick(user: &str) -> serde_json::Value {
    json!({"conv": "g-1", "user_id": user})
}

/// **参数改动必须重新确认**（#315 复核的主用例）：
/// 旧令牌对新参数只回 `ParamsChanged`，且**不消耗**；
/// 对旧参数仍可兑换；换新参数必须重新 prepare 才能过闸。
#[test]
fn param_change_requires_a_fresh_confirmation_round() {
    let (mut store, actor) = (ConfirmationStore::new(), ctx());
    let old = store
        .prepare(&actor, spec("group.kick"), &kick("u-bob"), NOW)
        .expect("prepare");

    // 拿旧令牌执行新参数：必须被拒，且错误要说清"参数变了"
    let err = store
        .redeem(&actor, spec("group.kick"), &kick("u-mallory"), old.token.as_str(), NOW + 1)
        .unwrap_err();
    assert_eq!(err, ConfirmError::ParamsChanged, "{err}");

    // 被拒不消耗令牌：旧参数原样兑换仍然成功（CLI 重试同一动作不该把人再问一遍）
    assert!(store
        .redeem(&actor, spec("group.kick"), &kick("u-bob"), old.token.as_str(), NOW + 2)
        .is_ok());

    // 新参数走新链路：不 prepare 就没有合法令牌
    assert_eq!(
        store
            .redeem(&actor, spec("group.kick"), &kick("u-mallory"), old.token.as_str(), NOW + 3)
            .unwrap_err(),
        ConfirmError::UnknownToken,
        "用过的令牌再拿来做任何事都必须不认识"
    );
    let fresh = store
        .prepare(&actor, spec("group.kick"), &kick("u-mallory"), NOW + 4)
        .expect("re-prepare");
    assert_ne!(fresh.token.as_str(), old.token.as_str());
    assert!(store
        .redeem(&actor, spec("group.kick"), &kick("u-mallory"), fresh.token.as_str(), NOW + 5)
        .is_ok());
    assert_eq!(store.outstanding(NOW + 5), 0, "链路走完不该留悬挂令牌");
}

/// **令牌一次性**是全 catalog 的形状，不是某条命令的巧合：
/// 对每条 `Confirmation::Required` 的命令都走一遍"成功兑换→再兑换必红"。
#[test]
fn every_required_confirmation_is_single_use() {
    let required: Vec<&str> = catalog::CATALOG
        .iter()
        .filter(|s| s.confirmation.is_required())
        .map(|s| s.id)
        .collect();
    assert_eq!(
        required,
        vec!["group.kick", "msg.recall"],
        "需要确认的集合变了：这里是全量遍历的口径，catalog 注释与 #315 结论要一起改"
    );
    let params = |id: &str| match id {
        "group.kick" => kick("u-bob"),
        _ => json!({"conv": "g-1", "msg_id": "m-1"}),
    };
    for id in required {
        let (mut store, actor) = (ConfirmationStore::new(), ctx());
        let prepared = store
            .prepare(&actor, spec(id), &params(id), NOW)
            .expect("prepare");
        assert_eq!(prepared.expires_at, NOW + TTL_SECONDS);
        assert!(
            store
                .redeem(&actor, spec(id), &params(id), prepared.token.as_str(), NOW + 1)
                .is_ok(),
            "{id} 首次兑换"
        );
        assert_eq!(
            store
                .redeem(&actor, spec(id), &params(id), prepared.token.as_str(), NOW + 2)
                .unwrap_err(),
            ConfirmError::UnknownToken,
            "{id}：用过即作废，重放必须被拒"
        );
    }
}

/// 各类**校验失败都不许消耗**令牌（改错了还能改回来），
/// 唯独成功兑换消耗——这是"一次性"的另一半。
#[test]
fn failed_redemptions_never_consume_the_token() {
    let (mut store, actor) = (ConfirmationStore::new(), ctx());
    let prepared = store
        .prepare(&actor, spec("msg.recall"), &json!({"conv": "g-1", "msg_id": "m-1"}), NOW)
        .expect("prepare");
    let t = prepared.token.as_str();

    // 错的主体
    assert_eq!(
        store
            .redeem(&context_from_parts("u-mallory", "s-1", "conv:*"), spec("msg.recall"),
                &json!({"conv": "g-1", "msg_id": "m-1"}), t, NOW + 1)
            .unwrap_err(),
        ConfirmError::ActorMismatch
    );
    // 拿召回的确认去踢人
    assert!(matches!(
        store
            .redeem(&actor, spec("group.kick"), &kick("u-bob"), t, NOW + 2)
            .unwrap_err(),
        ConfirmError::ActionMismatch { .. }
    ));
    // 错的参数
    assert_eq!(
        store
            .redeem(&actor, spec("msg.recall"), &json!({"conv": "g-1", "msg_id": "m-2"}), t, NOW + 3)
            .unwrap_err(),
        ConfirmError::ParamsChanged
    );
    // 原样重发：仍然可用
    assert!(store
        .redeem(&actor, spec("msg.recall"), &json!({"conv": "g-1", "msg_id": "m-1"}), t, NOW + 4)
        .is_ok());
}

/// 不需要确认的命令**连 prepare 都过不去**：全 catalog 横切一遍，
/// 防止"读命令也能签发令牌"这种形状（#315 撤 `msg.get` 装饰时踩过的坑）。
#[test]
fn non_required_commands_cannot_hold_a_token_at_all() {
    let (mut store, actor) = (ConfirmationStore::new(), ctx());
    for s in catalog::CATALOG {
        if s.confirmation.is_required() {
            continue;
        }
        let err = store
            .prepare(&actor, s, &json!({}), NOW)
            .unwrap_err();
        assert_eq!(err, ConfirmError::NotRequired(s.id), "{}", s.id);
    }
    assert_eq!(store.outstanding(NOW), 0, "一轮尝试不该留下任何令牌");
}
