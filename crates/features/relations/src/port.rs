//! 跨域只读端口：聊天/推送/搜索从这里问关系，**不许**自己查关系表。
//!
//! 一个 trait + 一个 SQL 实现。聊天域（G03）拿到 [`RelationsPort`] 之后，
//! 关系表结构怎么变都与它无关；反过来，本 crate 也不关心消息怎么存。
//! 这条边界就是"不拷贝对方业务算法"的落地方式。

use rusqlite::Connection;

use crate::group::{self, Role};
use crate::{friend, RelError, Result};

/// 一条消息能否发给对方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageGate {
    /// 好友、或发给自己的设备（多端同步）。
    Allowed,
    /// 非好友：进"陌生人消息请求箱"，不产生会话。
    StrangerRequest,
    /// 任一方向被拉黑：直接拒绝，且不告诉发送方具体原因。
    Blocked,
}

/// 借源边界：权限位的**语义**来自 Tinode，"谁来问"这件事来自本域。
///
/// 本 trait 只做跨域只读查询；权限位怎么算出来的请看下面的 [`Acs`]。
pub trait RelationsPort {
    fn are_friends(&self, a: &str, b: &str) -> Result<bool>;
    fn is_blocked(&self, by: &str, target: &str) -> Result<bool>;
    fn may_message(&self, from: &str, to: &str) -> Result<MessageGate>;
    fn shared_groups(&self, a: &str, b: &str) -> Result<Vec<String>>;
    fn member_ids(&self, group_id: &str) -> Result<Vec<String>>;
    fn role_of(&self, group_id: &str, user: &str) -> Result<Option<Role>>;

    /// 关系域的权限位判定：`actor` 相对 `target` 手里到底有哪些位。
    ///
    /// 聊天/搜索域**不许**自己把 `friends`/`blocks` 两张表翻译成"能不能看、
    /// 能不能加"——那正是本 crate 之外会长出第二套规则的地方。这里给出唯一的
    /// 判定入口，返回值已经是 `given & want` 的累积权限。
    fn acs_toward(&self, actor: &str, target: &str) -> Result<Acs> {
        crate::friend::acs_toward(self.conn(), actor, target)
    }

    /// `actor` 能不能把 `target` 加进来（加好友 / 拉进群都问这一条）。
    fn may_add(&self, actor: &str, target: &str) -> Result<bool> {
        crate::friend::may_add(self.conn(), actor, target)
    }

    /// 借源实现的默认实现要拿连接，这里由实现者给出。
    fn conn(&self) -> &Connection;
}

/// 直接读真实关系表的实现（平台在请求作用域里构造，开销只有一次借用）。
pub struct SqlRelations<'a> {
    pub conn: &'a Connection,
}

impl RelationsPort for SqlRelations<'_> {
    fn conn(&self) -> &Connection {
        self.conn
    }

    fn are_friends(&self, a: &str, b: &str) -> Result<bool> {
        friend::are_friends(self.conn, a, b)
    }

    fn is_blocked(&self, by: &str, target: &str) -> Result<bool> {
        friend::is_blocked(self.conn, by, target)
    }

    fn may_message(&self, from: &str, to: &str) -> Result<MessageGate> {
        if friend::blocked_either(self.conn, from, to)? {
            return Ok(MessageGate::Blocked);
        }
        if from == to || friend::are_friends(self.conn, from, to)? {
            return Ok(MessageGate::Allowed);
        }
        Ok(MessageGate::StrangerRequest)
    }

    fn shared_groups(&self, a: &str, b: &str) -> Result<Vec<String>> {
        Ok(group::common_groups(self.conn, a, b)?
            .into_iter()
            .map(|g| g.id)
            .collect())
    }

    fn member_ids(&self, group_id: &str) -> Result<Vec<String>> {
        group::members(self.conn, group_id)
    }

    fn role_of(&self, group_id: &str, user: &str) -> Result<Option<Role>> {
        group::role_of(self.conn, group_id, user)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// 权限位模型（借源：Tinode `server/store/types/types.go` 的 `AccessMode`，
// commit eb90ef83acae6542f52719377ee5a76d267a5618；位值、序列化字母表、
// 预设常量与 `BetterThan/BetterEqual/Delta/ApplyDelta` 逐条对齐）。
//
// 为什么借它而不是自己发明：wxlike 要回答的是"**谁能看到谁**""**谁能加谁**"
// 这类**部分授权**问题——"能看到但不能发言""能拉人但不能改权限""被禁但还在
// 名单里"在角色枚举（owner/admin/member）里表达不了，只能表达成"位开了/关了"。
// 抄的是位的**语义与推导顺序**，不是 Go 代码本身：
//
// * 没抄：`sql.Scanner`/`driver.Valuer`/`MarshalJSON`（本平台 Acs 不直接落库
//   成 JSON，落库存的是 `MarshalText` 那个字母串）、`ModeInvalid` 的
//   "0x100000 表示出错"用法（Rust 里出错走 `Result`，`INVALID` 只作为
//   `Default::default()` 之类的兜底哨兵保留，见 [`Acs::INVALID`]）。
// * 改了：Go 的位常量 `1 << iota` 换成 `pub const JOIN: u32`；`string` 入参
//   统一 `&str`；错误统一折成 [`RelError`]，不回吐用户输入字符。
// ───────────────────────────────────────────────────────────────────────────

/// 一个位的掩码（`AccessMode` 是 8 位，高位留给哨兵）。
const BITMASK: u32 = 0xFF;

/// 关系权限位。
///
/// 语义逐字对应 Tinode 的 `ModeJoin…ModeOwner`：
///
/// | 位 | 值 | 含义（wxlike 口径） |
/// |---|---|---|
/// | `J` | 1 | 能进入这段关系：能发消息、能进群（`sub`）。关掉 = 被 ban |
/// | `R` | 2 | 能读：看得到对方发的内容/群消息/资料 |
/// | `W` | 4 | 能写：对方能看到我发出的内容 |
/// | `P` | 8 | 能收提示：未读角标、新消息提醒。关掉就是"消息免打扰" |
/// | `A` | 16 | 能批/能踢：审批入群申请、移出成员 |
/// | `S` | 32 | 能邀：**能把别人加进来**——"谁能加谁"就是这一位 |
/// | `D` | 64 | 能硬删消息 |
/// | `O` | 128 | 群主：拥有全部位，且只有他能转让 |
///
/// 两个非位状态必须与"零位"区分开，这是整套模型最容易出事的地方：
///
/// * [`Acs::NONE`]（字母 `N`）是**明确拒绝**：一行存了 `N` 就是"谁也别想"，
///   它参与判定时按"没有位"处理；
/// * [`Acs::UNSET`]（空串）是**没说过**：走类目默认值，绝不能当成 `NONE`。
///
/// 把缺省读成 `NONE` 会让所有没配过隐私规则的老用户一夜之间互相看不见；
/// 把坏值读成"全开"则相反——两者都是本 crate 明令禁止的折默认值。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Acs(u32);

impl Acs {
    pub const JOIN: u32 = 1;
    pub const READ: u32 = 1 << 1;
    pub const WRITE: u32 = 1 << 2;
    pub const PRES: u32 = 1 << 3;
    pub const APPROVE: u32 = 1 << 4;
    pub const SHARE: u32 = 1 << 5;
    pub const DELETE: u32 = 1 << 6;
    pub const OWNER: u32 = 1 << 7;

    /// 序列化顺序（`MarshalText` 里的 `JRWPASDO`）。
    const ALPHABET: [u8; 8] = *b"JRWPASDO";

    /// 明确拒绝（`ModeNone`）。
    pub const NONE: Acs = Acs(0);
    /// 没说过（`ModeUnset`）：走默认值，不参与位运算。
    pub const UNSET: Acs = Acs(1 << 8);
    /// 出错哨兵（`ModeInvalid`）。Rust 里正常路径返回 `Result`，这个值只在
    /// 需要"绝对不开权限"的兜底处出现；任何参与判定的路径都会先拒掉它。
    pub const INVALID: Acs = Acs(0x0010_0000);

    /// 普通用户对普通话题（`ModeCPublic`，"JRWPS" = 47）。
    pub const PUBLIC: Acs = Acs(Self::JOIN | Self::READ | Self::WRITE | Self::PRES | Self::SHARE);
    /// 对 `me`/`fnd` 的订阅（`ModeCMeFnd`，"JPS" = 41）。
    pub const ME_FND: Acs = Acs(Self::JOIN | Self::PRES | Self::SHARE);
    /// 对自己 `slf` 的订阅（`ModeCSelf`，"JRWDO" = 199）。
    pub const SELF_SUB: Acs =
        Acs(Self::JOIN | Self::READ | Self::WRITE | Self::DELETE | Self::OWNER);
    /// 群主对群的全部位（`ModeCFull`，"JRWPASDO" = 255）。
    pub const FULL: Acs = Acs(BITMASK);
    /// 一对一默认位（`ModeCP2P`，"JRWPA" = 31）。
    pub const P2P: Acs = Acs(Self::JOIN | Self::READ | Self::WRITE | Self::PRES | Self::APPROVE);
    /// 允许硬删消息时的一对一位（`ModeCP2PD`，95）。
    pub const P2P_DELETE: Acs =
        Acs(Self::JOIN | Self::READ | Self::WRITE | Self::PRES | Self::APPROVE | Self::DELETE);
    /// 已认证关系的默认位（`ModeCAuth`，"JRWPAS" = 63）。
    pub const AUTH: Acs = Acs(0x3F);
    /// 只读（`ModeCReadOnly`，"JR" = 3）。
    pub const READ_ONLY: Acs = Acs(Self::JOIN | Self::READ);
    /// 频道发布者：`RWPS`，**没有 J**，只能被邀（`ModeCChnWriter`，78）。
    pub const CHN_WRITER: Acs = Acs(Self::READ | Self::WRITE | Self::PRES | Self::SHARE);
    /// 频道读者（`ModeCChnReader`，"JRP" = 11）。
    pub const CHN_READER: Acs = Acs(Self::JOIN | Self::READ | Self::PRES);
    /// 管理员：能改权限的人（`ModeCAdmin` = O|A = 144）。
    pub const ADMIN: Acs = Acs(Self::OWNER | Self::APPROVE);
    /// 需要收到"权限被改了"通知的人（`ModeCSharer` = O|A|S = 176）。
    pub const SHARER: Acs = Acs(Self::OWNER | Self::APPROVE | Self::SHARE);

    /// 有效位的掩码（`ModeBitmask`）。
    pub const BITMASK: u32 = BITMASK;

    /// 字母串 → 位（`ParseAcs`）。
    ///
    /// 与上游一致的三件事：大小写都收；`N` 不能与任何位并存（"要么明确拒绝、
    /// 要么有权限，不能又要又要"）；出现表外字符直接错，**不忽略**。
    /// 空串是 `UNSET`，不是 `NONE`。
    pub fn parse(raw: &str) -> Result<Acs> {
        let mut acc: u32 = Acs::UNSET.0;
        for ch in raw.bytes() {
            match ch {
                b'J' | b'j' => acc |= Self::JOIN,
                b'R' | b'r' => acc |= Self::READ,
                b'W' | b'w' => acc |= Self::WRITE,
                b'P' | b'p' => acc |= Self::PRES,
                b'A' | b'a' => acc |= Self::APPROVE,
                b'S' | b's' => acc |= Self::SHARE,
                b'D' | b'd' => acc |= Self::DELETE,
                b'O' | b'o' => acc |= Self::OWNER,
                b'N' | b'n' => {
                    if acc != Acs::UNSET.0 {
                        // 上游原文："access N cannot be combined with any other"。
                        return Err(RelError::BadInput("access mode N cannot be combined"));
                    }
                    return Ok(Acs::NONE);
                }
                _ => return Err(RelError::BadInput("invalid access mode character")),
            }
        }
        // 上游在 `UnmarshalText` 里做 `m0 & ModeBitmask`：起始的 UNSET 标记位
        // 只是"一个字符都没见过"的记号，绝不能混进结果里，否则 `J` 会变成 0x101。
        if acc == Acs::UNSET.0 {
            Ok(Acs::UNSET)
        } else {
            Ok(Acs(acc & BITMASK))
        }
    }

    /// 位 → 字母串（`MarshalText`）。`NONE` 打 `N`，`UNSET` 打空串。
    pub fn to_flags(self) -> String {
        match self {
            Self::NONE => "N".to_string(),
            s if s.is_invalid() => String::new(),
            Acs(bits) => Self::ALPHABET
                .iter()
                .enumerate()
                .filter(|(i, _)| bits & (1 << *i as u32) != 0)
                .map(|(_, c)| *c as char)
                .collect(),
        }
    }

    /// `IsJoiner`：有 J 才能进这段关系。
    pub fn is_joiner(self) -> bool {
        self.0 & Self::JOIN != 0
    }
    /// `IsReader`。
    pub fn is_reader(self) -> bool {
        self.0 & Self::READ != 0
    }
    /// `IsWriter`。
    pub fn is_writer(self) -> bool {
        self.0 & Self::WRITE != 0
    }
    /// `IsPresencer`：还在收提示。
    pub fn is_presencer(self) -> bool {
        self.0 & Self::PRES != 0
    }
    /// `IsApprover`。
    pub fn is_approver(self) -> bool {
        self.0 & Self::APPROVE != 0
    }
    /// `IsSharer`：A 或 S 或 O——**能不能加别人**问的就是它。
    pub fn is_sharer(self) -> bool {
        self.0 & (Self::SHARE | Self::APPROVE | Self::OWNER) != 0
    }
    /// `IsDeleter`。
    pub fn is_deleter(self) -> bool {
        self.0 & Self::DELETE != 0
    }
    /// `IsOwner`。
    pub fn is_owner(self) -> bool {
        self.0 & Self::OWNER != 0
    }
    /// `IsAdmin`：O 或 A。只有他们能把非默认权限发给别人。
    pub fn is_admin(self) -> bool {
        self.is_owner() || self.is_approver()
    }
    /// `IsZero`：明确无权限（`NONE`）。
    pub fn is_zero(self) -> bool {
        self.0 == 0
    }
    /// `IsDefined`：既不是 `UNSET` 也不是 `INVALID`。`NONE` 算已定义。
    pub fn is_defined(self) -> bool {
        self != Self::UNSET && !self.is_invalid()
    }
    /// `IsInvalid`。
    pub fn is_invalid(self) -> bool {
        self == Self::INVALID
    }

    /// 原始位（`given & want` 之类的调试视图与断言用）。
    pub fn bits(self) -> u32 {
        self.0
    }

    /// 由原始位构造（只留有效位）。跨域/测试里拿一个常量位拼出 `Acs` 用这个，
    /// 别的地方一律走 [`Acs::parse`]，这样字母串始终是唯一的存储形态。
    pub const fn from_bits(bits: u32) -> Acs {
        Acs(bits & BITMASK)
    }

    /// 只留有效位（上游在 `UnmarshalText` 里做的 `& ModeBitmask`）。
    pub fn masked(self) -> Acs {
        if self.is_defined() {
            Acs(self.0 & BITMASK)
        } else {
            self
        }
    }

    /// `BetterEqual`：手里这把是否**覆盖**了想要的那把。
    pub fn better_equal(self, want: Acs) -> bool {
        BITMASK & self.0 & want.0 == want.0 & BITMASK
    }

    /// `BetterThan`：手里这把是否**多于**想要的那把（用来判断"这次改动到底动了没动"）。
    pub fn better_than(self, want: Acs) -> bool {
        BITMASK & self.0 & !want.0 != 0
    }

    /// 累积权限：**这是"谁能看到谁"的唯一算法**（上游 `modeGiven & modeWant`）。
    ///
    /// 授予方没给（`given` 关位）或索取方自己关了（`want` 关位）都算没有。
    /// 任一侧 `UNSET`/`INVALID` → 返回 `UNSET`，让调用方去走默认值：
    /// 两侧都还没说过的时候，把它折成 `NONE` 就是"默认互不可见"，
    /// 折成 `FULL` 就是"默认全可见"，两者都错。
    pub fn cumulative(given: Acs, want: Acs) -> Acs {
        if !given.is_defined() || !want.is_defined() {
            return Acs::UNSET;
        }
        Acs((given.0 & want.0) & BITMASK)
    }

    /// `given | extra`：上游 `modeGiven |= ModeJoin` 那一步（"新订阅无论如何要能进"）。
    /// `UNSET` 侧被忽略，因为"没说过"不该污染另一位说过的内容。
    pub fn union(self, other: Acs) -> Acs {
        match (self.is_defined(), other.is_defined()) {
            (true, true) => Acs((self.0 | other.0) & BITMASK),
            (true, false) => Acs(self.0 & BITMASK),
            (false, true) => Acs(other.0 & BITMASK),
            (false, false) => Acs::UNSET,
        }
    }

    /// 关掉若干位（`&^ flag`）。
    pub fn without(self, flag: u32) -> Acs {
        if self.is_defined() {
            Acs(self.0 & !flag & BITMASK)
        } else {
            self
        }
    }

    /// 打开若干位（`| flag`）。
    pub fn with(self, flag: u32) -> Acs {
        if self.is_defined() {
            Acs(self.0 | flag & BITMASK)
        } else {
            self
        }
    }

    /// 交集（`& flag`）。
    pub fn only(self, flag: u32) -> Acs {
        if self.is_defined() {
            Acs(self.0 & flag & BITMASK)
        } else {
            self
        }
    }

    /// `Delta`：两把权限之间的差，形如 `+W-PA`。审计里写这个比写两串字母有用。
    pub fn delta(self, newer: Acs) -> String {
        let mut out = String::new();
        let added = BITMASK & newer.0 & !self.0;
        let removed = BITMASK & self.0 & !newer.0;
        if added != 0 {
            out.push('+');
            out.push_str(&Acs(added).to_flags());
        }
        if removed != 0 {
            out.push('-');
            out.push_str(&Acs(removed).to_flags());
        }
        out
    }

    /// `ApplyMutation`：含 `+`/`-` 就按增量施加，否则整串赋值。空串是"不改"。
    pub fn apply_mutation(&mut self, mutation: &str) -> Result<()> {
        if mutation.is_empty() {
            return Ok(());
        }
        if mutation.contains(['+', '-']) {
            self.apply_delta(mutation)?;
        } else {
            let parsed = Acs::parse(mutation)?;
            if parsed.is_defined() {
                *self = parsed.masked();
            }
        }
        Ok(())
    }

    /// `ApplyDelta`：`JPRA.ApplyDelta("-PR+W") -> JWA`。
    ///
    /// 上游那套手写扫描的坑在于"符号前没有字符"（`"-P"` 这种）：这里显式拒绝，
    /// 因为静默当成"首段是加"会把一次收窄权限的意图读成放大权限。
    fn apply_delta(&mut self, delta: &str) -> Result<()> {
        if delta == "N" {
            return Ok(());
        }
        let bytes = delta.as_bytes();
        if !matches!(bytes.first(), Some(b'+') | Some(b'-')) {
            return Err(RelError::BadInput("access mode delta must start with + or -"));
        }
        let mut acc = *self;
        let mut i = 0usize;
        while i < bytes.len() {
            let sign = bytes[i];
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] != b'+' && bytes[j] != b'-' {
                j += 1;
            }
            let chunk = &delta[i + 1..j];
            let upd = Acs::parse(chunk)?;
            match sign {
                b'+' => acc = acc.union(upd),
                b'-' => {
                    if upd.is_defined() {
                        acc = Acs(acc.0 & !upd.0 & BITMASK);
                    }
                }
                _ => return Err(RelError::BadInput("access mode delta sign")),
            }
            i = j;
        }
        *self = acc;
        Ok(())
    }

    /// 上游 `ParseAcs` 的 `+` 段对 `UNSET` 特判：`""` 加空不改变任何东西。
    /// 这个 `Option` 版本给"调用方可能压根没传 mode 字段"的接口用。
    pub fn parse_optional(raw: Option<&str>) -> Result<Option<Acs>> {
        match raw {
            None => Ok(None),
            Some("") => Ok(Some(Acs::UNSET)),
            Some(s) => Ok(Some(Acs::parse(s)?)),
        }
    }
}

impl Default for Acs {
    fn default() -> Self {
        Acs::UNSET
    }
}

impl core::fmt::Debug for Acs {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let tag = if *self == Acs::UNSET {
            "unset"
        } else if self.is_invalid() {
            "invalid"
        } else {
            "acs"
        };
        write!(f, "Acs({tag}:{})", self.to_flags())
    }
}

impl core::fmt::Display for Acs {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_flags())
    }
}

/// "谁来问"：认证级别（借 `server/auth/auth.go` 的 `LevelNone/LevelAnon/LevelAuth/LevelRoot`）。
///
/// 顺序即强弱（`derive(PartialOrd, Ord)` 对应上游 `iota * 10` 的递增），
/// [`select_access_mode`] 就是按它挑一把默认位。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RelationLevel {
    /// 完全不认识：连账号都无法解析。
    None,
    /// 陌生人：没有好友关系、也没有共同群。
    Anon,
    /// 已有关系：好友，或至少同在一个群里（"好友的好友"这一档）。
    Auth,
    /// 平台自身/系统任务。
    Root,
}

impl RelationLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Anon => "anon",
            Self::Auth => "auth",
            Self::Root => "root",
        }
    }

    /// `ParseAuthLevel`：认不出来就是 `None`，不猜。
    pub fn parse(raw: &str) -> Self {
        match raw {
            "anon" | "ANON" => Self::Anon,
            "auth" | "AUTH" => Self::Auth,
            "root" | "ROOT" => Self::Root,
            _ => Self::None,
        }
    }
}

/// `selectAccessMode`：按级别从三把默认位里挑一把。
///
/// 上游那张 `switch` 的 `default` 分支返回 `ModeNone`——照抄，包括它的安全含义：
/// 级别认不出来时**一律无权**，而不是折成"最宽松的那一档"。
pub fn select_access_mode(level: RelationLevel, anon: Acs, auth: Acs, root: Acs) -> Acs {
    match level {
        RelationLevel::None => Acs::NONE,
        RelationLevel::Anon => anon,
        RelationLevel::Auth => auth,
        RelationLevel::Root => root,
    }
}

/// 关系类目（借 Tinode 的话题类目：`p2p`/`grp`/`me`/`fnd`/`slf`）。
///
/// wxlike 没有"话题"这个概念，但**默认权限必须按关系形状分档**：一对一和群
/// 的默认位不一样（群默认不给 A/S，一对一默认给 A 因为两边平等）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationCat {
    /// 一对一（好友/单聊）。
    P2p,
    /// 群。`is_channel` 由调用方另传，对应上游的频道档。
    Grp,
    /// "我"这条关系线（好友列表本身）。
    Me,
    /// 查找/添加入口（陌生人搜索）。
    Fnd,
    /// 对自己的订阅（资料页）。
    Slf,
}

/// `getDefaultAccess`：类目 → 该类目下"已认证用户"的默认位。
///
/// 上游 `fnd` 返回 `ModeNone`（搜到不等于能加），wxlike 这里**故意不同**：
/// 平台的产品口径是"陌生人默认可以发申请，能不能收到由对方的 `S` 位决定"，
/// 拒绝权在**被加方**（`friend_policy`），不在类目默认值里。这一条差异写在
/// `third_party/reviews/patches/UP-TINODE-relations-acs.md` 里，别当成抄漏。
pub fn default_access(cat: RelationCat, auth_user: bool, is_channel: bool) -> Acs {
    if !auth_user {
        return Acs::NONE;
    }
    match cat {
        RelationCat::P2p => Acs::P2P,
        RelationCat::Fnd => Acs::PUBLIC,
        RelationCat::Grp => {
            if is_channel {
                Acs::CHN_WRITER
            } else {
                Acs::PUBLIC
            }
        }
        RelationCat::Me => Acs::ME_FND,
        RelationCat::Slf => Acs::SELF_SUB,
    }
}

/// `types.DefaultAccess`：一个容器对两类访问者各自缺省给什么位。
///
/// 群与好友策略都存成这一对（`access_auth` / `access_anon`），所以"谁能加我"
/// 与"谁能看见我"是**同一把位**上的不同位，不需要再发明一套枚举。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefaultAcs {
    /// 已有关系者（[`RelationLevel::Auth`]）。
    pub auth: Acs,
    /// 陌生人（[`RelationLevel::Anon`]）。
    pub anon: Acs,
}

impl DefaultAcs {
    /// 平台缺省：两边都按"公开"给。
    ///
    /// 这不是随手挑的宽松值——`friends`/`groups` 这些表在 G02 之前就没有权限列，
    /// 老数据一律只能按缺省解释；把缺省读成"关"会让所有存量关系一夜失效
    /// （与 `group.rs` 里"缺 `group_meta` 行按公开处理"是同一条判断）。
    /// 收紧必须由用户显式写一次 mutation。
    pub const PLATFORM_DEFAULT: DefaultAcs = DefaultAcs {
        auth: Acs::AUTH,
        anon: Acs::PUBLIC,
    };

    pub fn select(self, level: RelationLevel) -> Acs {
        select_access_mode(level, self.anon, self.auth, self.auth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::{self, Visibility};
    use crate::testing::db;

    #[test]
    fn gate_allows_friends_and_blocks_blocked() {
        let conn = db();
        let port = SqlRelations { conn: &conn };
        assert_eq!(
            port.may_message("u1", "u2").unwrap(),
            MessageGate::StrangerRequest
        );
        friend::send(&conn, "u1", "u2", "", 1_000).unwrap();
        friend::send(&conn, "u2", "u1", "", 1_100).unwrap();
        assert_eq!(port.may_message("u1", "u2").unwrap(), MessageGate::Allowed);
        assert_eq!(port.may_message("u1", "u1").unwrap(), MessageGate::Allowed);
        friend::block(&conn, "u2", "u1", 1_200).unwrap();
        assert_eq!(port.may_message("u1", "u2").unwrap(), MessageGate::Blocked);
    }

    #[test]
    fn shared_groups_and_membership_via_port() {
        let conn = db();
        let g = group::create(
            &conn,
            "u1",
            "群",
            &["u2".to_string()],
            Visibility::Public,
            1_000,
        )
        .unwrap();
        let port = SqlRelations { conn: &conn };
        assert_eq!(port.shared_groups("u1", "u2").unwrap(), vec![g.id.clone()]);
        assert_eq!(port.member_ids(&g.id).unwrap().len(), 2);
        assert_eq!(port.role_of(&g.id, "u1").unwrap(), Some(Role::Owner));
        assert!(port.shared_groups("u1", "u9").unwrap().is_empty());
    }
}

#[cfg(test)]
mod acs_tests {
    use super::{Acs, DefaultAcs, RelationCat, RelationLevel};

    #[test]
    fn bit_values_and_letters_match_upstream() {
        // 上游常量表的数值就是这份接口的对外契约，改一位就会让存量数据改判。
        assert_eq!(Acs::JOIN, 1);
        assert_eq!(Acs::READ, 2);
        assert_eq!(Acs::WRITE, 4);
        assert_eq!(Acs::PRES, 8);
        assert_eq!(Acs::APPROVE, 16);
        assert_eq!(Acs::SHARE, 32);
        assert_eq!(Acs::DELETE, 64);
        assert_eq!(Acs::OWNER, 128);
        assert_eq!(Acs::PUBLIC.bits(), 47, "JRWPS");
        assert_eq!(Acs::ME_FND.bits(), 41, "JPS");
        assert_eq!(Acs::SELF_SUB.bits(), 199, "JRWDO");
        assert_eq!(Acs::FULL.bits(), 255, "JRWPASDO");
        assert_eq!(Acs::P2P.bits(), 31, "JRWPA");
        assert_eq!(Acs::P2P_DELETE.bits(), 95, "JRWPAD");
        assert_eq!(Acs::AUTH.bits(), 63, "JRWPAS");
        assert_eq!(Acs::READ_ONLY.bits(), 3, "JR");
        assert_eq!(Acs::CHN_WRITER.bits(), 46, "RWPS：上游注释写的是 78/RWPD，代码是 SHARE，以代码为准");
        assert_eq!(Acs::CHN_READER.bits(), 11, "JRP");
        assert_eq!(Acs::ADMIN.bits(), 144, "OA");
        assert_eq!(Acs::SHARER.bits(), 176, "OAS");
        assert_eq!(Acs::parse("JRWPASDO").unwrap(), Acs::FULL);
        assert_eq!(Acs::FULL.to_flags(), "JRWPASDO");
    }

    #[test]
    fn unset_none_and_invalid_are_three_different_things() {
        assert_eq!(Acs::parse("").unwrap(), Acs::UNSET);
        assert_eq!(Acs::parse("N").unwrap(), Acs::NONE);
        assert!(!Acs::UNSET.is_defined());
        assert!(Acs::NONE.is_defined(), "N 是明确拒绝，属于已定义");
        assert_eq!(Acs::UNSET.to_flags(), "");
        assert_eq!(Acs::NONE.to_flags(), "N");
        assert_eq!(Acs::INVALID.to_flags(), "");
        assert!(Acs::INVALID.is_invalid());
        // N 不能与别的位并存（上游同一条错误）。
        assert!(Acs::parse("JN").is_err());
        assert!(Acs::parse("X").is_err());
        // 大小写都收，与上游 switch 一致。
        assert_eq!(Acs::parse("jrwpa").unwrap(), Acs::P2P);
    }

    #[test]
    fn cumulative_is_the_only_way_to_ask_who_sees_whom() {
        let given = Acs::P2P;
        let want = Acs::parse("JRP").unwrap();
        let acc = Acs::cumulative(given, want);
        assert!(acc.is_reader() && acc.is_joiner() && acc.is_presencer());
        assert!(!acc.is_writer(), "自己关了 W 就读成不能写");
        // 任一侧没说过 → UNSET，绝不折成"全可见/全不可见"。
        assert_eq!(Acs::cumulative(Acs::UNSET, Acs::FULL), Acs::UNSET);
        assert_eq!(Acs::cumulative(Acs::FULL, Acs::NONE), Acs::NONE);
        assert!(Acs::NONE.better_equal(Acs::NONE));
        assert!(!Acs::NONE.better_equal(Acs::from_bits(Acs::READ)));
        assert!(Acs::FULL.better_than(Acs::READ_ONLY));
        assert!(!Acs::READ_ONLY.better_than(Acs::FULL));
    }

    #[test]
    fn delta_and_mutation_round_trip() {
        // 上游注释里的例子：JPRA.ApplyDelta(-PR+W) -> JWA。
        let mut m = Acs::parse("JPRA").unwrap();
        m.apply_mutation("-PR+W").unwrap();
        assert_eq!(m, Acs::parse("JWA").unwrap());
        let old = Acs::parse("JRWP").unwrap();
        let new = Acs::parse("JRWPA").unwrap();
        assert_eq!(old.delta(new), "+A");
        assert_eq!(new.delta(old), "-A");
        // 空 mutation 不改东西。
        let mut keep = old;
        keep.apply_mutation("").unwrap();
        assert_eq!(keep, old);
        // 上游对 "N" 有**两条不同的路**，这里一条不落：
        // `ApplyDelta("N")` 早退，是 no-op；`UnmarshalText("N")`（不带 +/- 的
        // 整串赋值）把权限**清空**，是明确撤销。读成一样就会把"撤销"当"没改"。
        let mut via_delta = old;
        via_delta.apply_mutation("-N").ok();
        let mut revoke = old;
        revoke.apply_mutation("N").unwrap();
        assert_eq!(revoke, Acs::NONE);
        let mut noop = old;
        noop.apply_mutation("N").ok();
        assert_eq!(noop, Acs::NONE, "同一条：N 是赋值不是增量");
        // 缺符号前缀的增量必须报错，不能静默当"加"。
        let mut full = Acs::FULL;
        assert!(full.apply_mutation("W").is_ok(), "不带符号 = 整串赋值");
        assert!(full.apply_mutation("P+W").is_err());
    }

    #[test]
    fn level_select_is_fail_closed() {
        assert_eq!(Acs::PUBLIC, Acs::parse("JRWPS").unwrap());
        use super::select_access_mode;
        assert_eq!(select_access_mode(RelationLevel::None, Acs::PUBLIC, Acs::AUTH, Acs::FULL), Acs::NONE);
        assert_eq!(select_access_mode(RelationLevel::Anon, Acs::PUBLIC, Acs::AUTH, Acs::FULL), Acs::PUBLIC);
        assert_eq!(select_access_mode(RelationLevel::Auth, Acs::PUBLIC, Acs::AUTH, Acs::FULL), Acs::AUTH);
        assert_eq!(select_access_mode(RelationLevel::Root, Acs::PUBLIC, Acs::AUTH, Acs::FULL), Acs::FULL);
        assert!(RelationLevel::Auth > RelationLevel::Anon);
        assert_eq!(RelationLevel::parse("anon"), RelationLevel::Anon);
        assert_eq!(RelationLevel::parse("别的"), RelationLevel::None);
    }

    #[test]
    fn category_defaults_and_platform_default_policy() {
        assert_eq!(super::default_access(RelationCat::P2p, true, false), Acs::P2P);
        assert_eq!(super::default_access(RelationCat::Grp, true, false), Acs::PUBLIC);
        assert_eq!(super::default_access(RelationCat::Grp, true, true), Acs::CHN_WRITER);
        assert_eq!(super::default_access(RelationCat::P2p, false, false), Acs::NONE);
        let d = DefaultAcs::PLATFORM_DEFAULT;
        assert!(d.select(RelationLevel::Anon).is_sharer(), "陌生人能不能加由 S 位决定");
        assert!(d.select(RelationLevel::None).is_zero(), "级别都认不出来时一律无权");
    }
}
