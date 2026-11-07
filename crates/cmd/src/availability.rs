//! 可用性判定：**这条命令此刻能不能做，以及为什么不能**。
//!
//! 这里是"一份判定、三方读"的那一份。对照 wordcraft：它的
//! `CommandSpec.enabled: fn(&Session) -> Option<reason>` 同时喂给界面灰化
//! 与执行时报错（`crates/engine/src/session/run.rs`），两侧不可能各长一套；
//! 但它 headless 的 `list_commands` **不输出**这个布尔，模型只能试出来——
//! 我们不要连这条一起抄。
//!
//! 所以这一层的规矩只有一条，而且是可测的：
//! **`why_not` 不许自己判断任何事**。它按执行路径同样的顺序调用同样的四道
//! 原语（[`crate::auth::guard_channel`] → [`crate::auth::guard_provenance`] →
//! [`AuthGate::check_scope`] → [`crate::policy::Policy::authorize`]），把它们的
//! 错误翻译成 `{code, message}`。执行侧改了判定，这里自动跟着变；
//! 反过来如果有人在这里加了一条执行侧没有的规则，
//! `crates/cmd/tests/availability_no_drift.rs` 会当场红。
//!
//! 三个读它的地方：
//!
//! * `wxlike-mcp` 的 `im_list_commands`：让 agent 在动手之前就知道哪些做不了、
//!   差哪个开关（`available` + `why_not`）；
//! * `wxlike policy`（`AuthGate::render`）：人看的那张自查表；
//! * 客户端界面：置灰依据从这里取，别在 React/Flutter 里再写一遍规则。
//!
//! ## 参数用探针，所以这是"形状上能不能做"
//! 列表场景没有真参数。[`auth::probe_params`] 按 catalog 的声明造一组
//! 类型正确、语义无意义的值，因此答案覆盖的是**通道、来源、scope、策略**
//! 这类与参数内容无关的拒绝；而 `conv_not_allowed` 这类依赖具体会话的，
//! 探针必然落在白名单外，报出来是"白名单存在时这条命令要显式放行"，
//! 不是"这个会话能做"。真正执行时仍会用真参数再走一遍同样的判定。

use serde_json::Value;

use crate::auth::{self, AuthError, AuthGate, Channel, Provenance};
use crate::catalog::CommandSpec;
use crate::policy::Policy;

/// 一次拒绝：机器读的码 + 人读的一句话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Denial {
    /// 稳定码（见 [`AuthError::code`] / [`Deny::code`](crate::policy::Deny::code)）。
    pub code: &'static str,
    /// 给人和模型看的下一步指引——执行路径的原文，不另写一套措辞。
    pub message: String,
}

/// 判定所需的现场：通道、参数来源、身份闸门（可缺省）、本地策略。
pub struct Context<'a> {
    /// 由入口进程声明，模型改不了（见 [`Channel`]）。
    pub channel: Channel,
    /// 参数是谁给的：人敲的算可信，聊天正文里抠出来的算不可信。
    pub provenance: Provenance,
    /// `None` = 没配 `--scope`（本地 stdio 单用户），此时跳过身份那一段，
    /// 与执行路径一致。
    pub gate: Option<&'a AuthGate>,
    pub policy: &'a Policy,
}

/// 能不能做。`None` = 可以做；`Some` = 不能，附带码与原因。
pub fn why_not(spec: &CommandSpec, params: &Value, ctx: &Context<'_>) -> Option<Denial> {
    let mut denial = None;
    // 顺序与执行路径逐字一致：通道 → 来源 → scope → 策略。
    // 先短后长：任一步出错就记下并停下，后面的步骤不该把原因盖掉。
    if let Err(e) = auth::guard_channel(spec, ctx.channel) {
        denial = Some(denial_of(&e));
    } else if let Err(e) = auth::guard_provenance(spec, ctx.provenance) {
        denial = Some(denial_of(&e));
    } else if let Some(gate) = ctx.gate {
        if let Err(e) = gate.check_scope(spec, params) {
            denial = Some(denial_of(&e));
        }
    }
    if denial.is_none() {
        if let Err(e) = ctx.policy.authorize(spec.id, params) {
            denial = Some(Denial {
                code: e.code(),
                message: e.to_string(),
            });
        }
    }
    denial
}

fn denial_of(e: &AuthError) -> Denial {
    Denial {
        code: e.code(),
        message: e.to_string(),
    }
}

/// 列表场景的参数：类型对、语义空的探针（复用 [`auth::probe_params`]，
/// 与 `wxlike policy` 那张表用的是同一组值，两张表不会给出不同的答案）。
#[must_use]
pub fn probe_params(spec: &CommandSpec) -> Value {
    auth::probe_params(spec)
}
