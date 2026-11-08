//! **一份判定，不许漂移**：`wxlike_cmd::availability::why_not` 必须与执行路径
//! 的判定逐条同码同文。
//!
//! 这条测试是"界面置灰、`list_commands`、真调用读同一份"这个说法的**唯一证据**。
//! 说法本身不难成立——难的是以后：谁在 `tools_call` 里补一个 `if`，或者在
//! React/Flutter 里写一条"这条按钮先灰掉"，两份真相就重新长出来了，而两边的
//! 测试各自都还是绿的。所以这里做的是**矩阵**而不是抽样：
//! catalog 全量 × 两个通道 × 两种参数来源 × 三种身份闸门 × 三种策略，
//! 每格都要求"列表说的"与"执行拦的"完全一致。
//!
//! 参数有两组：[`probe_params`]（列表用的探针）与一组真实形状的固定参数
//! （带 `conv: "g-1"`，专门让会话白名单这类**与参数内容有关**的判定咬到）。
//! 只用探针会漏掉这一类——那是"绿给你看"的另一种形态。

use serde_json::{json, Value};
use wxlike_cmd::auth::{AuthError, AuthGate, Channel, Provenance};
use wxlike_cmd::availability::{self, probe_params};
use wxlike_cmd::catalog::{self, CommandSpec};
use wxlike_cmd::policy::{Deny, Policy};

/// 执行路径的拒绝：三种来源合成一个，方便与 `Denial` 对齐比较。
enum Exec {
    Auth(AuthError),
    Deny(Deny),
}

impl Exec {
    fn code(&self) -> &'static str {
        match self {
            Exec::Auth(e) => e.code(),
            Exec::Deny(e) => e.code(),
        }
    }

    fn message(&self) -> String {
        match self {
            Exec::Auth(e) => e.to_string(),
            Exec::Deny(e) => e.to_string(),
        }
    }
}

/// 判定现场。写成函数而不是闭包：`Context<'a>` 的两个借用字段必须共享
/// 同一个生命周期，闭包推不出来。
fn ctx_with<'a>(
    channel: Channel,
    provenance: Provenance,
    gate: &'a AuthGate,
    policy: &'a Policy,
) -> availability::Context<'a> {
    availability::Context {
        channel,
        provenance,
        gate: Some(gate),
        policy,
    }
}

fn gate(channel: Channel, scope: &str) -> AuthGate {
    AuthGate::from_parts("u-alice", "s-1", scope, Some("s-1"), Vec::new(), channel)
        .unwrap_or_else(|e| panic!("scope {scope:?} 应当可解析：{e}"))
}

/// 执行路径原样：通道 → 来源 → scope（闸门在才查）→ 本地策略。
/// 这三步在收敛前是散在 `tools_call` 与 `Executor::run` 里的，这里照抄那个次序。
fn executed(
    spec: &CommandSpec,
    params: &Value,
    channel: Channel,
    provenance: Provenance,
    gate: Option<&AuthGate>,
    policy: &Policy,
) -> Result<(), Exec> {
    wxlike_cmd::auth::guard_channel(spec, channel).map_err(Exec::Auth)?;
    wxlike_cmd::auth::guard_provenance(spec, provenance).map_err(Exec::Auth)?;
    if let Some(g) = gate {
        g.check_scope(spec, params).map_err(Exec::Auth)?;
    }
    policy
        .authorize(spec.id, params)
        .map(|_| ())
        .map_err(Exec::Deny)
}

/// 一格：列表答案与执行答案必须同码同文。
fn one(
    spec: &CommandSpec,
    params: &Value,
    channel: Channel,
    provenance: Provenance,
    gate: Option<&AuthGate>,
    policy: &Policy,
) {
    let ctx = availability::Context {
        channel,
        provenance,
        gate,
        policy,
    };
    let why = availability::why_not(spec, params, &ctx);
    let exec = executed(spec, params, channel, provenance, gate, policy);
    let where_ = format!(
        "{} channel={} provenance={} scope={} policy=({},{})",
        spec.id,
        channel,
        if provenance == Provenance::Trusted {
            "trusted"
        } else {
            "untrusted"
        },
        gate.map(|g| if g.channel() == Channel::Agent {
            "agent-gate"
        } else {
            "human-gate"
        })
        .unwrap_or("none"),
        policy.allow_write(),
        policy.allow_destructive()
    );
    match (why, exec) {
        (None, Ok(())) => {}
        (Some(d), Err(e)) => {
            assert_eq!(d.code, e.code(), "{where_}：拒绝码漂移");
            assert_eq!(d.message, e.message(), "{where_}：拒绝文案漂移");
        }
        (Some(d), Ok(())) => panic!("{where_}：列表说不能，执行却放行 — {}", d.message),
        (None, Err(e)) => panic!("{where_}：执行拦住了，列表却没报 — {}", e.message()),
    }
}

#[test]
fn listing_and_execution_never_disagree() {
    let broad = gate(Channel::Agent, "*");
    let narrow = gate(Channel::Agent, "conv:g-1");
    let human = gate(Channel::Human, "*");
    let gates = [Some(&broad), Some(&narrow), Some(&human), None];
    let policies = [
        Policy::none(),
        Policy::new(Vec::<String>::new(), true, false),
        Policy::new(["g-1"], true, true),
    ];
    let mut cells = 0usize;
    for spec in catalog::CATALOG {
        for gate in gates {
            for policy in &policies {
                for channel in [Channel::Agent, Channel::Human] {
                    for provenance in [Provenance::Trusted, Provenance::Untrusted("message body")] {
                        for params in [
                            probe_params(spec),
                            json!({"conv": "g-1", "text": "hi", "user_id": "u-2"}),
                        ] {
                            one(spec, &params, channel, provenance, gate, policy);
                            cells += 1;
                        }
                    }
                }
            }
        }
    }
    // 矩阵规模本身也要钉住：哪天有人把它缩成抽样，这条会先响
    assert_eq!(cells, 50 * 4 * 3 * 2 * 2 * 2, "用例格数变了");
}

/// 上面那条只保证"两边一致"，不保证"有东西被拦住"。
/// 全绿的一致有可能是两边都放行——那这层就是装饰，所以这一条专测咬合：
/// 同一个 `(spec, scope, policy)`，换通道或换策略必须给出**不同**的答案。
#[test]
fn the_predicate_actually_changes_its_answer() {
    let untrusted = Provenance::Untrusted("message body");
    // 空 `allow_conv` 是"一个会话都不许"（不是"全部"），所以这里点名 g-1
    let write_only = Policy::new(["g-1"], true, false);
    let broad = gate(Channel::Agent, "*");
    let writes: Vec<&CommandSpec> = catalog::CATALOG
        .iter()
        .filter(|s| s.capability != catalog::Capability::Read)
        .collect();
    assert!(
        !writes.is_empty(),
        "catalog 里没有写命令？那这条测试无从验证"
    );

    // 1) 不可信来源：写命令在可信时可用、在不可信时被拦
    let msg_send = catalog::get("msg.send").expect("msg.send");
    let params = json!({"conv": "g-1", "text": "hi", "idempotency_key": "k"});
    assert!(
        availability::why_not(
            msg_send,
            &params,
            &ctx_with(
                Channel::Agent,
                Provenance::Trusted,
                &broad,
                &Policy::new(["g-1"], true, false),
            )
        )
        .is_none(),
        "开写权限 + 可信来源，msg.send 就该可用"
    );
    let denied = availability::why_not(
        msg_send,
        &params,
        &ctx_with(Channel::Agent, untrusted, &broad, &write_only),
    )
    .expect("正文里抠出来的指令必须被拦");
    assert_eq!(denied.code, "untrusted_content", "{denied:?}");

    // 2) 写权限开关：同一条命令，两个答案
    let without = availability::why_not(
        msg_send,
        &params,
        &ctx_with(Channel::Agent, Provenance::Trusted, &broad, &Policy::none()),
    )
    .expect("Policy::none() 不开写权限，必须拦");
    assert_eq!(without.code, "write_disabled", "{without:?}");

    // 3) 会话白名单：探针参数落在名单外，必须比"全开"多拦一条
    let allowed = Policy::new(["g-1"], true, true);
    assert!(
        availability::why_not(
            msg_send,
            &json!({"conv": "g-1"}),
            &ctx_with(Channel::Agent, Provenance::Trusted, &broad, &allowed)
        )
        .is_none(),
        "名单内的会话应当可用"
    );
    let outside = availability::why_not(
        msg_send,
        &json!({"conv": "g-9"}),
        &ctx_with(Channel::Agent, Provenance::Trusted, &broad, &allowed),
    )
    .expect("名单外的会话必须拦");
    assert_eq!(outside.code, "conv_not_allowed", "{outside:?}");

    // 4) 破坏性：只开写不开破坏，踢人必须被拦且说清差哪个开关
    let kick = catalog::get("group.kick").expect("group.kick");
    let denial = availability::why_not(
        kick,
        &json!({"conv": "g-1", "user_id": "u-2"}),
        &ctx_with(Channel::Human, Provenance::Trusted, &broad, &write_only),
    )
    .expect("破坏性没开");
    assert_eq!(denial.code, "destructive_disabled", "{denial:?}");
    assert!(
        denial.message.contains("--allow-destructive"),
        "要说清怎么放开：{}",
        denial.message
    );
}

/// `wxlike policy` 那张人看的表也必须用同一个判定——它以前自己拼
/// `check + authorize`，是第三份真相。这里不重跑矩阵，只钉它引用的函数
/// 与列表用的是同一个答案（探针参数、同样三档）。
#[test]
fn the_human_policy_table_answers_the_same_way() {
    let broad = gate(Channel::Human, "*");
    let policy = Policy::none();
    let table = broad.render(&policy);
    let ctx = availability::Context {
        channel: Channel::Human,
        provenance: Provenance::Trusted,
        gate: Some(&broad),
        policy: &policy,
    };
    let mut expected_rows = 0;
    for spec in catalog::CATALOG {
        let params = probe_params(spec);
        let row = table
            .lines()
            .find(|l| l.starts_with(spec.id))
            .unwrap_or_else(|| panic!("自查表里没有 {}", spec.id));
        match availability::why_not(spec, &params, &ctx) {
            None => assert!(row.contains("允许"), "{row}"),
            Some(d) => {
                assert!(
                    row.contains(&format!("{} — {}", d.code, d.message)),
                    "表里这句和判定不一样：{row} ≠ {:?}",
                    d.message
                );
                expected_rows += 1;
            }
        }
    }
    assert!(
        expected_rows > 0,
        "只读策略下应当至少有一条被拦，否则这张表测不出东西"
    );
}
