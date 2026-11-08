//! 动作目录（catalog）：**唯一一份**"有哪些动作、多危险、要什么参数"。
//!
//! 一条命令在这里定义一次，四个入口共用：
//! CLI（`wxlike <domain> <verb>`）、MCP（`tools/list` + `tools/call`）、
//! 权限判定（[`crate::policy`]）、以及审计（`X-Wxlike-Command`）。
//!
//! 三个"不"：
//! * **不执行**——执行在 `wxlike-client` 的 `Executor`，这里只描述；
//! * **不手抄 schema**——JSON Schema 与参数校验都从 [`Param`] 生成；
//! * **不静默宽容**——未知参数、类型不符一律拒绝（见 [`validate_args`]）。

pub mod spec;

use serde_json::{json, Map, Value};

pub use spec::{
    Capability, CommandSpec, Confirmation, HostCapability, Idempotency, Param, ParamType,
    Platforms, ScopeClass,
};

/// 资源 URI 前缀。MCP `resources/read` 与审计日志引用同一个 URI。
pub const ACTION_URI_PREFIX: &str = "wxlike://actions/";

/// 现有 50 条：首批 18 条，加 Zone C 的社区/频道 2 条、Zone E 的视频/点播 6 条，
/// 再加 #316 relations 第一批 6 条、#317 第二批 7 条、#318 群管理 3 条
/// 与 #319 媒体对象 7 条，再加后续补录。逐条清单与顺序以
/// `crates/cmd/tests/contract_v1_baseline.rs` 的 `BASELINE_IDS` 为准；上面这段
/// 分批数字是历史沿革，不随每次纳入重算。
/// **加功能先加这里**，再去实现执行器——`crates/client` 的防漂移测试会盯着。
///
/// 顺序按"人怎么用"排，不按字母：先看（会话/消息/成员/自己/好友），再写，最后危险操作。
///
/// ## contract-v1 已冻结（#314；#316 第一次扩基线，#317 第二次，#318 第三次，#319 第四次）
/// 这 50 个 id 与 `crates/cmd/data/domains.json`（+ `data/entries/<域>.json`）
/// 的 81 条入口（automated 50 / blocked 26 / host_only 5）是当前基线，
/// 机器可读的形式在 `crates/cmd/tests/contract_v1_baseline.rs`：
/// id 全集与顺序、派生名（工具名 / action_id / 资源 URI）、逐域数字、
/// 以及**生成面的 SHA-256**（MCP 工具定义 + JSON Schema + CLI 帮助 + 覆盖表报告）。
/// 规则：id 不改名不删除；字段只增；新增 id 走 v1.x 且必须同步登记覆盖表。
/// 参数名与执行器取用字段的自动比对在 `crates/cmd/tests/param_dto_parity.rs`。
///
/// ## 写命令的标注规则（#315 定；#316-#319 加新写命令时直接套这一段）
///
/// **`Idempotency::Required` 要两条同时成立**，缺一条就不许标，也**不许声明
/// `idempotency_key` 参数**：
/// 1. 重试会产生**新的业务对象**或**累加**副作用——发消息多一条、建群多一个群、
///    观看心跳多一段时长、转账多一笔；
/// 2. 服务端有**真的消费这个键**的地方，两种形态之一：
///    * handler 读 `Idempotency-Key` 头做重放——`crates/platform/src/groups.rs:519-528`
///      （建群，配 `replay_idempotent`/`remember_idempotent`，`:110-139`）、
///      `crates/platform/src/wallet.rs:635-681`（转账，除重放外还比对**参数指纹**，
///      同键异参回 409，比建群那套更严）；
///    * 键被**派生成业务 id**——`server/src/lib.rs:972-988` 的 send：
///      `SHA-256(key ‖ actor ‖ conv)` 当前 16 字节就是 `msg_id`，
///      重复提交撞的是既有的去重与唯一索引，重试落在同一条消息上。
///
/// 执行器侧必须把键**发出去**（`Http::post_idempotent` 的 `Idempotency-Key` 头 +
/// body 字段），否则声明就是装饰——`param_dto_parity` 的方向 2 专抓这个。
///
/// 只成立 1、不成立 2 ⇒ **别加键**。加了就是"schema 里说支持、代码读不到"，
/// 正是 #314 的比对器当场抓出的那 4 条（`group.add_member` / `msg.send` /
/// `group.members`）。此时的正确做法：把"重试会重复什么"写进这条命令的注释、
/// 在服务端补出去重点（platform 诉求），补上之后再标 Required。
///
/// **天然幂等**标 `None` 且**不声明** `idempotency_key`，但必须写明靠哪一处代码
/// 保证。只有这三种来源算数：
/// * 唯一键 + `ON CONFLICT DO NOTHING`：群成员是 `PRIMARY KEY (group_id, user_id)`
///   （`crates/platform/src/db/schema.rs:212-219`），写入见 `groups.rs:368-373`；
/// * 游标只推不退：`conv.ack` 推 `read_seq`，同一个值推两次没有可观察差别
///   （ISM 的"重放按水位丢弃"同一形状）；
/// * 状态机门：`video.job.retry` 只接受 `Failed`（`crates/video/src/jobs.rs:1147-1152`），
///   第二次重试拿到 409，而不是再排一次队。
///
/// **`Confirmation::Required` 满足任一即标**：副作用不可撤销（`msg.recall`）、
/// 把别人从系统里拿走或让别人的东西消失（`group.kick`）、动钱或动凭证。
/// 只增不减、服务端又天然去重的关系写入（拉人进群、标已读）**不要**加确认——
/// 把日常操作也套上确认，人就会开始无脑点确认，那是另一类事故。
/// 确认链路的令牌一次性与"改参重确认"在 `crate::confirmation`，
/// 用例在 `crates/cmd/tests/confirmation_token.rs`。
///
/// **`Confirmation::HumanOnly` 是第三级，别跟 `Required` 混用**。`Required`
/// 在无人值守通道上只是"把参数摘要再核一遍"——计划与令牌都回给同一个 agent，
/// 它自己就能走完（见 `crates/mcp/src/server.rs` 的 `confirm_step`）。
/// 标 `HumanOnly` 的意思是**这个通道的令牌根本发不出来**：动钱（`Payment`）、
/// 后台动作（`Admin`）、以及任何"出了事得人自己收场"的凭证类操作。
/// 判据只有一条：**能否把后果完整地交回给人**。能——`Required`；
/// 不能——`HumanOnly`。`ScopeClass::Payment` 与 `Admin` 必须标这一级，
/// 由 `contract_v1_baseline.rs` 的 `payment_and_admin_commands_are_human_only` 守着。
///
/// ## 为什么按域拆成数据文件（#329）
/// #314 时试过把这张表**机械**拆成 per-domain 模块：生成面逐字节
/// 不变、全测试绿，但 `scripts/quality-gate.py` 的"跨文件重复窗口"
/// 从 58 涨到 80——每条 `CommandSpec { .. }` 字面量的尾巴七行逐字
/// 相同，一分文件就成了跨文件重复。所以当时的结论是先不拆，等四张
/// 命令面卡（#316–#319）串行合完再做**数据驱动**的拆法，也就是现在
/// 这副样子：
/// * 模板只存在一份：本文件里的 `spec()` 构造器（`platforms` 恒为
///   `Platforms::ALL`、`host` 恒为 `None`，唯一的例外走 `with_host`）；
/// * 每个域一个文件（`catalog/<域>.rs`），里面只有数据——每条命令
///   一个具名常量、一次 `spec(...)` 调用，逐条的标注理由注释随
///   命令一起搬过去，原文不变；
/// * 本文件的 `CATALOG` 只做装配：按冻结顺序列出各域的常量。
///   顺序仍按"人怎么用"（读 → 写 → 破坏性），跨域交错是刻意的。
///
/// 选 `.rs` 数据文件而不是 `data/commands/<域>.json`：参数类型、
/// 标注枚举在编译期就被检查，`spec()` 的位置参数漏填会直接编译
/// 失败；JSON 则要到运行时解析才知道对不对，还得再养一套
/// JSON → CommandSpec 的映射与它自己的漂移测试。
mod channel;
mod community;
mod conv;
mod friend;
mod group;
mod me;
mod media;
mod msg;
mod video;

/// 命令声明的唯一模板（#329）：各域文件只给数据，字段的默认值
/// 在这里收敛——`platforms` 恒为全平台，`host` 恒为无宿主能力，
/// 唯一需要宿主的命令（`video.job.create` 的文件选择）走
/// [`with_host`]。参数顺序刻意把 `args` 放最后：标注五元组夹在
/// 唯一的参数说明与唯一的参数表之间，任何 8 行窗口都跨着唯一
/// 内容，跨文件重复窗口不会因为拆文件而涨（#329 的验收口径）。
#[allow(clippy::too_many_arguments)]
const fn spec(
    id: &'static str,
    summary: &'static str,
    params: &'static str,
    capability: Capability,
    needs_conv: bool,
    scope: ScopeClass,
    confirmation: Confirmation,
    idempotency: Idempotency,
    args: &'static [Param],
) -> CommandSpec {
    CommandSpec {
        id,
        summary,
        params,
        args,
        capability,
        needs_conv,
        scope,
        confirmation,
        idempotency,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    }
}

/// 无参只读命令的简写：`params` 恒为 "无"、`args` 恒空、能力恒为
/// Read、不需要会话、确认与幂等恒为 None——目录里有 8 条这样的
/// 命令（`conv.list`、`me.*`、`friend.list` 等）。用一行调用表达，
/// 除了让数据文件更短，还有一个实际作用：多行 `spec(...)` 调用在
/// 这批命令上会排出逐字相同的 8 行尾巴，正是 #329 要压住的跨文件
/// 重复窗口形状。
const fn read_no_args(id: &'static str, summary: &'static str, scope: ScopeClass) -> CommandSpec {
    spec(
        id,
        summary,
        "无",
        Capability::Read,
        false,
        scope,
        Confirmation::None,
        Idempotency::None,
        &[],
    )
}

/// 给一条已构造好的声明补上宿主能力（见 [`spec`] 的说明）。
const fn with_host(spec: CommandSpec, host: HostCapability) -> CommandSpec {
    CommandSpec { host, ..spec }
}

pub const CATALOG: &[CommandSpec] = &[
    // ---- 读：会话 ----
    conv::CONV_LIST,
    conv::CONV_UNREAD,
    conv::CONV_HISTORY,
    conv::CONV_SYNC,
    // ---- 读：消息 ----
    msg::MSG_LIST,
    msg::MSG_SEARCH,
    msg::MSG_GET,
    // ---- 读：成员与自查 ----
    group::GROUP_MEMBERS,
    group::GROUP_INFO,
    group::GROUP_ANNOUNCEMENT_GET,
    group::GROUP_MUTE_ALL_GET,
    community::COMMUNITY_THREADS,
    channel::CHANNEL_ROLES,
    me::ME_INFO,
    me::ME_SESSIONS,
    // ---- 读：好友与申请（#316 第一批，服务端路由早已存在）----
    friend::FRIEND_LIST,
    friend::FRIEND_REQUESTS_INCOMING,
    friend::FRIEND_REQUESTS_OUTGOING,
    // ---- 写 ----
    msg::MSG_SEND,
    conv::CONV_ACK,
    msg::MSG_FORWARD,
    group::GROUP_CREATE,
    group::GROUP_ADD_MEMBER,
    // ---- 写：群管理（#318）----
    group::GROUP_ANNOUNCEMENT_SET,
    group::GROUP_MUTE_ALL_SET,
    // ---- 写：好友申请（#316 第一批）----
    friend::FRIEND_REQUEST_CREATE,
    friend::FRIEND_REQUEST_ACCEPT,
    friend::FRIEND_REQUEST_DECLINE,
    // ---- 写：好友备注与删除（#317，relations 第二批）----
    friend::FRIEND_REMARK_SET,
    friend::FRIEND_REMOVE,
    // ---- 黑名单与隐私（#317）----
    friend::FRIEND_BLOCK_LIST,
    friend::FRIEND_BLOCK_ADD,
    friend::FRIEND_BLOCK_REMOVE,
    friend::FRIEND_PRIVACY_GET,
    friend::FRIEND_PRIVACY_SET,
    // ---- 读：视频 / 点播 ----
    video::VIDEO_SELECT,
    video::VIDEO_STATS,
    video::VIDEO_JOB_GET,
    // ---- 读：媒体对象（#319）----
    media::MEDIA_FILES_LIST,
    media::MEDIA_UPLOAD_GET,
    // ---- 写：视频 / 点播 ----
    video::VIDEO_JOB_CREATE,
    video::VIDEO_JOB_RETRY,
    video::VIDEO_VIEW_REPORT,
    // ---- 写：媒体对象（#319）----
    media::MEDIA_UPLOAD_START,
    media::MEDIA_UPLOAD_COMPLETE,
    media::MEDIA_UPLOAD_CANCEL,
    media::MEDIA_OBJECT_ACCESS,
    media::MEDIA_OBJECT_REVOKE,
    // ---- 破坏性 ----
    group::GROUP_KICK,
    msg::MSG_RECALL,
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

/// 所有出现过的域名，按 catalog 顺序去重（CLI 帮助的分组依据）。
pub fn domains() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for spec in CATALOG {
        let (domain, _) = spec.cli_path();
        if !out.contains(&domain) {
            out.push(domain);
        }
    }
    out
}

/// 某个域名下的全部命令（CLI `wxlike <domain> --help`）。
pub fn by_domain(domain: &str) -> Vec<&'static CommandSpec> {
    CATALOG
        .iter()
        .filter(|s| s.cli_path().0 == domain)
        .collect()
}

/// 参数错误。**每个分支都带参数名**——"参数错误"四个字对调用方毫无用处。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgError {
    /// 参数整体不是对象。
    NotObject,
    /// 未知参数（含拼写错误）。
    Unknown(String),
    /// 缺少必填参数。
    Missing(&'static str),
    /// 类型不符。
    WrongType {
        name: &'static str,
        expected: &'static str,
    },
}

impl std::fmt::Display for ArgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgError::NotObject => f.write_str("参数必须是 JSON 对象"),
            ArgError::Unknown(name) => write!(f, "未知参数 {name:?}（拒绝静默忽略）"),
            ArgError::Missing(name) => write!(f, "缺少必填参数 {name}"),
            ArgError::WrongType { name, expected } => {
                write!(f, "参数 {name} 类型不对，期望 {expected}")
            }
        }
    }
}

impl std::error::Error for ArgError {}

/// 按 catalog 声明校验参数。未知参数直接拒绝——见 [`ArgError::Unknown`]。
pub fn validate_args(spec: &CommandSpec, args: &Value) -> Result<(), ArgError> {
    let obj = args.as_object().ok_or(ArgError::NotObject)?;
    reject_unknown(spec, obj)?;
    for param in spec.args {
        check_param(param, obj.get(param.name))?;
    }
    Ok(())
}

/// 拼错的参数不该被当成"没传"：静默忽略会让调用方永远收不到报错。
fn reject_unknown(spec: &CommandSpec, obj: &Map<String, Value>) -> Result<(), ArgError> {
    for key in obj.keys() {
        if !spec.args.iter().any(|p| p.name == key) {
            return Err(ArgError::Unknown(key.clone()));
        }
    }
    Ok(())
}

/// 单个参数的值检查。`null` 与"没给"等价（JSON 里两者都表示缺席）。
fn check_param(param: &Param, value: Option<&Value>) -> Result<(), ArgError> {
    let given = value.filter(|v| !v.is_null());
    match given {
        None if param.required => Err(ArgError::Missing(param.name)),
        None => Ok(()),
        Some(v) if param.ty.accepts(v) => Ok(()),
        Some(_) => Err(ArgError::WrongType {
            name: param.name,
            expected: param.ty.json_type(),
        }),
    }
}

/// 生成该命令的 JSON Schema。
pub fn schema_of(spec: &CommandSpec) -> Value {
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    for p in spec.args {
        let mut prop = Map::new();
        prop.insert("type".into(), json!(p.ty.json_type()));
        if p.ty == ParamType::NonNegativeInt {
            prop.insert("minimum".into(), json!(0));
        }
        if p.ty == ParamType::StrList {
            prop.insert("items".into(), json!({"type": "string"}));
        }
        prop.insert("description".into(), json!(p.note));
        properties.insert(p.name.to_string(), Value::Object(prop));
        if p.required {
            required.push(json!(p.name));
        }
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        // 默认拒绝未知参数：拼错的参数不该被当成"没传"
        "additionalProperties": false,
    })
}

/// 一条命令的 MCP 工具定义（`tools/list` 的一项）。
///
/// `annotations` 是 MCP 的客户端提示位：读/破坏/幂等。它不是授权依据
/// （授权永远在 [`crate::policy`] 与服务端），只是让界面能提前警告。
pub fn tool_definition(spec: &CommandSpec) -> Value {
    json!({
        "name": spec.tool_name(),
        "title": spec.summary,
        "description": format!("{}｜参数：{}", spec.summary, spec.params),
        "inputSchema": schema_of(spec),
        "annotations": {
            "readOnlyHint": spec.capability == Capability::Read,
            "destructiveHint": spec.capability == Capability::Destructive,
            "idempotentHint": spec.idempotency == Idempotency::Required
                || spec.capability == Capability::Read,
            "openWorldHint": false,
        },
        "_meta": {
            "wxlike/actionId": spec.action_id(),
            "wxlike/scope": spec.scope.as_str(),
            "wxlike/confirmation": spec.confirmation.as_str(),
            "wxlike/platforms": spec.platforms.names(),
            "wxlike/host": spec.host.as_str(),
        }
    })
}

/// CLI 帮助文本。`domain = None` 时输出总表，否则输出该域的命令表。
///
/// 与 MCP 描述共用 catalog：手写第二份帮助，一定会出现"帮助里有的命令
/// 已经删了"这种事故。
pub fn cli_help(domain: Option<&str>) -> String {
    match domain {
        None => help_overview(),
        Some(d) => help_domain(d),
    }
}

fn help_overview() -> String {
    let mut out = String::new();
    out.push_str(
        "用法: wxlike [--server URL] [--token T] [--json] <域> <动作> [--param k=v ...]
",
    );
    out.push_str(
        "       wxlike [--server URL] [--token T] list [关键词]
",
    );
    out.push_str(
        "       wxlike policy | inspect | doctor | run --cmd 'id={json}'

",
    );
    out.push_str(&format!(
        "{:<24} {:<12} 说明
",
        "命令", "能力"
    ));
    for spec in CATALOG {
        out.push_str(&format!(
            "{:<24} {:<12} {}
",
            spec.id, spec.capability, spec.summary
        ));
    }
    out.push_str(&format!(
        "
共 {} 条命令，域名：{}
",
        CATALOG.len(),
        domains().join(", ")
    ));
    out
}

fn help_domain(domain: &str) -> String {
    let specs = by_domain(domain);
    if specs.is_empty() {
        return format!(
            "未知域名 {domain}；可用：{}
",
            domains().join(", ")
        );
    }
    let mut out = format!(
        "域 {domain}：
"
    );
    for spec in specs {
        push_command_help(&mut out, domain, spec);
    }
    out
}

fn push_command_help(out: &mut String, domain: &str, spec: &CommandSpec) {
    let (_, verb) = spec.cli_path();
    out.push_str(&format!(
        "
  wxlike {domain} {verb} [--param k=v ...]
"
    ));
    out.push_str(&format!(
        "    {}（{}，scope={}，平台={}）
",
        spec.summary,
        spec.capability,
        spec.scope,
        spec.platforms.names().join("/")
    ));
    if spec.args.is_empty() {
        out.push_str(
            "    参数：无
",
        );
        return;
    }
    for p in spec.args {
        out.push_str(&format!(
            "    --param {}=<{}>{}	{}
",
            p.name,
            p.ty.json_type(),
            if p.required { " 必填" } else { "" },
            p.note
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_is_well_formed() {
        assert_eq!(CATALOG.len(), 50, "命令数变了：快照测试要一起加 id");
        let mut ids = HashSet::new();
        let mut tools = HashSet::new();
        for spec in CATALOG {
            assert!(ids.insert(spec.id), "重复的命令 id: {}", spec.id);
            assert!(spec.id.contains('.'), "id 必须是 域.动作: {}", spec.id);
            assert!(!spec.summary.is_empty(), "缺说明: {}", spec.id);
            assert!(!spec.params.is_empty(), "缺参数说明: {}", spec.id);
            // 工具名是单射：`a_b.c` 与 `a.b_c` 不能撞成同一个名字
            assert!(tools.insert(spec.tool_name()), "工具名冲突: {}", spec.id);
            assert!(
                !spec.id.contains('-'),
                "id 里出现连字符会让工具名映射不再单射: {}",
                spec.id
            );
            // 声明了的参数必须出现在人类可读说明里，否则人和 AI 看到两套
            for p in spec.args {
                assert!(
                    spec.params.contains(p.name),
                    "{} 的 params 说明里没有 {}",
                    spec.id,
                    p.name
                );
            }
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
                "group.announcement.get",
                "group.mute_all.get",
                "community.threads",
                "channel.roles",
                "me.info",
                "me.sessions",
                "friend.list",
                "friend.requests.incoming",
                "friend.requests.outgoing",
                "msg.send",
                "conv.ack",
                "msg.forward",
                "group.create",
                "group.add_member",
                "group.announcement.set",
                "group.mute_all.set",
                "friend.request.create",
                "friend.request.accept",
                "friend.request.decline",
                "friend.remark.set",
                "friend.remove",
                "friend.block.list",
                "friend.block.add",
                "friend.block.remove",
                "friend.privacy.get",
                "friend.privacy.set",
                "video.select",
                "video.stats",
                "video.job.get",
                "media.files.list",
                "media.upload.get",
                "video.job.create",
                "video.job.retry",
                "video.view.report",
                "media.upload.start",
                "media.upload.complete",
                "media.upload.cancel",
                "media.object.access",
                "media.object.revoke",
                "group.kick",
                "msg.recall",
            ]
        );
        // action_id 与 MCP 工具名也一并钉住：改任何一个都要过这条测试
        assert_eq!(get("msg.send").unwrap().action_id(), "msg.send");
        assert_eq!(get("video.job.get").unwrap().tool_name(), "video-job-get");
        assert_eq!(
            get("group.add_member").unwrap().tool_name(),
            "group-add_member"
        );
        assert_eq!(
            get("msg.send").unwrap().resource_uri(),
            "wxlike://actions/msg.send"
        );
    }

    #[test]
    fn video_commands_are_capability_gated_not_conv_gated() {
        for id in ["video.select", "video.stats", "video.job.get"] {
            let spec = get(id).unwrap();
            assert!(!spec.needs_conv, "{id} 不该要求 conv");
            assert_eq!(spec.capability, Capability::Read, "{id}");
        }
        for id in ["video.job.create", "video.job.retry", "video.view.report"] {
            let spec = get(id).unwrap();
            assert!(!spec.needs_conv, "{id} 不该要求 conv");
            assert_eq!(spec.capability, Capability::Write, "{id}");
        }
    }

    #[test]
    fn capability_serialises_to_the_same_words_as_as_str() {
        for cap in [Capability::Read, Capability::Write, Capability::Destructive] {
            let json = serde_json::to_string(&cap).unwrap();
            assert_eq!(json, format!("\"{}\"", cap.as_str()), "{cap:?}");
            let back: Capability = serde_json::from_str(&json).unwrap();
            assert_eq!(back, cap, "往返必须相等");
        }
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

    #[test]
    fn schema_rejects_unknown_and_missing_and_wrong_types() {
        let spec = get("msg.send").unwrap();
        assert!(validate_args(
            spec,
            &json!({
                "conv": "g-1", "text": "hi", "idempotency_key": "k"
            })
        )
        .is_ok());
        // 未知参数
        let err = validate_args(
            spec,
            &json!({
                "conv": "g-1", "text": "hi", "idempotency_key": "k", "txet": "typo"
            }),
        )
        .unwrap_err();
        assert_eq!(err, ArgError::Unknown("txet".into()));
        // 缺必填
        let err = validate_args(spec, &json!({"conv": "g-1", "text": "hi"})).unwrap_err();
        assert_eq!(err, ArgError::Missing("idempotency_key"));
        // 类型不对：数字当字符串
        let err = validate_args(
            spec,
            &json!({"conv": 1, "text": "hi", "idempotency_key": "k"}),
        )
        .unwrap_err();
        assert_eq!(
            err,
            ArgError::WrongType {
                name: "conv",
                expected: "string"
            }
        );
        // 非对象
        assert_eq!(
            validate_args(spec, &json!([])).unwrap_err(),
            ArgError::NotObject
        );
    }

    #[test]
    fn schema_distinguishes_int_from_float_and_string() {
        let spec = get("conv.history").unwrap();
        let base = json!({"conv": "g-1"});
        assert!(validate_args(spec, &base).is_ok(), "可选参数可以不给");
        assert!(validate_args(spec, &json!({"conv": "g-1", "limit": 20})).is_ok());
        for bad in [json!(1.5), json!("20"), json!(-1), json!(true)] {
            assert!(
                validate_args(spec, &json!({"conv": "g-1", "limit": bad})).is_err(),
                "limit={bad} 必须被拒"
            );
        }
    }

    #[test]
    fn generated_schema_matches_the_declaration() {
        let spec = get("video.view.report").unwrap();
        let schema = schema_of(spec);
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["watched_ms"]["type"], "integer");
        assert_eq!(schema["properties"]["watched_ms"]["minimum"], 0);
        assert_eq!(schema["properties"]["completed"]["type"], "boolean");
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(required, ["video_id", "viewer", "watched_ms"]);
        for p in spec.args {
            assert!(
                schema["properties"].get(p.name).is_some(),
                "schema 少了 {}",
                p.name
            );
        }
    }

    #[test]
    fn tool_definition_carries_annotations_and_action_id() {
        let t = tool_definition(get("group.kick").unwrap());
        assert_eq!(t["name"], "group-kick");
        assert_eq!(t["annotations"]["readOnlyHint"], false);
        assert_eq!(t["annotations"]["destructiveHint"], true);
        assert_eq!(t["_meta"]["wxlike/actionId"], "group.kick");
        assert_eq!(t["_meta"]["wxlike/confirmation"], "required");
    }

    #[test]
    fn cli_help_covers_every_domain_and_command() {
        let text = cli_help(None);
        for spec in CATALOG {
            assert!(text.contains(spec.id), "总帮助漏了 {}", spec.id);
        }
        for d in domains() {
            assert!(text.contains(d), "总帮助漏了域名 {d}");
        }
        let video = cli_help(Some("video"));
        assert!(video.contains("wxlike video job.get"), "{video}");
        assert!(video.contains("--param kbps=<integer> 必填"), "{video}");
        assert!(cli_help(Some("nope")).contains("未知域名"), "未知域要报错");
    }
}
