//! **contract-v1 基线冻结**（#314）。
//!
//! 这个文件是"冻结"的**机器可读形态**：基线数字、命令 id 全集、覆盖表统计，
//! 全部写成断言。想动基线的人必须先动这里，改动会立刻在 CI 里显形。
//!
//! ## 基线数字的出处
//! issue 正文与评论都列了数字，**以评论为准**（正文是初稿）：
//! * `https://github.com/bear20252026/wxlike/issues/314#issuecomment-6459035844`
//!   的更正评论逐条对 main 核实："26 条命令、64 条覆盖表 26/33/5、元工具 4 vs 3、
//!   params-DTO 无自动比对，四条全对"；本文件就是照着这四个数写的。
//! * 最后一条评论（副作用更正）把验收口径改成
//!   "v1 基线 = 原 26 条 + 本卡纳入的 N 条，automated / blocked / host_only 按新基线重算"。
//!   **本卡的边界禁止加新命令**（钱包 / 账户 / 媒体的候选 id 由 G00-02 定名、
//!   并要先过 #315 的幂等与确认复核），所以 N = 0：
//!   这里冻的就是"原 26 条 + 64 条入口"。扩大基线时把下面三个常量一起改，
//!   并按该评论第 4 条同步 `#322` 总览与 Wiki《开发顺序清单》。
//!
//! ## 冻结后规则（写在代码里，免得只活在评论里）
//! 1. `id` 不改名、不删除（`CommandSpec` 的注释同一条：改名 = 破坏兼容）；
//! 2. 字段只增不改语义；
//! 3. 新增 id 只走 v1.x，且必须同步登记 `crates/cmd/data/domains.json`
//!    ——`domains::tests::every_command_is_mapped_exactly_once` 已经在守这一条。

use std::collections::BTreeMap;

use wxlike_cmd::catalog;
use wxlike_cmd::domains::{entries, EntryState};

/// contract-v1 冻结的命令数（评论核实的第 1 条）。
const BASELINE_COMMANDS: usize = 26;
/// contract-v1 冻结的入口条数（评论核实的第 2 条）。
const BASELINE_ENTRIES: usize = 64;
/// 覆盖表的三态基线：automated / blocked / host_only（评论核实的第 2 条）。
const BASELINE_AUTOMATED: usize = 26;
const BASELINE_BLOCKED: usize = 33;
const BASELINE_HOST_ONLY: usize = 5;
/// 业务域个数（`domains.json` 的 domains 数组）。
const BASELINE_DOMAINS: usize = 8;

/// 命令 id 全集**与顺序**。catalog 顺序也是输出顺序（CLI 帮助、`tools/list`
/// 都按它排），所以顺序一并钉住。
const BASELINE_IDS: [&str; BASELINE_COMMANDS] = [
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
    "video.select",
    "video.stats",
    "video.job.get",
    "video.job.create",
    "video.job.retry",
    "video.view.report",
    "group.kick",
    "msg.recall",
];

/// 按业务域分组的 id 清单：#316/#317/#318/#319 各自独占一组时，
/// 新增落点与"这一组现在有几条"先看这里。
fn ids_by_domain() -> BTreeMap<&'static str, Vec<&'static str>> {
    let mut map: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for spec in catalog::CATALOG {
        map.entry(spec.cli_path().0).or_default().push(spec.id);
    }
    map
}

#[test]
fn contract_v1_command_count_is_frozen() {
    assert_eq!(
        catalog::CATALOG.len(),
        BASELINE_COMMANDS,
        "基线命令数变了：加命令属 v1.x（走 #315 复核 + 同步 domains.json），\
         删/改名属 v2，都不许悄悄做"
    );
}

#[test]
fn contract_v1_ids_are_frozen_verbatim() {
    let ids: Vec<&str> = catalog::CATALOG.iter().map(|s| s.id).collect();
    assert_eq!(ids, BASELINE_IDS.to_vec(), "id 是外部契约：改名即破坏兼容");
}

/// id 的三个派生名字一起钉住：MCP 工具名、action_id、资源 URI。
/// 它们各有一处消费方（MCP 客户端缓存工具名 / 审计与契约 / 资源面），
/// 只钉 id 的话，改映射规则也能把外部契约换掉。
#[test]
fn contract_v1_derived_names_are_frozen() {
    for id in BASELINE_IDS {
        let spec = catalog::get(id).unwrap_or_else(|| panic!("catalog 少了 {id}"));
        assert_eq!(spec.action_id(), id, "{id} 的 action_id 必须就是 id");
        assert_eq!(
            spec.tool_name(),
            id.replace('.', "-"),
            "{id} 的 MCP 工具名规则：`.` → `-`"
        );
        assert_eq!(
            spec.resource_uri(),
            format!("wxlike://actions/{id}"),
            "{id} 的资源 URI"
        );
    }
    assert!(
        BASELINE_IDS.iter().all(|id| !id.contains('-')),
        "id 里出现 `-` 会让工具名映射不再单射"
    );
}

#[test]
fn contract_v1_coverage_table_is_frozen() {
    let table = wxlike_cmd::domains::table();
    assert_eq!(table.domains.len(), BASELINE_DOMAINS, "业务域个数");
    assert_eq!(entries().len(), BASELINE_ENTRIES, "入口条数");

    let mut automated = 0;
    let mut blocked = 0;
    let mut host_only = 0;
    for entry in entries() {
        match &entry.state {
            EntryState::Command(_) => automated += 1,
            EntryState::Blocked { .. } => blocked += 1,
            EntryState::Host(_) => host_only += 1,
        }
    }
    assert_eq!(automated, BASELINE_AUTOMATED, "automated 条数");
    assert_eq!(blocked, BASELINE_BLOCKED, "blocked 条数");
    assert_eq!(host_only, BASELINE_HOST_ONLY, "host_only 条数");
    assert_eq!(
        automated + blocked + host_only,
        entries().len(),
        "三态之和必须等于入口总数"
    );
}

/// `summary()` 是给 `wxlike coverage` / 自查页 / 文档读的那一份，
/// 必须与手工点数一致——评论里专门警告过"汇总数字按实际翻转重算，
/// 不得直接写 54 / 30 / 5"，所以这里把两处绑在一起。
#[test]
fn contract_v1_summary_agrees_with_the_table() {
    let s = wxlike_cmd::domains::summary();
    assert_eq!(s["commands"], BASELINE_COMMANDS, "{s}");
    assert_eq!(s["entries"], BASELINE_ENTRIES, "{s}");
    assert_eq!(s["automated"], BASELINE_AUTOMATED, "{s}");
    assert_eq!(s["blocked"], BASELINE_BLOCKED, "{s}");
    assert_eq!(s["host_only"], BASELINE_HOST_ONLY, "{s}");
    assert_eq!(s["domains"], BASELINE_DOMAINS, "{s}");
}

/// 逐域的入口分布：冻结的不只是总数，还有"哪个域有几条"。
/// 翻一条 blocked 成 automated 时这里会指出是哪个域动了。
#[test]
fn contract_v1_entries_per_domain_are_frozen() {
    let want: &[(&str, usize)] = &[
        ("admin", 5),
        ("content", 7),
        ("media", 10),
        ("messaging", 19),
        ("miniprogram", 4),
        ("payment", 5),
        ("relations", 10),
        ("service", 4),
    ];
    let mut got: BTreeMap<&str, usize> = BTreeMap::new();
    for entry in entries() {
        *got.entry(entry.domain.as_str()).or_default() += 1;
    }
    let pairs: Vec<(&str, usize)> = got.iter().map(|(k, v)| (*k, *v)).collect();
    assert_eq!(pairs, want.to_vec(), "逐域入口数：{pairs:?}");
}

/// 逐域的 catalog 命令数（后续四张卡各占一域时，这是各自的落点与基数）。
#[test]
fn contract_v1_commands_per_domain_are_frozen() {
    let want: &[(&str, usize)] = &[
        ("channel", 1),
        ("community", 1),
        ("conv", 5),
        ("group", 5),
        ("me", 2),
        ("msg", 6),
        ("video", 6),
    ];
    let pairs: Vec<(&str, usize)> = ids_by_domain().iter().map(|(k, v)| (*k, v.len())).collect();
    assert_eq!(pairs, want.to_vec(), "逐域命令数：{pairs:?}");
}

/// 元工具口径（#314 第 3 条）：以代码为准是 **4 个**，不是 3 个。
/// 证据：`crates/mcp/src/server.rs:38` 的
/// `const META_TOOLS: [&str; 4] = ["im_list_commands", "im_execute", "im_inspect", "im_jobs"]`，
/// 另有 `crates/mcp/tests/stdio_surface.rs:54` 断言 `tools/list` 共 30 = 26 + 4。
/// 这里守的是**命令面那 26 条**：`tools/list` = 元工具 + catalog 全量，
/// 所以 catalog 一变，MCP 侧的快照测试会一起要改。
#[test]
fn contract_v1_tool_surface_is_commands_plus_meta() {
    const META_TOOLS: [&str; 4] = ["im_list_commands", "im_execute", "im_inspect", "im_jobs"];
    let surface = META_TOOLS.len() + catalog::CATALOG.len();
    assert_eq!(surface, 30, "tools/list 的面：26 条命令 + 4 个元工具");
    assert_eq!(
        catalog::CATALOG.len(),
        BASELINE_COMMANDS,
        "命令面变了就得同步 crates/mcp/tests/stdio_surface.rs 的 30"
    );
}

/// **逐字节冻结生成面**：把 26 条命令的 MCP 工具定义（含 JSON Schema）、
/// CLI 帮助、覆盖表报告串成一段文本，钉一个 SHA-256。
///
/// 为什么钉摘要而不是钉文件：这四个产物都是**运行时从 catalog 生成**的，
/// 没有落盘的快照可比。`serde_json::Map` 不带 `preserve_order`，键序按
/// 字典序排，所以同一份声明永远生成同一串字节。
///
/// 这张摘要就是 #314 第 4 条（catalog / domains 按域拆文件）的验收门：
/// **拆完必须还是这一串字节**，变了就说明拆分动了行为。
/// 真的要动基线（加命令 / 改参数 / 改说明）时，这里是唯一的改点，
/// 改它等于对外宣告 contract-v1 的表面变了——评审躲不过去。
#[test]
fn contract_v1_generated_surface_digest_is_frozen() {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    for spec in catalog::CATALOG {
        hasher.update(spec.id.as_bytes());
        hasher.update(b"\n");
        hasher.update(catalog::tool_definition(spec).to_string().as_bytes());
        hasher.update(b"\n");
        hasher.update(catalog::schema_of(spec).to_string().as_bytes());
        hasher.update(b"\n");
    }
    hasher.update(catalog::cli_help(None).as_bytes());
    hasher.update(b"\n");
    for domain in catalog::domains() {
        hasher.update(catalog::cli_help(Some(domain)).as_bytes());
        hasher.update(b"\n");
    }
    hasher.update(wxlike_cmd::domains::report().to_string().as_bytes());
    hasher.update(b"\n");
    hasher.update(wxlike_cmd::domains::render().as_bytes());
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        digest, GENERATED_SURFACE_SHA256,
        "生成面（MCP 工具定义 / JSON Schema / CLI 帮助 / 覆盖表报告）变了。\
         拆分文件不该动它；动了就是 contract-v1 的表面变了"
    );
}

/// `contract_v1_generated_surface_digest_is_frozen` 的基线摘要。
/// 来历：#314 拆分前用同一算法实测得到，拆后逐字节不变（`8a871793…`）。
/// **本次翻新（#315，逐条理由）**——表面相对 `8a871793…` 动了 4 处，全是
/// "声明与代码不一致"的撤销/接线，没有新增命令：
/// 1. `group.members`：`after_user`/`limit` 从装饰变成**真接线的游标翻页**
///    （执行器 `members_page` 切页），summary 与参数说明随之改口径；
/// 2. `msg.get`：撤销 `confirmation_token` 参数——读命令的令牌根本签发不出来，
///    留着等于骗调用方"要先确认"（见 catalog.rs 该条目注释）；
/// 3. `group.add_member`：撤销 `idempotency_key` 参数——执行器没发、服务端
///    `add_members` 不读头，天然幂等靠 `PRIMARY KEY (group_id, user_id)`
///    去重（见 catalog.rs 该条目注释）；
/// 4. `msg.send`：撤销 `reply_to` 参数——HTTP 面（`SendBody`）没有这个字段，
///    透传是 #315 提的 server 诉求，补上后再把声明加回来。
const GENERATED_SURFACE_SHA256: &str =
    "af0334426457d1753606aa0114ca1216802c2c30427542a807e97c775aa9bd7c";
