//! #296 / G01：能力目录 ↔ 路由表 ↔ 前端契约的静态对账。
//!
//! 这里不起服务端、不建会话——只把 `core_bridge.rs` 的源码当数据读，
//! 让"目录里登记的 action 路由表里真的有、路由表里挂了的目录一定登记"这条
//! 不变式由机器兜底。审计当天的两类实锤都能被它抓到：
//!
//! 1. `relations.group.kick` 在 `dispatch_inner` 与 `relations_router` 都挂了，
//!    却没进 `IMPLEMENTED_ACTIONS` → `core_capabilities()` 不报，UI 发现不了；
//!    修复方向只能是"真实现 + 登记"，不是"照着挂了就往目录里塞"。
//! 2. 一个 action 只挂在总入口却漏了子路由，会掉进子路由的 `_ => unrouted(req)`
//!    ——目录里登记了也一样是空壳。
//! 3. "一个处理器服务多条 action"（共享处理器 + 未核实副作用）列成显式白名单，
//!    新不加登记就得先写测试。
//!
//! 关于第 1、2 条的**成因**：action 名当时在 `dispatch_inner` 和每个 `*_router`
//! 里各列了一遍，两处各自漂移。现在总入口只按域前缀选路由（[`entry_table`]
//! 读的就是这份前缀表），action 名单的唯一来源是各 `*_router` 的臂，
//! 所以还额外钉了两条：
//!
//! 4. 总入口不许再列举 action 名（[`no_action_is_listed_in_both_the_entry_and_a_router`]）；
//! 5. 前缀表必须把每条臂都走到、且走到它自己所在的那个路由，反向也要每条前缀
//!    都有臂、每个子路由都被走到（[`prefix_table_routes_every_router_arm`]）。
//!
//! 注：`STUBBED_ACTIONS` 是按文本读的（不是按符号），所以本测试在修复前的树上
//! 同样能编译，并以断言失败的形式报出漂移，而不是以编译失败的形式。

use std::collections::{BTreeMap, BTreeSet};

use wxlike_ui_lib::core_bridge::{capabilities, IMPLEMENTED_ACTIONS};

const SRC: &str = include_str!("../src/core_bridge.rs");

/// 已核实的共享处理器：一条 action 一个分支，副作用互不污染。
/// 逐条证明在 `tests/shared_handlers.rs`（pin/mute、accept/decline、tag/star）。
const VETTED_SHARED_HANDLERS: &[&str] = &[
    "act_messaging_conversation_pin",
    "act_relations_request_accept",
    "act_relations_tag_set",
];

// ---------------------------------------------------------------------------
// 解析工具（没有 regex 依赖，手搓够用的一条扫描线）
// ---------------------------------------------------------------------------

/// 是不是 action_id 形状：小写点分（`relations.group.kick`）。
fn is_action_id(lit: &str) -> bool {
    lit.contains('.')
        && !lit.starts_with('.')
        && !lit.ends_with('.')
        && !lit.contains("..")
        && lit.len() > 3
        && lit
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

/// 从 `open` 处的 `"` 找到未转义的收尾 `"`，返回其字节下标。
fn close_quote(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// 列出一段文本里的所有字符串字面量。
fn string_literals(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let Some(end) = close_quote(text, i) else {
            i += 1;
            continue;
        };
        out.push(&text[i + 1..end]);
        i = end + 1;
    }
    out
}

/// 按文档顺序遍历一段代码里的字符串字面量，给出每个字面量**之后**剩下的文本。
///
/// 三个解析器都要看"字面量后面紧跟的是 `|`、`,` 还是 `=>`"才能判断它在 match
/// 模式里的角色，所以把这层扫描抽出来共用。
fn literals_with_tail(text: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let Some(open) = text[i..].find('"') else {
            break;
        };
        let Some(end) = close_quote(text, i + open) else {
            i += 1;
            continue;
        };
        let start = i + open;
        out.push((&text[start + 1..end], &text[end + 1..]));
        i = end + 1;
    }
    out
}

/// `=>` 后面的目标函数名（`qr_router::<R>` → `qr_router`）。
fn arm_target(after: &str) -> String {
    after
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// 扫一张路由表，返回 `(action_id, 目标函数)`。
///
/// 只认 match 模式位上的字面量：后面紧跟 `|`（同一臂续写）或 `=>`（臂结尾）。
/// 处理器体里的 `"text" | "e2ee" =>` 之类没有点号，被 [`is_action_id`] 挡掉。
fn routes(table: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    for (literal, tail) in literals_with_tail(table) {
        let rest = tail.trim_start();
        if let Some(after) = rest.strip_prefix("=>") {
            if is_action_id(literal) {
                pending.push(literal.to_string());
            }
            let name = arm_target(after);
            for action in pending.drain(..) {
                out.push((action, name.clone()));
            }
            continue;
        }
        if rest.starts_with('|') && !rest.starts_with("||") {
            if is_action_id(literal) {
                pending.push(literal.to_string());
            }
            continue;
        }
        // 落在模式位之外（处理器体里的字符串）：待定的 action 不能留给下一臂。
        pending.clear();
    }
    out
}

/// 总入口的**域前缀表**：每条形如 `("identity", "qr_login") => qr_router::<R>,`。
///
/// 返回 `(head, family, 目标子路由)`，family 为 None 就是模式里的 `_`（"这个域
/// 下所有 action 都归这个子路由"）。`_ => return None` 这种没有字面量的臂跳过。
/// 一个元组一行（rustfmt 不会把这么短的行折开），所以按行解析。
fn entry_table(body: &str) -> Vec<(String, Option<String>, String)> {
    let mut out = Vec::new();
    for line in body.lines() {
        let Some((pattern, target)) = line.split_once("=>") else {
            continue;
        };
        let name = arm_target(target);
        // 一行可以挂多个元组（`("a", _) | ("b", _)`）：按 `|` 切开后成对读字面量。
        for piece in pattern.split('|') {
            match string_literals(piece).as_slice() {
                [] => {}
                [head] => out.push(((*head).to_string(), None, name.clone())),
                [head, family] => out.push((
                    (*head).to_string(),
                    Some((*family).to_string()),
                    name.clone(),
                )),
                // 三元组以上说明这张表不是"每段一个前缀"，解析器得跟着重写。
                many => panic!("域前缀的一行里出现意外的字面量数量：{line} {many:?}"),
            }
        }
    }
    out
}

/// 是不是总入口那一层的条目（前缀表本体 + 唯一调用点）。
fn is_entry_table(name: &str) -> bool {
    name == "dispatch_inner" || name == "route_for"
}

/// 去掉整行注释（含 `///` 文档），只留代码。
///
/// "总入口不许列举 action 名"要按字面量扫，而注释里提到某个 action 名是正常的
/// （#296 的来龙去脉就写在注释里），不能算列举。
fn strip_line_comments(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<&str>>()
        .join("\n")
}

/// 顶层条目的起始字节（列 0 上的 `fn`/`pub fn`/`const`/`impl`/`mod`/`static`）。
fn item_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let head = line.trim_end_matches(['\r', '\n']);
        if [
            "fn ",
            "pub fn ",
            "pub(crate) fn ",
            "const ",
            "pub const ",
            "static ",
            "pub static ",
            "impl ",
            "mod ",
        ]
        .iter()
        .any(|prefix| head.starts_with(prefix))
        {
            starts.push(offset);
        }
        offset += line.len();
    }
    starts
}

/// 取路由表的函数体：从它的 `fn` 行到下一个顶层条目之前。
///
/// 只按顶层条目切分，不做花括号配对——体内出现的 `}` 都在缩进列，而顶层 `fn`
/// 的收尾 `}` 在列 0，所以不会因为注释/字符串里的花括号被骗。
///
/// 收进来的三类条目各有各的职责，少一类就会漏判：
/// * `route_for` —— 域前缀表（谁走到哪个子路由）；
/// * `dispatch_inner` —— 前缀表的唯一调用点（防止"表在、但没人按表分派"）；
/// * `*_router` —— action 名单的唯一来源。
fn routing_tables(text: &str) -> Vec<(String, String)> {
    let starts = item_starts(text);
    let mut out = Vec::new();
    for (rank, start) in starts.iter().enumerate() {
        let end = starts.get(rank + 1).copied().unwrap_or(text.len());
        // 下一个顶层条目可能就在下一行（空行分隔），所以按条目切而不是按 `}` 找。
        let item = &text[*start..end];
        let head = item
            .trim_start_matches("pub ")
            .trim_start_matches("pub(crate) ");
        let Some(rest) = head.strip_prefix("fn ") else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if is_entry_table(&name) || name.ends_with("_router") {
            out.push((name, item.to_string()));
        }
    }
    out
}

/// 抠 `pub const NAME: &[&str] = &[ ... ];` 里的字面量；常量不存在时返回 None。
fn const_list(text: &str, name: &str) -> Option<Vec<String>> {
    let marker = format!("pub const {name}: &[&str] = &[");
    let start = text.find(&marker)? + marker.len();
    let rest = &text[start..];
    let end = rest.find("];")?;
    Some(
        string_literals(&rest[..end])
            .into_iter()
            .map(str::to_string)
            .collect(),
    )
}

/// action 名单的唯一来源：各子路由的 match 臂（总入口只有域前缀，不参与）。
fn routed_set(tables: &[(String, String)]) -> BTreeSet<String> {
    router_arms(tables)
        .into_iter()
        .map(|(action, _)| action)
        .collect()
}

/// 同上，但要"是谁挂的"：`(action, 所在子路由)`。
fn router_arms(tables: &[(String, String)]) -> BTreeSet<(String, String)> {
    tables
        .iter()
        .filter(|(name, _)| name.ends_with("_router"))
        .flat_map(|(name, body)| {
            routes(body)
                .into_iter()
                .map(move |(action, _)| (action, name.clone()))
        })
        .collect()
}

/// action 的域键：第一段，以及第二段（identity 有两个域）。
fn segments(action: &str) -> (String, String) {
    let mut parts = action.split('.');
    let head = parts.next().unwrap_or_default();
    let family = parts.next().unwrap_or_default();
    (head.to_string(), family.to_string())
}

/// 前缀表里第一个能接住这条 action 的条目（与 `match` 一致：先到先得）。
fn find_entry<'a>(
    entries: &'a [(String, Option<String>, String)],
    action: &str,
) -> Option<&'a (String, Option<String>, String)> {
    let (head, family) = segments(action);
    entries.iter().find(|(key_head, key_family, _)| {
        *key_head == head && key_family.as_deref().is_none_or(|f| f == family)
    })
}

/// 这条 action 该被走到哪个子路由（None = 前缀表里没有它的位置）。
fn resolve<'a>(entries: &'a [(String, Option<String>, String)], action: &str) -> Option<&'a str> {
    find_entry(entries, action).map(|(_, _, target)| target.as_str())
}

/// 一个条目的键，只用来把断言消息写得能读（`"identity.qr_login"` / `"messaging"`）。
fn entry_key(entry: &(String, Option<String>, String)) -> String {
    match (&entry.0, &entry.1) {
        (head, Some(family)) => format!("{head}.{family}"),
        (head, None) => head.clone(),
    }
}

fn catalog() -> BTreeSet<String> {
    IMPLEMENTED_ACTIONS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

fn stubbed() -> BTreeSet<String> {
    const_list(SRC, "STUBBED_ACTIONS")
        .unwrap_or_default()
        .into_iter()
        .collect()
}

// ---------------------------------------------------------------------------
// 对账
// ---------------------------------------------------------------------------

/// 解析器自身的健康检查：它要是扫空了，后面的断言就会假绿。
#[test]
fn parser_finds_the_routing_tables() {
    let tables = routing_tables(SRC);
    let names: Vec<&str> = tables.iter().map(|(n, _)| n.as_str()).collect();
    for entry in ["dispatch_inner", "route_for"] {
        assert!(names.contains(&entry), "没找到总入口的 {entry}：{names:?}");
    }
    assert!(
        names.iter().filter(|n| n.ends_with("_router")).count() >= 6,
        "子路由表数量不对（只找到 {names:?}）——分域结构变了要同步改本测试"
    );
    let routed = routed_set(&tables);
    assert!(
        routed.len() > 40,
        "子路由里只解析出 {} 个 action，解析器八成失效了",
        routed.len()
    );
    // 总入口那层要能解析出前缀表，并且真的被 dispatch_inner 调用。
    let entry = tables
        .iter()
        .find(|(name, _)| name == "route_for")
        .expect("route_for 必须存在")
        .1
        .clone();
    let prefixes = entry_table(&entry);
    assert!(
        prefixes.len() >= 8,
        "域前缀表只解析出 {} 条：{prefixes:?}",
        prefixes.len()
    );
    let inner = tables
        .iter()
        .find(|(name, _)| name == "dispatch_inner")
        .expect("dispatch_inner 必须存在")
        .1
        .clone();
    assert!(
        strip_line_comments(&inner).contains("route_for"),
        "dispatch_inner 没有按域前缀表分派——那张表就是摆设"
    );
    // 一张表里同一个 action 不许挂两次（后一条永远走不到，还会掩盖漂移）。
    for (name, body) in &tables {
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for (action, _) in routes(body) {
            *seen.entry(action).or_default() += 1;
        }
        let dupes: Vec<&String> = seen
            .iter()
            .filter(|(_, n)| **n > 1)
            .map(|(a, _)| a)
            .collect();
        assert!(
            dupes.is_empty(),
            "路由表 {name} 里同一个 action 挂了两次：{dupes:?}"
        );
    }
}

/// 核心不变式：路由 == 能力目录 ∪ 有意 stub 表，且两表互斥、目录无重复。
#[test]
fn capability_catalog_matches_the_dispatch_table() {
    let routed = routed_set(&routing_tables(SRC));
    let parsed_catalog = const_list(SRC, "IMPLEMENTED_ACTIONS")
        .expect("IMPLEMENTED_ACTIONS 常量必须存在（capabilities 就靠它）")
        .into_iter()
        .collect::<BTreeSet<String>>();
    let implemented = catalog();
    let stubs = stubbed();
    let advertised = routed_union(&implemented, &stubs);

    // 源码里解析出的目录必须就是代码在用的那个（否则对账对的是空气）。
    assert_eq!(
        parsed_catalog, implemented,
        "解析出的 IMPLEMENTED_ACTIONS 与符号本身不一致：解析器该修了"
    );
    assert_eq!(
        IMPLEMENTED_ACTIONS.len(),
        implemented.len(),
        "IMPLEMENTED_ACTIONS 里有重复项"
    );

    let undiscoverable: Vec<&String> = routed.difference(&advertised).collect();
    let lies: Vec<&String> = advertised.difference(&routed).collect();
    assert!(
        undiscoverable.is_empty() && lies.is_empty(),
        "能力目录与路由表漂移：\n  已路由但没登记（UI 发现不了）：{undiscoverable:?}\n  已登记但没路由（capabilities 说谎）：{lies:?}\n\
         修法：真实现 → 加进 IMPLEMENTED_ACTIONS；确实做不了 → 加进 STUBBED_ACTIONS 并保留如实的 NOT_IMPLEMENTED。"
    );

    let overlap: Vec<&String> = implemented.intersection(&stubs).collect();
    assert!(
        overlap.is_empty(),
        "同一个 action 既算已实现又算 stub：{overlap:?}"
    );
}

fn routed_union(left: &BTreeSet<String>, right: &BTreeSet<String>) -> BTreeSet<String> {
    left.union(right).cloned().collect()
}

/// 域前缀表与子路由臂必须双向对得上：
/// * 正向——每条臂都能从前缀表走到，且走到的就是它自己所在的那个路由
///   （挂错域 = 恒返 `unrouted` 的空壳；漏一行 = 那条 action 永远走不到）；
/// * 反向——每条前缀都得真有臂（否则是个空域），每个子路由都得被走到
///   （否则是死代码），前缀表里指向的函数必须是真存在的子路由。
#[test]
fn prefix_table_routes_every_router_arm() {
    let tables = routing_tables(SRC);
    let entry = tables
        .iter()
        .find(|(name, _)| name == "route_for")
        .expect("route_for 必须存在（总入口的域前缀表）");
    let prefixes = entry_table(&entry.1);
    let routers: BTreeSet<String> = tables
        .iter()
        .filter(|(name, _)| name.ends_with("_router"))
        .map(|(name, _)| name.clone())
        .collect();
    let arms = router_arms(&tables);

    // 前缀表指向的目标必须是真实存在的子路由。
    let dangling: Vec<&String> = prefixes
        .iter()
        .map(|(_, _, target)| target)
        .filter(|target| !routers.contains(*target))
        .collect();
    assert!(
        dangling.is_empty(),
        "域前缀表指向了不存在的子路由：{dangling:?}"
    );

    // 正向：每条臂都能走到自己所在的那个路由。
    let mut wrong: Vec<(String, String, String)> = Vec::new();
    let mut strays: Vec<String> = Vec::new();
    for (action, owner) in &arms {
        match resolve(&prefixes, action) {
            Some(target) if target == owner => {}
            Some(target) => wrong.push((action.clone(), owner.clone(), target.to_string())),
            None => strays.push(action.clone()),
        }
    }
    assert!(
        wrong.is_empty() && strays.is_empty(),
        "域前缀表与子路由对不上：\n  走错了子路由（action, 所在子路由, 走到哪）：{wrong:?}\n  \
         前缀表里没有它的位置（永远走不到，恒返 NOT_IMPLEMENTED）：{strays:?}\n\
         修法：改 route_for 那一行的域键，或把臂挪到走得到的那个子路由里。"
    );
    assert!(
        arms.len() > 40,
        "子路由里只解析出 {} 条臂，解析器八成失效了",
        arms.len()
    );

    // 反向：每条前缀都要有它服务的臂。
    let used: BTreeSet<String> = arms
        .iter()
        .filter_map(|(action, _)| find_entry(&prefixes, action))
        .map(entry_key)
        .collect();
    let dead: Vec<String> = prefixes
        .iter()
        .map(entry_key)
        .filter(|key| !used.contains(key))
        .collect();
    assert!(
        dead.is_empty(),
        "域前缀 {dead:?} 手上一臂都没有——要么删掉，要么把 action 登记到它指向的子路由"
    );

    // 反向：每个子路由都要被总入口走到（没被走到 = 死代码）。
    let targeted: BTreeSet<String> = prefixes
        .iter()
        .map(|(_, _, target)| target.clone())
        .collect();
    let unreachable: Vec<&String> = routers.difference(&targeted).collect();
    assert!(
        unreachable.is_empty(),
        "子路由 {unreachable:?} 没有任何前缀指向它（走不到，等于没实现）"
    );
}

/// 同一个 action 名在两处各列一遍，正是 #296 的漂移来源：
/// `relations.group.kick` 当时在 `dispatch_inner` 与 `relations_router` 都挂着，
/// 目录里却没有——两处各自演化，谁也没发现少了一边。
///
/// 现在名单的唯一来源是各 `*_router` 的臂，总入口只认域前缀。所以总入口那层
/// 一个 action_id 都不许出现（前缀本身没有点号，不是 action_id 形状）。
#[test]
fn no_action_is_listed_in_both_the_entry_and_a_router() {
    let tables = routing_tables(SRC);
    let arms = routed_set(&tables);
    assert!(!arms.is_empty(), "子路由一条臂都没有：这条对账就失去意义了");
    for (name, body) in &tables {
        if !is_entry_table(name) {
            continue;
        }
        // 注释里讲历史可以提 action 名，代码里不行：前缀表的键都不带点号。
        let listed: Vec<String> = string_literals(&strip_line_comments(body))
            .into_iter()
            .filter(|lit| is_action_id(lit))
            .map(str::to_string)
            .collect();
        let duplicated: Vec<&String> = listed.iter().filter(|lit| arms.contains(*lit)).collect();
        let unregistered: Vec<&String> = listed.iter().filter(|lit| !arms.contains(*lit)).collect();
        assert!(
            listed.is_empty(),
            "总入口 {name} 又列举了 action 名：{listed:?}\n  \
             与子路由重复（#296 的漂移来源，改一处忘一处）：{duplicated:?}\n  \
             子路由里根本没有（这条臂恒返 NOT_IMPLEMENTED）：{unregistered:?}\n\
             名单的唯一来源是各 *_router 的臂；这里只该有域前缀（head/family，都不带点号）。"
        );
    }
}

/// 共享处理器要显式登记：一个 fn 服务多条 action 正是 #296 第二类缺陷的温床
/// （`messaging.conversation.mute` 曾被指望"顺带"把 pin 的事也做了）。
#[test]
fn shared_handlers_are_deliberately_registered() {
    let tables = routing_tables(SRC);
    let mut by_handler: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, body) in &tables {
        if !name.ends_with("_router") {
            continue; // 总入口只到子域，共享发生在子路由里
        }
        for (action, handler) in routes(body) {
            if handler.starts_with("act_") {
                by_handler.entry(handler).or_default().push(action);
            }
        }
    }
    let shared: Vec<(&String, &Vec<String>)> = by_handler
        .iter()
        .filter(|(_, actions)| actions.len() > 1)
        .collect();

    let registered: Vec<&String> = shared
        .iter()
        .map(|(h, _)| *h)
        .filter(|h| !VETTED_SHARED_HANDLERS.contains(&h.as_str()))
        .collect();
    assert!(
        registered.is_empty(),
        "未登记的共享处理器：{registered:?}\n\
         一个处理器服务多条 action 时，副作用必须逐条证明（分支只改自己那份状态、\n  \
         返回投影不能被兄弟字段污染）。证完再把它加进 VETTED_SHARED_HANDLERS。"
    );

    // 白名单也不能烂在里面：登记了却不再共享 = 该删。
    let still_shared: BTreeSet<&str> = shared.iter().map(|(h, _)| h.as_str()).collect();
    for handler in VETTED_SHARED_HANDLERS {
        assert!(
            still_shared.contains(handler),
            "VETTED_SHARED_HANDLERS 里的 {handler} 现在已经不共享了，删掉这条登记"
        );
    }
}

/// 前端契约里会被 dispatch 的每个 action，桥这边一定要有归属
/// （已实现或已登记 stub）。#296 当天的实锤就是这条：`relations.group.kick`
/// 前端在调、桥在路由，但能力目录里没有它。
#[test]
fn every_action_the_ui_dispatches_is_accounted_for() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let path =
        std::path::Path::new(manifest).join("../../apps/desktop-web/src/shell/core-contract.ts");
    let Ok(ts) = std::fs::read_to_string(&path) else {
        eprintln!("SKIP：没有 {path:?}（只有装了前端才有这条可查）");
        return;
    };
    let Some(start) = ts.find("export const ACTIONS") else {
        eprintln!("SKIP：core-contract.ts 里找不到 ACTIONS 常量");
        return;
    };
    let declared: BTreeSet<String> = string_literals(&ts[start..])
        .into_iter()
        .filter(|lit| is_action_id(lit))
        .map(str::to_string)
        .collect();
    assert!(
        declared.len() > 40,
        "ACTIONS 里只解析出 {} 个 id",
        declared.len()
    );

    let mut accounted = catalog();
    accounted.extend(stubbed());
    let orphans: Vec<&String> = declared.difference(&accounted).collect();
    assert!(
        orphans.is_empty(),
        "前端会 dispatch 但桥的目录里没登记（UI 无从发现能力）：{orphans:?}"
    );
}

/// `core_capabilities()` 报的就是这份目录（不是另一份手抄表）。
#[test]
fn capabilities_advertise_exactly_the_catalog() {
    let advertised: Vec<String> = capabilities()["actions"]
        .as_array()
        .expect("capabilities 要有 actions 数组")
        .iter()
        .map(|v| v.as_str().expect("action id 必须是字符串").to_string())
        .collect();
    let symbol: Vec<String> = IMPLEMENTED_ACTIONS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(
        advertised, symbol,
        "capabilities 与 IMPLEMENTED_ACTIONS 不是同一份表"
    );
}
