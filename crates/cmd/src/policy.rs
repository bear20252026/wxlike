//! **权限控制中心**：所有"这个 agent 能不能做这件事"的判断只在这里做一次。
//!
//! 借 WordCraft 的能力模型（`--automation-read-root` / `--automation-write-root`），
//! 但把"路径"换成"会话"：IM 的边界是**哪些会话**、**能不能写**、**能不能做破坏性操作**。
//!
//! 三条设计约束：
//! 1. **不写在 handler 里**——命令实现只负责做事，不做权限判断；
//! 2. **默认拒绝**——没有显式授予的会话/能力一律不行；
//! 3. **拒绝要可读**——错误消息里带具体会话 id 和"该开哪个开关"，
//!    agent 读到后能自己纠正（WordCraft 的 `automation path rejected: ...` 同款）。

use crate::{get, Capability, CommandSpec};
use std::fmt;

/// 拒绝原因。每条都带足够的信息让调用方自己修。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Deny {
    /// 命令 id 不存在（拼错或版本不匹配）。
    UnknownCommand(String),
    /// 会话不在白名单里。
    ConvNotAllowed { conv: String, reason: &'static str },
    /// 会话类命令没给 conv 参数。
    MissingConv(String),
    /// 只读模式：拒绝一切写。
    ReadOnly(String),
    /// 没开写权限。
    WriteDisabled(String),
    /// 没开破坏性权限。
    DestructiveDisabled(String),
}

impl fmt::Display for Deny {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Deny::UnknownCommand(id) => write!(f, "unknown command {id:?} (call im_list_commands)"),
            Deny::ConvNotAllowed { conv, reason } => write!(
                f,
                "conversation {conv} is not allowed: {reason} (grant it with --allow-conv)"
            ),
            Deny::MissingConv(id) => {
                write!(f, "{id} needs a `conv` param, but none was given")
            }
            Deny::ReadOnly(id) => write!(
                f,
                "{id} is a write command and this session is read-only (drop --read-only)"
            ),
            Deny::WriteDisabled(id) => write!(
                f,
                "{id} writes and write access was not granted (add --allow-write)"
            ),
            Deny::DestructiveDisabled(id) => write!(
                f,
                "{id} is destructive and destructive access was not granted (add --allow-destructive)"
            ),
        }
    }
}

impl std::error::Error for Deny {}

/// 一个 agent 会话被授予的能力边界。
#[derive(Clone, Debug, Default)]
pub struct Policy {
    /// 允许的会话，支持 `*` 通配（`d-*~alice` 表示"我和任何人的直聊"）。
    /// **空 = 一个都不许**，与 WordCraft 的"不给 root 就没有访问"一致。
    allow_conv: Vec<String>,
    /// 是否允许写命令。
    allow_write: bool,
    /// 是否允许破坏性命令（踢人、撤回他人消息）。默认 false。
    allow_destructive: bool,
}

impl Policy {
    /// 只读、零会话：最保守的起点。
    pub fn none() -> Self {
        Self::default()
    }

    /// 显式构造。`convs` 里的空串会被忽略（防止误传 `""` 当成"全部"）。
    pub fn new<I, S>(convs: I, allow_write: bool, allow_destructive: bool) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            allow_conv: convs
                .into_iter()
                .map(Into::into)
                .filter(|c| !c.trim().is_empty())
                .collect(),
            allow_write,
            allow_destructive,
        }
    }

    /// 从环境变量读（`WXLIKE_AGENT_ALLOW_CONV` 用逗号分隔）。
    ///
    /// 为什么支持环境变量：容器/systemd 里传 flag 麻烦，而 MCP 客户端配置里
    /// 写 `env` 更自然。两条路通向同一个 Policy。
    pub fn from_env() -> Self {
        let convs = std::env::var("WXLIKE_AGENT_ALLOW_CONV")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .collect::<Vec<_>>();
        Self::new(
            convs,
            env_flag("WXLIKE_AGENT_ALLOW_WRITE"),
            env_flag("WXLIKE_AGENT_ALLOW_DESTRUCTIVE"),
        )
    }

    pub fn allow_write(&self) -> bool {
        self.allow_write
    }

    pub fn allow_destructive(&self) -> bool {
        self.allow_destructive
    }

    pub fn conv_patterns(&self) -> &[String] {
        &self.allow_conv
    }

    /// 这个会话是否在白名单里。
    pub fn conv_allowed(&self, conv: &str) -> bool {
        self.allow_conv.iter().any(|p| glob_match(p, conv))
    }

    /// **唯一的授权入口**：查命令 → 查能力等级 → 查会话。
    ///
    /// 顺序是刻意的：先报"命令不存在"再报"没权限"，避免用错误消息探测命令表。
    pub fn authorize(
        &self,
        id: &str,
        params: &serde_json::Value,
    ) -> Result<&'static CommandSpec, Deny> {
        let spec = get(id).ok_or_else(|| Deny::UnknownCommand(id.to_string()))?;
        self.check_capability(spec)?;
        if spec.needs_conv {
            let convs = convs_in(params);
            if convs.is_empty() {
                return Err(Deny::MissingConv(id.to_string()));
            }
            for conv in convs {
                if !self.conv_allowed(conv) {
                    return Err(Deny::ConvNotAllowed {
                        conv: conv.to_string(),
                        reason: "outside the allowlist",
                    });
                }
            }
        }
        Ok(spec)
    }

    fn check_capability(&self, spec: &CommandSpec) -> Result<(), Deny> {
        match spec.capability {
            Capability::Read => Ok(()),
            Capability::Write => {
                if !self.allow_write {
                    return Err(Deny::WriteDisabled(spec.id.to_string()));
                }
                Ok(())
            }
            Capability::Destructive => {
                if !self.allow_destructive {
                    return Err(Deny::DestructiveDisabled(spec.id.to_string()));
                }
                // 破坏性命令必然也是写；只开破坏性不开写是配置错误，
                // 按"写也没开"处理，避免出现"能踢人但不能发消息"的怪状态。
                if !self.allow_write {
                    return Err(Deny::ReadOnly(spec.id.to_string()));
                }
                Ok(())
            }
        }
    }

    /// 列会话时用：把结果过滤到白名单内（`conv.list` 不该泄露白名单外的会话）。
    pub fn retain_allowed(&self, convs: &mut Vec<String>) {
        convs.retain(|c| self.conv_allowed(c));
    }
}

fn env_flag(key: &str) -> bool {
    matches!(
        std::env::var(key).unwrap_or_default().trim(),
        "1" | "true" | "TRUE" | "yes" | "on"
    )
}

/// 从参数里取所有需要校验的会话 id。
///
/// `msg.forward` 同时涉及两个会话（来源与目标），两个都要在名单里——
/// 否则 agent 可以把一个白名单会话的内容转到名单外。
fn convs_in(params: &serde_json::Value) -> Vec<&str> {
    let mut out = Vec::new();
    for key in ["conv", "to_conv", "from_conv"] {
        if let Some(v) = params.get(key).and_then(|v| v.as_str()) {
            if !v.is_empty() {
                out.push(v);
            }
        }
    }
    out
}

/// 只支持 `*` 的简单通配：够用，且可预测（不做正则，避免灾难性回溯）。
///
/// `*` 匹配任意字符序列；其余字符按字面比较，大小写敏感（会话 id 区分大小写）。
fn glob_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == text;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = text;
    // 首段必须在前缀处
    if let Some(first) = parts.first() {
        if !first.is_empty() {
            match rest.strip_prefix(first) {
                Some(r) => rest = r,
                None => return false,
            }
        }
    }
    // 末段必须在后缀处
    let last = parts.last().copied().unwrap_or("");
    if !last.is_empty() && !text.ends_with(last) {
        return false;
    }
    // 中间段按顺序找
    for part in parts.iter().skip(1).take(parts.len().saturating_sub(2)) {
        if part.is_empty() {
            continue;
        }
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

/// 把当前策略渲染成给人看的表格。
///
/// **两个二进制共用这一份**（`wxlike-cli policy` 与 `wxlike-cmd policy`）：
/// 以前各写一遍，文案已经漂移（一处内联 `if/else`，一处用 `yes_no()`）。
/// 返回 `String` 而不是直接 `println!`，是为了能写快照测试。
pub fn render(p: &Policy) -> String {
    let mut out = String::new();
    out.push_str("权限控制中心 · 当前生效策略\n");
    let convs = p.conv_patterns();
    out.push_str(&format!(
        "  会话白名单   {}\n",
        if convs.is_empty() {
            "（空 = 任何会话都不允许）".to_string()
        } else {
            convs.join(", ")
        }
    ));
    out.push_str(&format!("  写权限       {}\n", yes_no(p.allow_write())));
    out.push_str(&format!(
        "  破坏性权限   {}\n",
        yes_no(p.allow_destructive())
    ));
    out.push('\n');
    out.push_str(&format!("{:<20} {:<12} 判定\n", "ID", "能力"));

    let mut allowed = 0usize;
    for spec in crate::CATALOG {
        // 用一个占位会话试权限：只关心"这条命令在当前策略下能不能过"。
        // 三个键都给：`msg.forward` 要同时校验 from/to 两个会话。
        let probe = serde_json::json!({
            "conv": probe_conv(p),
            "to_conv": probe_conv(p),
            "from_conv": probe_conv(p),
        });
        match p.authorize(spec.id, &probe) {
            Ok(_) => {
                allowed += 1;
                out.push_str(&format!("{:<20} {:<12} 允许\n", spec.id, spec.capability));
            }
            Err(err) => out.push_str(&format!(
                "{:<20} {:<12} 拒绝 — {err}\n",
                spec.id, spec.capability
            )),
        }
    }
    out.push_str(&format!(
        "\n可执行 {allowed} / {} 条\n",
        crate::CATALOG.len()
    ));
    out
}

/// 拿白名单里的第一个模式当探针；没有白名单就随便给一个必然被拒的 id。
fn probe_conv(p: &Policy) -> String {
    match p.conv_patterns().first() {
        Some(pat) if !pat.contains('*') => pat.clone(),
        Some(pat) => pat.replace('*', "probe"),
        None => "probe".to_string(),
    }
}

fn yes_no(v: bool) -> &'static str {
    if v {
        "开"
    } else {
        "关"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_policy_denies_everything_conversation_scoped() {
        let p = Policy::none();
        let err = p
            .authorize("conv.unread", &json!({"conv": "d-a~b"}))
            .unwrap_err();
        assert!(matches!(err, Deny::ConvNotAllowed { .. }), "{err:?}");
        assert!(err.to_string().contains("d-a~b"), "错误里要带会话 id");
        assert!(err.to_string().contains("--allow-conv"), "要告诉人怎么修");
    }

    #[test]
    fn non_conversation_commands_work_in_read_only_mode() {
        let p = Policy::none();
        assert!(p.authorize("me.info", &json!({})).is_ok());
        assert!(p.authorize("conv.list", &json!({})).is_ok());
    }

    #[test]
    fn write_needs_the_write_flag() {
        let p = Policy::new(["g-1"], false, false);
        let err = p
            .authorize("msg.send", &json!({"conv": "g-1", "text": "hi"}))
            .unwrap_err();
        assert!(matches!(err, Deny::WriteDisabled(_)), "{err:?}");
        assert!(err.to_string().contains("--allow-write"));

        let p = Policy::new(["g-1"], true, false);
        assert!(p
            .authorize("msg.send", &json!({"conv": "g-1", "text": "hi"}))
            .is_ok());
    }

    #[test]
    fn destructive_needs_its_own_flag() {
        let p = Policy::new(["g-1"], true, false);
        let err = p
            .authorize("group.kick", &json!({"conv": "g-1", "user_id": "u2"}))
            .unwrap_err();
        assert!(matches!(err, Deny::DestructiveDisabled(_)), "{err:?}");
        assert!(err.to_string().contains("--allow-destructive"));

        let p = Policy::new(["g-1"], true, true);
        assert!(p
            .authorize("group.kick", &json!({"conv": "g-1", "user_id": "u2"}))
            .is_ok());
    }

    /// 只开破坏性、不开写：这是配置错误，不该变成"能踢人不能发言"。
    #[test]
    fn destructive_without_write_is_refused() {
        let p = Policy::new(["g-1"], false, true);
        let err = p
            .authorize("group.kick", &json!({"conv": "g-1", "user_id": "u2"}))
            .unwrap_err();
        assert!(matches!(err, Deny::ReadOnly(_)), "{err:?}");
    }

    #[test]
    fn forward_checks_both_conversations() {
        let p = Policy::new(["g-in"], true, false);
        let err = p
            .authorize(
                "msg.forward",
                &json!({"from_conv": "g-in", "to_conv": "g-out", "msg_id": "m"}),
            )
            .unwrap_err();
        assert!(matches!(err, Deny::ConvNotAllowed { .. }), "{err:?}");
        assert!(err.to_string().contains("g-out"));

        // 两个都在名单里才放行
        let p = Policy::new(["g-in", "g-out"], true, false);
        assert!(p
            .authorize(
                "msg.forward",
                &json!({"from_conv": "g-in", "to_conv": "g-out", "msg_id": "m"})
            )
            .is_ok());
    }

    #[test]
    fn wildcards_are_predictable() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("d-*~alice", "d-bob~alice"));
        assert!(!glob_match("d-*~alice", "d-bob~carol"));
        assert!(glob_match("g-*", "g-abc"));
        assert!(!glob_match("g-*", "d-abc"));
        assert!(glob_match("g-a*c", "g-abc"));
        // 通配不是正则：点号是字面量
        assert!(!glob_match("g.a", "gxa"));
    }

    #[test]
    fn missing_conv_param_is_reported_as_such() {
        let p = Policy::new(["*"], true, true);
        let err = p.authorize("conv.unread", &json!({})).unwrap_err();
        assert!(matches!(err, Deny::MissingConv(_)), "{err:?}");
    }

    #[test]
    fn unknown_command_is_reported_before_permissions() {
        let p = Policy::none();
        let err = p.authorize("msg.send_for_ai", &json!({})).unwrap_err();
        assert!(matches!(err, Deny::UnknownCommand(_)), "{err:?}");
    }

    /// 渲染出来的表必须**每条命令一行**，且两个二进制看到同一份文案。
    /// 这条测试挡的是"改了一处、另一处还是旧文案"。
    #[test]
    fn rendered_policy_covers_every_command_once() {
        let p = Policy::new(["g-1"], true, false);
        let text = render(&p);
        assert!(text.contains("权限控制中心"), "{text}");
        assert!(text.contains("g-1"), "白名单要显示出来：{text}");

        let rows = text
            .lines()
            .filter(|l| l.contains(" 允许") || l.contains(" 拒绝 — "))
            .count();
        assert_eq!(rows, crate::CATALOG.len(), "每条命令一行：{text}");

        // 同一份策略渲染两次必须逐字节相同（不能有隐藏的随机/时间因素）
        assert_eq!(text, render(&p));
    }

    /// 空策略下：不挂会话的读命令可用，写命令一律拒绝。
    #[test]
    fn rendered_policy_reflects_the_empty_policy() {
        let text = render(&Policy::none());
        assert!(text.contains("（空 = 任何会话都不允许）"), "{text}");
        assert!(text.contains("  写权限       关"), "{text}");
        assert!(text.contains("msg.send"), "{text}");
        assert!(
            text.lines()
                .any(|l| l.starts_with("msg.send") && l.contains("拒绝")),
            "空策略下写命令必须被拒：{text}"
        );
    }

    #[test]
    fn listing_filters_out_conversations_outside_the_allowlist() {
        let p = Policy::new(["g-1"], true, false);
        let mut convs = vec!["g-1".to_string(), "g-2".to_string()];
        p.retain_allowed(&mut convs);
        assert_eq!(convs, vec!["g-1".to_string()]);
    }
}
