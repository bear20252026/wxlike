//! IM 的命令目录（catalog）：**只定义，不执行**。
//!
//! 结构借自 WordCraft（`storytold/wordcraft`）：一条命令一个 id、一处定义、
//! 多个入口（UI / CLI / MCP / HTTP）共用。差异在于我们把"定义"和"执行"拆开——
//! IM 有两个执行面（本地客户端、远端 HTTP），catalog 不该知道任何一边。
//!
//! 三件事在这里定死，别处不许再判断：
//! 1. **命令有哪些**（`catalog()`）——新增功能先加一条 spec；
//! 2. **每条命令多危险**（[`Capability`]）——读 / 写 / 破坏性；
//! 3. **谁能做什么**（[`policy::Policy`]）——由启动参数注入，不写在 handler 里。
//!
//! 命令 id 用 `域.动作`（`msg.send`、`group.kick`），与 WordCraft 的
//! `text.insert` / `table.resize` 同一套命名直觉。

pub mod policy;

use std::fmt;

/// 命令的危险等级。默认只给 `Read`，写要显式开，破坏性再开一层。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// 只读：看会话、看历史、看成员、看自己的状态。
    Read,
    /// 写入：发消息、标已读、转发、建群、拉人。
    Write,
    /// 破坏性：踢人、撤回他人消息、转让群主。默认拒绝。
    Destructive,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Read => "read",
            Capability::Write => "write",
            Capability::Destructive => "destructive",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一条命令的定义。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    /// `域.动作`，全局唯一，**改名等于破坏兼容**。
    pub id: &'static str,
    /// 一句话说明（给人和 AI 看，进 `list_commands`）。
    pub summary: &'static str,
    /// 参数说明。刻意用自然语言而不是 JSON Schema：18 条命令手写 schema 的
    /// 维护成本高于收益，而 AI 读一段话就够。
    pub params: &'static str,
    pub capability: Capability,
    /// 是否必须带 `conv` 参数（决定权限检查时要不要比对会话白名单）。
    pub needs_conv: bool,
}

/// 现有 20 条（首批 18 + Zone C 的社区/频道 2 条）。**加功能先加这里**，再去实现执行器。
///
/// 顺序按"人怎么用"排，不按字母：先看（会话/消息/成员/自己），再写，最后危险操作。
pub const CATALOG: &[CommandSpec] = &[
    // ---- 读：会话 ----
    CommandSpec {
        id: "conv.list",
        summary: "列出我参与的会话（含未读数）",
        params: "无",
        capability: Capability::Read,
        needs_conv: false,
    },
    CommandSpec {
        id: "conv.unread",
        summary: "某个会话的未读快照：head_seq / read_seq / unread",
        params: "{conv: string}",
        capability: Capability::Read,
        needs_conv: true,
    },
    CommandSpec {
        id: "conv.history",
        summary: "翻历史消息，按会话内序列号倒序分页",
        params: "{conv: string, before_seq?: number, limit?: number}",
        capability: Capability::Read,
        needs_conv: true,
    },
    CommandSpec {
        id: "conv.sync",
        summary: "五层差分指令：客户端该全量拉还是增量拉",
        params: "{conv: string, version: number}",
        capability: Capability::Read,
        needs_conv: true,
    },
    // ---- 读：消息 ----
    CommandSpec {
        id: "msg.list",
        summary: "按 seq 区间取消息（不翻页，给脚本用）",
        params: "{conv: string, from_seq: number, to_seq?: number}",
        capability: Capability::Read,
        needs_conv: true,
    },
    CommandSpec {
        id: "msg.search",
        summary: "在可见会话内全文搜索",
        params: "{q: string, limit?: number}",
        capability: Capability::Read,
        needs_conv: false,
    },
    CommandSpec {
        id: "msg.get",
        summary: "取单条消息（含撤回状态）",
        params: "{conv: string, msg_id: string}",
        capability: Capability::Read,
        needs_conv: true,
    },
    // ---- 读：成员与自查 ----
    CommandSpec {
        id: "group.members",
        summary: "群成员列表（分页，万人群不全量拉）",
        params: "{conv: string, after_user?: string, limit?: number}",
        capability: Capability::Read,
        needs_conv: true,
    },
    CommandSpec {
        id: "group.info",
        summary: "群基本信息与我的角色",
        params: "{conv: string}",
        capability: Capability::Read,
        needs_conv: true,
    },
    CommandSpec {
        id: "community.threads",
        summary: "社区里正在讨论的话题（跨频道汇总，带回复数与我的订阅状态）",
        params: "{community: string, limit?: number}",
        capability: Capability::Read,
        needs_conv: false,
    },
    CommandSpec {
        id: "channel.roles",
        summary: "频道角色名单：谁在这个频道里是 owner/moderator/member/guest，含继承标记",
        params: "{conv: string}",
        capability: Capability::Read,
        // conv 在这里是频道 id：会话白名单同样管得住它
        needs_conv: true,
    },
    CommandSpec {
        id: "me.info",
        summary: "我是谁：user_id、昵称、在线设备",
        params: "无",
        capability: Capability::Read,
        needs_conv: false,
    },
    CommandSpec {
        id: "me.sessions",
        summary: "我的登录会话（设备/时间），用于排查异地登录",
        params: "无",
        capability: Capability::Read,
        needs_conv: false,
    },
    // ---- 写 ----
    CommandSpec {
        id: "msg.send",
        summary: "发一条文本消息（必须带 idempotency_key，重试不会重复发）",
        params: "{conv: string, text: string, idempotency_key: string, reply_to?: string}",
        capability: Capability::Write,
        needs_conv: true,
    },
    CommandSpec {
        id: "conv.ack",
        summary: "把已读游标推到指定 seq（缺省=最新）",
        params: "{conv: string, seq?: number}",
        capability: Capability::Write,
        needs_conv: true,
    },
    CommandSpec {
        id: "msg.forward",
        summary: "转发一条消息到另一个会话",
        params: "{from_conv: string, to_conv: string, msg_id: string, idempotency_key: string}",
        capability: Capability::Write,
        needs_conv: true,
    },
    CommandSpec {
        id: "group.create",
        summary: "建群并拉入初始成员",
        params: "{name: string, member_ids: [string], idempotency_key: string}",
        capability: Capability::Write,
        needs_conv: false,
    },
    CommandSpec {
        id: "group.add_member",
        summary: "拉人进群",
        params: "{conv: string, user_ids: [string], idempotency_key: string}",
        capability: Capability::Write,
        needs_conv: true,
    },
    // ---- 破坏性 ----
    CommandSpec {
        id: "group.kick",
        summary: "把成员移出群（不可撤销）",
        params: "{conv: string, user_id: string}",
        capability: Capability::Destructive,
        needs_conv: true,
    },
    CommandSpec {
        id: "msg.recall",
        summary: "撤回一条消息（他人消息需管理员）",
        params: "{conv: string, msg_id: string}",
        capability: Capability::Destructive,
        needs_conv: true,
    },
];

/// 按 id 查命令。
pub fn get(id: &str) -> Option<&'static CommandSpec> {
    CATALOG.iter().find(|s| s.id == id)
}

/// 过滤清单（大小写不敏感，匹配 id 或说明）。
pub fn search(query: &str) -> Vec<&'static CommandSpec> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return CATALOG.iter().collect();
    }
    CATALOG
        .iter()
        .filter(|s| s.id.to_lowercase().contains(&q) || s.summary.to_lowercase().contains(&q))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_is_well_formed() {
        assert_eq!(CATALOG.len(), 20, "命令数变了：快照测试要一起加 id");
        let mut ids = HashSet::new();
        for spec in CATALOG {
            assert!(ids.insert(spec.id), "重复的命令 id: {}", spec.id);
            assert!(spec.id.contains('.'), "id 必须是 域.动作: {}", spec.id);
            assert!(!spec.summary.is_empty(), "缺说明: {}", spec.id);
            assert!(!spec.params.is_empty(), "缺参数说明: {}", spec.id);
        }
    }

    /// id 是外部契约（MCP 客户端会存下来），改名等于破坏兼容。
    /// 这条快照测试就是拦"悄悄改名"的。
    #[test]
    fn command_ids_are_stable() {
        let ids: Vec<&str> = CATALOG.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec![
                "conv.list",
                "conv.unread",
                "conv.history",
                "conv.sync",
                "msg.list",
                "msg.search",
                "msg.get",
                "group.members",
                "group.info",
                "community.threads",
                "channel.roles",
                "me.info",
                "me.sessions",
                "msg.send",
                "conv.ack",
                "msg.forward",
                "group.create",
                "group.add_member",
                "group.kick",
                "msg.recall",
            ]
        );
    }

    #[test]
    fn search_matches_id_and_summary() {
        assert_eq!(search("unread").len(), 1);
        assert!(search("群").len() >= 3, "中文说明也要能搜到");
        assert_eq!(search("").len(), CATALOG.len());
        assert!(search("不存在的命令").is_empty());
    }

    #[test]
    fn get_returns_none_for_unknown() {
        assert!(get("msg.send").is_some());
        assert!(get("msg.send_for_ai").is_none(), "不做 AI 专用接口");
    }
}
