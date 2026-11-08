//! `Confirmation::HumanOnly` 的**行为**用例：闸门是不是真能咬。
//!
//! 为什么单独一个文件，而不是塞进 `contract_v1_baseline.rs`：那边守的是
//! catalog 的形状（`payment_and_admin_commands_are_human_only`），而这条
//! 不变量**今天是空跑的**——catalog 里一条 `Payment`/`Admin` 命令都没有，
//! G06 的钱包与 G13/G14 的后台都还挂在覆盖表的 `blocked` 上。空跑的断言
//! 会一直绿，绿到什么也不能证明。所以这里用**合成命令**把闸门本身打红：
//! 同一个 `CommandSpec`，只把 `confirmation` 从 `Required` 换成 `HumanOnly`，
//! 无人值守通道就必须拒、人守着的通道就必须放行。换错了等级也要能被发现
//! （`required_still_passes_both_channels` 专抓"把闸门写成无条件拒绝"）。
//!
//! 分工还有第二层：`confirmation.rs` 与这里不重叠——
//! 令牌链路的**签发侧**（`prepare` 绝不给 HumanOnly 造令牌）在这里测，
//! 因为"人不在场时连令牌都拿不到"正是这一级存在的全部理由。

use serde_json::json;
use wxlike_cmd::auth::{
    context_from_parts, guard_channel, ActorContext, AuthError, AuthGate, Channel, Provenance,
};
use wxlike_cmd::catalog::{self, Capability, CommandSpec, Confirmation, Idempotency, ScopeClass};
use wxlike_cmd::confirmation::{ConfirmError, ConfirmationStore};

const NOW: i64 = 1_700_000_000;

/// 合成一条"转账"命令：`confirmation` 由每个用例自己填，其余固定。
fn transfer(confirmation: Confirmation) -> CommandSpec {
    CommandSpec {
        id: "wallet.transfer",
        summary: "给指定账户转一笔钱",
        params: r#"{"to": string, "amount": integer}"#,
        args: &[],
        capability: Capability::Write,
        needs_conv: false,
        scope: ScopeClass::Payment,
        confirmation,
        idempotency: Idempotency::None,
        platforms: catalog::Platforms::ALL,
        host: catalog::HostCapability::None,
    }
}

fn ctx() -> ActorContext {
    context_from_parts("u-alice", "s-1", "*")
}

fn gate(channel: Channel) -> AuthGate {
    AuthGate::from_parts("u-alice", "s-1", "*", Some("s-1"), Vec::new(), channel)
        .expect("gate from parts")
}

#[test]
fn agent_channel_is_denied_and_human_channel_is_allowed() {
    let spec = transfer(Confirmation::HumanOnly);
    let err = match guard_channel(&spec, Channel::Agent) {
        Err(e) => e,
        Ok(()) => panic!("无人值守通道必须被拒：闸门没咬"),
    };
    assert_eq!(err, AuthError::HumanOnlyAction("wallet.transfer"));
    // 错误消息要能指挥下一步：既说"别重试"，也说"没有令牌这条路"
    let text = err.to_string();
    assert!(text.contains("human-only"), "{text}");
    assert!(text.contains("attended"), "{text}");
    assert!(
        !text.contains("confirmation_token`"),
        "不该把调用方指回令牌流程：{text}"
    );
    assert!(
        guard_channel(&spec, Channel::Human).is_ok(),
        "人在场就该放行"
    );
}

/// 反向对照：只标 `Required` 的同一命令，两个通道都得过闸门这一关。
/// 这条是"等级判据"的守门人——如果哪天有人把 `guard_channel` 写成
/// "凡是带确认的一律拒 agent"，日常危险动作（踢人、撤回）会在 MCP 上
/// 全线失效，而 catalog 的测试照样绿。
#[test]
fn required_still_passes_both_channels() {
    let spec = transfer(Confirmation::Required);
    assert!(guard_channel(&spec, Channel::Agent).is_ok());
    assert!(guard_channel(&spec, Channel::Human).is_ok());
    // 只读更是两个通道都不拦
    let mut read = transfer(Confirmation::HumanOnly);
    read.confirmation = Confirmation::None;
    read.capability = Capability::Read;
    assert!(guard_channel(&read, Channel::Agent).is_ok());
}

/// 闸门不依赖是否配了 scope：挂上 `AuthGate` 也是同一个判断，
/// 而且**排在 scope 判定之前**（注定被拒的请求不必先算资源）。
#[test]
fn gate_denies_before_scope_math() {
    let spec = transfer(Confirmation::HumanOnly);
    let err = gate(Channel::Agent)
        .check(
            &spec,
            &json!({"to": "u-bob", "amount": 1}),
            Provenance::Trusted,
        )
        .unwrap_err();
    assert_eq!(err, AuthError::HumanOnlyAction("wallet.transfer"), "{err}");
    // 人在场时闸门放行（scope 那关是另一件事，不由这里管）
    assert!(gate(Channel::Human)
        .check(&spec, &json!({"to": "u-bob"}), Provenance::Trusted)
        .is_ok());
}

/// **令牌发不出来**：这一级的存在意义就是自动化通道拿不到令牌。
/// 只测 `guard_channel` 不够——`confirm_step` 那条路如果还能 `prepare`
/// 成功，闸门就等于装饰。
#[test]
fn no_token_is_ever_issued_for_human_only() {
    let spec = transfer(Confirmation::HumanOnly);
    let params = json!({"to": "u-bob", "amount": 1});
    let mut store = ConfirmationStore::new();
    let err = store.prepare(&ctx(), &spec, &params, NOW).unwrap_err();
    assert_eq!(err, ConfirmError::HumanOnly("wallet.transfer"));
    assert_eq!(store.outstanding(NOW), 0, "一次都没签发");
    // 兑换同理：拿谁都签发不出来的令牌去 redeem，报错也不许是"多此一举"
    let err = store
        .redeem(&ctx(), &spec, &params, "wxc-forged", NOW)
        .unwrap_err();
    assert_eq!(err, ConfirmError::HumanOnly("wallet.transfer"));
    // 对照：`Required` 走同一条链路必须仍能签发与兑换
    let required = transfer(Confirmation::Required);
    let prepared = store
        .prepare(&ctx(), &required, &params, NOW)
        .expect("Required 仍可签发");
    assert!(store
        .redeem(&ctx(), &required, &params, prepared.token.as_str(), NOW + 1)
        .is_ok());
}

/// agent 得**被告知**有这一步，而不是撞上一堵无名墙。
/// 与 wordcraft 的 `NEEDS_APP` 同一取向：拒绝要带着"去哪儿做"，
/// 并且留在 catalog 生成面上，让 `tools/list` 与覆盖表都能看见。
#[test]
fn human_only_is_announced_on_every_generated_surface() {
    let spec = transfer(Confirmation::HumanOnly);
    assert_eq!(spec.confirmation.as_str(), "human_only");
    assert_eq!(
        catalog::tool_definition(&spec)["_meta"]["wxlike/confirmation"],
        "human_only"
    );
    assert_eq!(
        serde_json::to_string(&spec.confirmation).unwrap(),
        "\"human_only\"",
        "序列化口径与 as_str() 一字不差"
    );
    // `needs_plan_step` 是 CLI 侧的判据：HumanOnly 也要先摊计划，
    // 只是回答它的人不能用令牌，而是自己敲 `--confirm`
    assert!(spec.confirmation.needs_plan_step());
    assert!(
        !spec.confirmation.is_required(),
        "is_required 仍是令牌那一档"
    );
}
