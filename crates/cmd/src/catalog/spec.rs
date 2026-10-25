//! 命令定义的类型：一条命令"是什么"，不包含"怎么执行"。
//!
//! 这里的每个字段都对应一个**外部契约**：MCP 客户端会把 `id` 存下来，
//! 脚本会依赖退出码与 JSON 形状，AI 会读 schema 决定参数。所以：
//! 字段只增不改，`id` 改名等于破坏兼容。

use std::fmt;

/// 命令的危险等级。默认只给 `Read`，写要显式开，破坏性再开一层。
///
/// **这是全仓库唯一的一份**。`wxlike-admin` 的 RBAC 直接复用这个类型，
/// 不再自己定义一遍——权限模型有两个定义，就一定会出现"两边对破坏性
/// 的理解不一样"而没人发现。序列化统一成 `read` / `write` / `destructive`，
/// 与 `as_str()` 一字不差（有测试钉住）。
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
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

/// 资源类别：服务端与 MCP 侧按这个类别做**第二次**授权。
///
/// `Conversation` 之外的四类不是为了好看——它们决定"授权对象是谁"：
/// 账户类看 actor 自己，内容类看内容归属，媒体类看对象存储前缀，
/// 支付类看账本账户，管理类看后台角色。把这些混成一个 "resource" 字符串，
/// 就会有人拿会话白名单去放行管理动作。
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum ScopeClass {
    /// 当前登录主体自己（`me.*`）。
    Account,
    /// 会话/消息/群/频道成员关系（`conv.*`、`msg.*`、`group.*`）。
    Conversation,
    /// 社区、帖子、频道内容（`community.*`）。
    Content,
    /// 视频/文件/对象存储（`video.*`）。
    Media,
    /// 账本与资金（钱包、转账、红包）。G15 目前没有可自动化的支付命令。
    Payment,
    /// 管理后台动作（审核、封禁、对账）。普通 agent 一律不可调用。
    Admin,
}

impl ScopeClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ScopeClass::Account => "account",
            ScopeClass::Conversation => "conversation",
            ScopeClass::Content => "content",
            ScopeClass::Media => "media",
            ScopeClass::Payment => "payment",
            ScopeClass::Admin => "admin",
        }
    }
}

impl fmt::Display for ScopeClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 是否需要人工确认（prepare → confirmation_token → execute）。
///
/// 等级写进 catalog 而不是散在 handler 里：一句话能回答"哪些动作要先问人"。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confirmation {
    /// 不需要确认。
    None,
    /// 必须带一次性确认令牌；参数一改就要重新确认。
    Required,
}

impl Confirmation {
    pub fn is_required(self) -> bool {
        matches!(self, Confirmation::Required)
    }
}

/// 写操作是否必须带 `idempotency_key`。
///
/// 与 `Capability` 分开：`conv.ack` 是写但天然幂等（推游标），
/// `msg.send` 是写且重试会变成两条消息——两者都要"写"权限，风险不同。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Idempotency {
    /// 天然幂等或只读。
    None,
    /// 必须由调用方给稳定键，重试落在同一个业务结果上。
    Required,
}

impl Idempotency {
    pub fn is_required(self) -> bool {
        matches!(self, Idempotency::Required)
    }
}

/// 表面能力：这条命令在哪些客户端上能被调用。
///
/// 用位掩码而不是 `Vec`：`CommandSpec` 要能 `Copy`，且判断必须零分配。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Platforms(u8);

impl Platforms {
    pub const DESKTOP: Platforms = Platforms(0b0001);
    pub const WEB: Platforms = Platforms(0b0010);
    pub const ANDROID: Platforms = Platforms(0b0100);
    pub const IOS: Platforms = Platforms(0b1000);
    /// CLI / MCP / 服务端：协议面本身无平台差异。
    pub const ALL: Platforms = Platforms(0b1111);

    pub fn contains(self, other: Platforms) -> bool {
        self.0 & other.0 == other.0
    }

    /// 给人和 AI 看的平台名，顺序固定（测试按字面比较）。
    pub fn names(self) -> Vec<&'static str> {
        [
            (Platforms::DESKTOP, "desktop"),
            (Platforms::WEB, "web"),
            (Platforms::ANDROID, "android"),
            (Platforms::IOS, "ios"),
        ]
        .into_iter()
        .filter(|(p, _)| self.contains(*p))
        .map(|(_, name)| name)
        .collect()
    }
}

/// 需要宿主物理能力时，明确标出来——**不伪造远程硬件**。
///
/// 拍照、扫码这类能力在 CLI/MCP 里只能"交一个已采集的文件"或"让宿主确认"，
/// 不能凭空给模型一个 `camera.read` 工具。catalog 里没有这类命令，
/// 这个枚举是为 G15-03 的覆盖报告留的口径：哪些入口是**人机物理例外**。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCapability {
    /// 纯远程，无宿主能力需求。
    None,
    /// 需要相机（拍照/录像）。
    Camera,
    /// 需要扫码（摄像头或本机解码）。
    Scanner,
    /// 需要本机文件选择器。
    FilePicker,
}

impl HostCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            HostCapability::None => "none",
            HostCapability::Camera => "camera",
            HostCapability::Scanner => "scanner",
            HostCapability::FilePicker => "file_picker",
        }
    }
}

/// 参数类型。**只支持 JSON 里真正常用的四种**：字符串、整数、布尔、
/// 字符串数组。多一种类型就多一处"schema 说支持、代码没处理"的裂缝。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamType {
    Str,
    /// 整数，可为负。
    Int,
    /// 非负整数（条数、序列号、时长）。
    NonNegativeInt,
    Bool,
    StrList,
}

impl ParamType {
    pub fn json_type(self) -> &'static str {
        match self {
            ParamType::Str => "string",
            ParamType::Int | ParamType::NonNegativeInt => "integer",
            ParamType::Bool => "boolean",
            ParamType::StrList => "array",
        }
    }

    /// 这个 JSON 值是否符合类型。整数用 `as_i64` 判断而不是 `as_u64`：
    /// `1.5` 和 `"1"` 都必须被拒，否则"CLI 与 UI 同参数同结果"就不成立。
    pub fn accepts(self, v: &serde_json::Value) -> bool {
        match self {
            ParamType::Str => v.is_string(),
            ParamType::Int => v.as_i64().is_some_and(|_| v.is_number()),
            ParamType::NonNegativeInt => v.as_u64().is_some(),
            ParamType::Bool => v.is_boolean(),
            ParamType::StrList => v
                .as_array()
                .is_some_and(|items| items.iter().all(serde_json::Value::is_string)),
        }
    }
}

/// 一个参数。`note` 进 JSON Schema 的 `description`，也是 CLI 帮助的一列。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: &'static str,
    pub ty: ParamType,
    pub required: bool,
    pub note: &'static str,
}

impl Param {
    pub const fn req(name: &'static str, ty: ParamType, note: &'static str) -> Self {
        Self {
            name,
            ty,
            required: true,
            note,
        }
    }

    pub const fn opt(name: &'static str, ty: ParamType, note: &'static str) -> Self {
        Self {
            name,
            ty,
            required: false,
            note,
        }
    }
}

/// 一条命令的定义。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    /// `域.动作`，全局唯一，**改名等于破坏兼容**；同时也是 action_id。
    pub id: &'static str,
    /// 一句话说明（给人和 AI 看，进 `list_commands` / MCP `description`）。
    pub summary: &'static str,
    /// 人类可读的参数说明（CLI `--help` 与 MCP 描述里的一段话）。
    /// 机器可读的那份由 `args` 生成——**同一份声明**，不手抄两遍。
    pub params: &'static str,
    /// 参数声明。JSON Schema 与参数校验都从这里长出来。
    pub args: &'static [Param],
    pub capability: Capability,
    /// 是否必须带 `conv` 参数（决定权限检查时要不要比对会话白名单）。
    pub needs_conv: bool,
    pub scope: ScopeClass,
    pub confirmation: Confirmation,
    pub idempotency: Idempotency,
    pub platforms: Platforms,
    pub host: HostCapability,
}

impl CommandSpec {
    /// 稳定的 action_id。**就是 `id`**：两个名字迟早会漂移，
    /// 所以这里只做一层显式声明，不引入第二套编号。
    pub fn action_id(&self) -> &'static str {
        self.id
    }

    /// MCP 工具名：`.` 换成 `-`（MCP 的 name 只保证 `[a-zA-Z0-9_-]`）。
    ///
    /// 为什么是 `-` 而不是 `_`：id 里本来就有下划线（`group.add_member`），
    /// 换成 `_` 会让 `a.b_c` 与 `a_b.c` 撞成同一个工具名，而工具名是
    /// 模型调用时的**唯一键**。`-` 在 id 里不出现，所以这个映射是单射
    /// （`catalog_is_well_formed` 钉住"id 不含 `-`"）。
    pub fn tool_name(&self) -> String {
        self.id.replace('.', "-")
    }

    /// 协议里的资源 URI（`resources/read` 与审计日志都用它）。
    pub fn resource_uri(&self) -> String {
        format!("{}{}", crate::catalog::ACTION_URI_PREFIX, self.id)
    }

    /// `wxlike <domain> <verb>` 的两段：域名 + 剩余动作。
    /// `video.job.get` → (`video`, `job.get`)：域名只有一层，动作可以带点。
    pub fn cli_path(&self) -> (&'static str, &'static str) {
        match self.id.split_once('.') {
            Some((domain, verb)) => (domain, verb),
            // catalog 里每条 id 都含 '.'，由 `catalog_is_well_formed` 守着
            None => (self.id, ""),
        }
    }
}
