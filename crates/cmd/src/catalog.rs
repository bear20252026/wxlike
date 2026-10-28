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

/// 现有 32 条：首批 18 条，加 Zone C 的社区/频道 2 条、Zone E 的视频/点播 6 条，
/// 再加 #316 relations 第一批的好友/申请 6 条。
/// **加功能先加这里**，再去实现执行器——`crates/client` 的防漂移测试会盯着。
///
/// 顺序按"人怎么用"排，不按字母：先看（会话/消息/成员/自己/好友），再写，最后危险操作。
///
/// ## contract-v1 已冻结（#314；#316 第一次扩基线）
/// 这 32 个 id 与 `crates/cmd/data/domains.json`（+ `data/entries/<域>.json`）
/// 的 68 条入口（automated 32 / blocked 31 / host_only 5）是当前基线，
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
/// ## 为什么这张表还在一个文件里
/// 按域拆成 `catalog/conv.rs`、`catalog/video.rs`……试过，也**能拆干净**
/// （生成面摘要逐字节不变、全测试绿），但本仓 `scripts/quality-gate.py` 的
/// "跨文件重复窗口"会从 58 涨到 80（基线 66，`--update` 禁用）：命令表是
/// 32 条近似记录，每条尾巴七行逐字相同，一分文件就被判成重复。
/// 所以 #316-#319 会共用这一个文件——改动请小步提交，别攒大批。
pub const CATALOG: &[CommandSpec] = &[
    // ---- 读：会话 ----
    CommandSpec {
        id: "conv.list",
        summary: "列出我参与的会话（含未读数）",
        params: "无",
        args: &[],
        capability: Capability::Read,
        needs_conv: false,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "conv.unread",
        summary: "某个会话的未读快照：head_seq / read_seq / unread",
        params: "{conv: string}",
        args: &[Param::req("conv", ParamType::Str, "会话 id")],
        capability: Capability::Read,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "conv.history",
        summary: "翻历史消息，按会话内序列号倒序分页",
        params: "{conv: string, before_seq?: number, limit?: number}",
        args: &[
            Param::req("conv", ParamType::Str, "会话 id"),
            Param::opt("before_seq", ParamType::NonNegativeInt, "只取 seq 小于它的"),
            Param::opt("limit", ParamType::NonNegativeInt, "条数上限"),
        ],
        capability: Capability::Read,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "conv.sync",
        summary: "五层差分指令：客户端该全量拉还是增量拉",
        params: "{conv: string, version: number}",
        args: &[
            Param::req("conv", ParamType::Str, "会话 id"),
            Param::req("version", ParamType::NonNegativeInt, "客户端持有的版本号"),
        ],
        capability: Capability::Read,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    // ---- 读：消息 ----
    CommandSpec {
        id: "msg.list",
        summary: "按 seq 区间取消息（不翻页，给脚本用）",
        params: "{conv: string, from_seq: number, to_seq?: number}",
        args: &[
            Param::req("conv", ParamType::Str, "会话 id"),
            Param::req("from_seq", ParamType::NonNegativeInt, "起始 seq（含）"),
            Param::opt("to_seq", ParamType::NonNegativeInt, "结束 seq（含）"),
        ],
        capability: Capability::Read,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "msg.search",
        summary: "在可见会话内全文搜索",
        params: "{q: string, limit?: number}",
        args: &[
            Param::req("q", ParamType::Str, "搜索词"),
            Param::opt("limit", ParamType::NonNegativeInt, "条数上限"),
        ],
        capability: Capability::Read,
        needs_conv: false,
        scope: ScopeClass::Content,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "msg.get",
        summary: "取单条消息（含撤回状态）",
        params: "{conv: string, msg_id: string}",
        args: &[
            Param::req("conv", ParamType::Str, "会话 id"),
            Param::req("msg_id", ParamType::Str, "消息 id"),
            // #315 撤销声明：这里原本挂着 `confirmation_token?`，但本条是
            // `Confirmation::None` 的读命令——`ConfirmationStore::prepare` 对
            // 非 Required 的命令直接回 `NotRequired`（`crate::confirmation::prepare`），
            // 令牌**根本签发不出来**，MCP 的 `confirm_step` 也只在
            // `spec.confirmation.is_required()` 时才走（`crates/mcp/src/server.rs:275-283`）。
            // 留着它等于让 AI 以为"读一条消息要先确认"。
            // 同一条不变式由 `crates/cmd/tests/idempotency_annotations.rs` 守着：
            // 声明 `confirmation_token` ⇔ `Confirmation::Required`。
        ],
        capability: Capability::Read,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    // ---- 读：成员与自查 ----
    CommandSpec {
        id: "group.members",
        // #315：`after_user` / `limit` 原来是装饰性的（handler 一次也没取用）。
        // 现在接上线了——切页发生在执行器（`crates/client/src/execute/handlers.rs`
        // 的 `members_page`，用例 `group_members_honours_the_cursor_and_the_limit`），
        // 所以说明里不再写"万人群不全量拉"：服务端 `list_members`
        // （`crates/platform/src/groups.rs:806-815`）只有一个 `Path(group_id)`，
        // 没有 `Query`，**整份名单仍然一次拉回来**，命令面只承诺"给你的这一页"。
        // 游标下推是 #315 提的 platform 诉求；补上之后只改 handler，声明不动。
        summary: "群成员列表（按游标翻页；翻页目前切在客户端）",
        params: "{conv: string, after_user?: string, limit?: number}",
        args: &[
            Param::req("conv", ParamType::Str, "群 id"),
            Param::opt(
                "after_user",
                ParamType::Str,
                "游标：上一页最后一个 user_id，从它之后继续；它已不在名单里就给空页",
            ),
            Param::opt("limit", ParamType::NonNegativeInt, "这一页最多几条（缺省=剩下的全给）"),
        ],
        capability: Capability::Read,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "group.info",
        summary: "群基本信息与我的角色",
        params: "{conv: string}",
        args: &[Param::req("conv", ParamType::Str, "群 id")],
        capability: Capability::Read,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "community.threads",
        summary: "社区里正在讨论的话题（跨频道汇总，带回复数与我的订阅状态）",
        params: "{community: string, limit?: number}",
        args: &[
            Param::req("community", ParamType::Str, "社区 id"),
            Param::opt("limit", ParamType::NonNegativeInt, "条数上限"),
        ],
        capability: Capability::Read,
        needs_conv: false,
        scope: ScopeClass::Content,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "channel.roles",
        summary: "频道角色名单：谁在这个频道里是 owner/moderator/member/guest，含继承标记",
        params: "{conv: string}",
        args: &[Param::req("conv", ParamType::Str, "频道 id")],
        capability: Capability::Read,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "me.info",
        summary: "我是谁：user_id、昵称、在线设备",
        params: "无",
        args: &[],
        capability: Capability::Read,
        needs_conv: false,
        scope: ScopeClass::Account,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "me.sessions",
        summary: "我的登录会话（设备/时间），用于排查异地登录",
        params: "无",
        args: &[],
        capability: Capability::Read,
        needs_conv: false,
        scope: ScopeClass::Account,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    // ---- 读：好友与申请（#316 第一批，服务端路由早已存在）----
    CommandSpec {
        id: "friend.list",
        // 路由 `GET /api/friends`（`crates/platform/src/friends.rs:32`），
        // handler `list_friends`（`:511-517`）只把 **`authed.user_id`** 交给
        // `Platform::friends()`（`:299-318`，SQL `WHERE user_id = ?1`）。
        // 命令面因此**不声明**任何"看谁的名单"参数：调用方没法指名别人，
        // 也就没有"传个 user_id 就把别人的通讯录拿走"的口子。
        // scope 取 `Account`（授权对象=调用者自己），不是 `Conversation`——
        // 通讯录不是会话，拿会话白名单去放行它属于顶部注释警告的"混成一个 resource"。
        summary: "好友列表（我的通讯录，按加入时间排）",
        params: "无",
        args: &[],
        capability: Capability::Read,
        needs_conv: false,
        scope: ScopeClass::Account,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "friend.requests.incoming",
        // 路由 `GET /api/friends/requests/incoming`（`friends.rs:30`）。
        // "收件向"就是**待办队列**：SQL 带 `AND status = 'pending'`（`:257`），
        // 接受/拒绝之后自动离开，所以客户端不必再自己过滤一遍。
        // 服务端不接 `limit`/游标（handler 只有 `State` + `Authed`，`:495-501`），
        // 所以这里**不声明**分页参数——声明了没人消费，正是 #314 比对器抓的装饰。
        // 想要增量同步（`GET /api/friends/changes`）是 #317 的一批，本卡不纳入。
        summary: "待我处理的好友申请（收件向，只含 pending）",
        params: "无",
        args: &[],
        capability: Capability::Read,
        needs_conv: false,
        scope: ScopeClass::Account,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "friend.requests.outgoing",
        // 路由 `GET /api/friends/requests/outgoing`（`friends.rs:31`）。
        // 与收件向**不同口径**：`outgoing_requests()`（`:273-276`）走
        // `list_requests("from_uid", me)`，SQL 不带状态条件（`:281-282`）——
        // 发起方要看得见"谁拒了我"，所以这一条返回的是全部状态。
        // `col` 是调用方写死的闭集（`:278-283` 注释），不是用户输入。
        summary: "我发出去的好友申请（发起向，含已接受/已拒绝）",
        params: "无",
        args: &[],
        capability: Capability::Read,
        needs_conv: false,
        scope: ScopeClass::Account,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    // ---- 写 ----
    CommandSpec {
        id: "msg.send",
        // #315 撤销声明：这里原本挂着可选的 `reply_to`（引用回复）。
        // 执行器确实**不发送**它（`crates/client/src/execute/handlers.rs` 的
        // `msg.send` 条目只拼 `{text, idempotency_key}`）；接收侧
        // `POST /api/conv/{conv}/send` 的 `SendBody`（`server/src/lib.rs:935-942`）
        // 也没有这个字段——线格式 `Envelope.quote_of` 是存在的
        // （`crates/core/src/store.rs:60,170`、`crates/platform/src/lib.rs:227`），
        // 缺的是 HTTP 面透传：`SendBody` 加字段 + send 里把 hex msg_id 解回
        // `quote_of`。那要动 `server/**`（本轮不可动），已写成 #315 的 platform
        // 诉求单开卡；补上后只需 handler 拼字段 + 把声明加回来。
        // 幂等键是真接线的：执行器 `Http::post_idempotent`（头 + body 都带），
        // 服务端把键派生成 msg_id（`server/src/lib.rs:972-988`），重试落同一条消息。
        summary: "发一条文本消息（必须带 idempotency_key，重试不会重复发）",
        params: "{conv: string, text: string, idempotency_key: string}",
        args: &[
            Param::req("conv", ParamType::Str, "会话 id"),
            Param::req("text", ParamType::Str, "消息正文"),
            Param::req("idempotency_key", ParamType::Str, "幂等键"),
        ],
        capability: Capability::Write,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::Required,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "conv.ack",
        summary: "把已读游标推到指定 seq（缺省=最新）",
        params: "{conv: string, seq?: number}",
        args: &[
            Param::req("conv", ParamType::Str, "会话 id"),
            Param::opt("seq", ParamType::NonNegativeInt, "推到哪个 seq"),
        ],
        capability: Capability::Write,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "msg.forward",
        summary: "转发一条消息到另一个会话",
        params: "{from_conv: string, to_conv: string, msg_id: string, idempotency_key: string}",
        args: &[
            Param::req("from_conv", ParamType::Str, "来源会话"),
            Param::req("to_conv", ParamType::Str, "目标会话"),
            Param::req("msg_id", ParamType::Str, "消息 id"),
            Param::req("idempotency_key", ParamType::Str, "幂等键"),
        ],
        capability: Capability::Write,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::Required,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "group.create",
        summary: "建群并拉入初始成员",
        params: "{name: string, member_ids: [string], idempotency_key: string}",
        args: &[
            Param::req("name", ParamType::Str, "群名"),
            Param::opt("member_ids", ParamType::StrList, "初始成员 user_id 列表"),
            Param::req("idempotency_key", ParamType::Str, "幂等键"),
        ],
        capability: Capability::Write,
        needs_conv: false,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::Required,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "group.add_member",
        // #315 撤销声明：这里原本挂着可选的 `idempotency_key`，但它是**装饰**——
        // 执行器走 `expect_json("POST", …)`（`crates/client/src/execute/handlers.rs`
        // 的 `group.add_member` 条目），从头到尾没把键发出去；服务端
        // `add_members`（`crates/platform/src/groups.rs:552-581`）也**不读**
        // `Idempotency-Key` 头——读头的只有建群那条（`:519-528`）。
        // 要真接上得同时改执行器和服务端 handler，platform 侧本轮不可动（#328 独占），
        // 所以按本文件顶部的规则**撤掉声明**，不留"schema 里说支持、代码读不到"。
        // 标注 `Idempotency::None` 的依据是**天然幂等**：重试会不会重复加人？不会——
        // 服务端去重靠 `PRIMARY KEY (group_id, user_id)`
        // （`crates/platform/src/db/schema.rs:212-219`），写入用
        // `ON CONFLICT(group_id, user_id) DO NOTHING`（`groups.rs:368-373`）；
        // "X 加入了群聊"的系统通知只在 `added > 0` 时才发（`groups.rs:559-579`），
        // 整批重试第二次 `added=0`，没有任何可观察副作用。
        summary: "拉人进群",
        params: "{conv: string, user_ids: [string]}",
        args: &[
            Param::req("conv", ParamType::Str, "群 id"),
            Param::req("user_ids", ParamType::StrList, "要拉进来的 user_id 列表"),
        ],
        capability: Capability::Write,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    // ---- 写：好友申请（#316 第一批）----
    CommandSpec {
        id: "friend.request.create",
        // 顶部规则第 1 条问"重试会不会长出第二个业务对象"。这里**不会**，
        // 而且靠的不是幂等键：`send_friend_request`
        // （`crates/platform/src/friends.rs:143-163`）先按 `(from_uid, to_uid)`
        // 查已有行，`pending`/`accepted` 直接 `Conflict`→409（`:150-155`），
        // 只有 `declined` 把**同一行**改回 pending 并复用原 id（`:156-163`）；
        // 底下还有表约束 `UNIQUE(from_uid, to_uid)`
        // （`crates/platform/src/db/schema.rs:43-51`；规范形状同
        // `crates/features/relations/src/lib.rs:84-96`）。
        // 409 那条早退发生在任何 INSERT 与 `contact_log`（`:171-172`）之前，
        // 所以重复提交既不多一条申请、也不多一条推给对方的系统消息。
        // 规则第 2 条同样不成立：`create_request`（`:409-443`）**不读**
        // `Idempotency-Key` 头（读头的只有建群 `groups.rs:519-528` 与转账
        // `wallet.rs:635-681`），执行器无键可发。
        // ⇒ `Idempotency::None`（天然幂等，来源=唯一键）且**不声明**
        // `idempotency_key`——按顶部规则，加了就是"schema 里说支持、代码读不到"。
        // 确认：`Confirmation::None`。这是"只增不减、服务端天然去重的关系写入"，
        // 顶部规则明令**不要**加确认（把日常操作也套上确认，人会开始无脑点确认）。
        summary: "发起好友申请（同一对象重复提交回 409，不会排第二队）",
        params: "{to_user_id: string, message?: string}",
        args: &[
            Param::req("to_user_id", ParamType::Str, "要添加的那个人 user_id"),
            Param::opt("message", ParamType::Str, "申请附言；不给=只发空附言，服务端限长"),
        ],
        capability: Capability::Write,
        needs_conv: false,
        scope: ScopeClass::Account,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "friend.request.accept",
        // 天然幂等的来源是顶部规则列的第三种：**状态机门**。
        // `accept_request`（`crates/platform/src/friends.rs:198-231`）
        // 先 `status != "pending"` → 409（`:203-205`），改写用带条件的
        // `UPDATE … WHERE id = ?1 AND status = 'pending'`（`:209-212`），
        // 并发撞车的第二次拿到 `changed = 0` 同样 409（`:213-216`）；
        // 好友双向行是 `ON CONFLICT(user_id, friend_id) DO NOTHING`
        // （`:221-226`，主键见 `db/schema.rs:55-62`）。
        // ⇒ 重试不会长出第二对好友、不会重复写 `contact_log`，
        // `Idempotency::None` 且不声明 `idempotency_key`
        // （`accept` handler `:445-469` 只有 `Path(req_id)`，不读头）。
        // 鉴权：`req.to_uid != me` → Forbidden（`:200-202`），
        // 所以命令面**只给申请 id**，给不了"我是谁"，也碰不到别人的申请。
        // 确认：`Confirmation::None`——接受是"只增"的关系写入；
        // 真正不可撤销的那一步（删除好友）在 #317。
        summary: "同意一条好友申请（只有 pending 能被改，重复同意回 409）",
        params: "{req_id: string}",
        args: &[Param::req(
            "req_id",
            ParamType::Str,
            "申请 id（incoming 列表里的 id 字段）",
        )],
        capability: Capability::Write,
        needs_conv: false,
        scope: ScopeClass::Account,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "friend.request.decline",
        // 与 accept 同一条路子（状态机门 → `Idempotency::None`），但**门略松一档**，
        // 依据要写准：`decline_request`（`crates/platform/src/friends.rs:233-252`）
        // 的 `status != "pending"` → 409 是**前置检查**（`:238-240`），
        // 下面那条 UPDATE 没有带 `AND status = 'pending'` 条件（`:244-247`）——
        // 串行重试照样 409（第二次进来已是 declined），但并发两次都改写得动，
        // 每趟各追加两条 `contact_log`（`:248-249`）。这是 accept 有
        // `changed == 0` 兜底、decline 没有的一处差别，属 platform 侧观察，
        // 本卡不动 `crates/platform/**`，已记进 #316 报告。
        // 确认：`Confirmation::None`——拒绝不删对方的东西，而且对方可以重新申请
        // （`:156-163` 正是把 declined 那行改回 pending），副作用可复原。
        summary: "拒绝一条好友申请（只有 pending 能被改，重复拒绝回 409）",
        params: "{req_id: string}",
        args: &[Param::req(
            "req_id",
            ParamType::Str,
            "申请 id（incoming 列表里的 id 字段）",
        )],
        capability: Capability::Write,
        needs_conv: false,
        scope: ScopeClass::Account,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    // ---- 读：视频 / 点播 ----
    CommandSpec {
        id: "video.select",
        summary: "按实测带宽选一档码率（服务端留余量，三端选到同一档）",
        params: "{video_id: string, kbps: number}",
        args: &[
            Param::req("video_id", ParamType::Str, "视频 id"),
            Param::req("kbps", ParamType::NonNegativeInt, "实测可用带宽（千比特/秒）"),
        ],
        capability: Capability::Read,
        needs_conv: false,
        scope: ScopeClass::Media,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "video.stats",
        summary: "点播观看统计：条数 / 独立观众 / 总时长 / 人均时长",
        params: "{video_id: string}",
        args: &[Param::req("video_id", ParamType::Str, "视频 id")],
        capability: Capability::Read,
        needs_conv: false,
        scope: ScopeClass::Media,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "video.job.get",
        summary: "查一个转码作业的状态与失败原因",
        params: "{job_id: string}",
        args: &[Param::req("job_id", ParamType::Str, "作业 id")],
        capability: Capability::Read,
        needs_conv: false,
        scope: ScopeClass::Media,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    // ---- 写：视频 / 点播 ----
    CommandSpec {
        id: "video.job.create",
        summary: "提交一个本地视频文件转码（读本地路径，上传后入队）",
        params: "{path: string, callback_url?: string}",
        args: &[
            Param::req("path", ParamType::Str, "本地文件路径"),
            Param::opt("callback_url", ParamType::Str, "完成后回调地址"),
        ],
        capability: Capability::Write,
        needs_conv: false,
        scope: ScopeClass::Media,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        // 读本地文件是宿主能力：CLI 侧有，纯浏览器侧没有
        host: HostCapability::FilePicker,
    },
    CommandSpec {
        id: "video.job.retry",
        summary: "重试一个失败的转码作业（同一条作业，不新建）",
        params: "{job_id: string}",
        args: &[Param::req("job_id", ParamType::Str, "作业 id")],
        capability: Capability::Write,
        needs_conv: false,
        scope: ScopeClass::Media,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "video.view.report",
        summary: "上报一次观看心跳（本次新增时长必须为正）",
        params: "{video_id: string, viewer: string, watched_ms: number, position_ms?: number, completed?: boolean, session_id?: string}",
        args: &[
            Param::req("video_id", ParamType::Str, "视频 id"),
            Param::req("viewer", ParamType::Str, "观众 user_id"),
            Param::req("watched_ms", ParamType::NonNegativeInt, "本次新增观看时长"),
            Param::opt("position_ms", ParamType::NonNegativeInt, "当前播放位置"),
            Param::opt("completed", ParamType::Bool, "是否看完"),
            Param::opt("session_id", ParamType::Str, "播放会话 id"),
        ],
        capability: Capability::Write,
        needs_conv: false,
        scope: ScopeClass::Media,
        confirmation: Confirmation::None,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    // ---- 破坏性 ----
    CommandSpec {
        id: "group.kick",
        summary: "把成员移出群（不可撤销）",
        params: "{conv: string, user_id: string, confirmation_token?: string}",
        args: &[
            Param::req("conv", ParamType::Str, "群 id"),
            Param::req("user_id", ParamType::Str, "被移出的 user_id"),
            Param::opt(
                "confirmation_token",
                ParamType::Str,
                "确认令牌：先 prepare，再把令牌带回来",
            ),
        ],
        capability: Capability::Destructive,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::Required,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
    },
    CommandSpec {
        id: "msg.recall",
        summary: "撤回一条消息（他人消息需管理员）",
        params: "{conv: string, msg_id: string, confirmation_token?: string}",
        args: &[
            Param::req("conv", ParamType::Str, "会话 id"),
            Param::req("msg_id", ParamType::Str, "消息 id"),
            Param::opt(
                "confirmation_token",
                ParamType::Str,
                "确认令牌：先 prepare，再把令牌带回来",
            ),
        ],
        capability: Capability::Destructive,
        needs_conv: true,
        scope: ScopeClass::Conversation,
        confirmation: Confirmation::Required,
        idempotency: Idempotency::None,
        platforms: Platforms::ALL,
        host: HostCapability::None,
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
            "wxlike/confirmation": if spec.confirmation.is_required() { "required" } else { "none" },
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
        assert_eq!(CATALOG.len(), 32, "命令数变了：快照测试要一起加 id");
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
                "friend.request.create",
                "friend.request.accept",
                "friend.request.decline",
                "video.select",
                "video.stats",
                "video.job.get",
                "video.job.create",
                "video.job.retry",
                "video.view.report",
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
