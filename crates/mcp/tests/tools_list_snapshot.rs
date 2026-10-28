//! `tools/list` 的**逐字节基线**（#320 的替换门）。
//!
//! 为什么单独一个文件、而不是只看 fixture：`tests/fixtures/tools_list_shape.json`
//! 只钉住"第 0 个是 im_list_commands""schema 关 additionalProperties"这类**局部
//! 形状**，它绿不代表 30 个工具的字段值没被动过。#320 用 rmcp 替掉手写 JSON-RPC
//! 分派，而 rmcp 的 `tools/list` 走它自己的 `Tool` 模型序列化——字段被改名、被
//! `skip_serializing_if` 吃掉、或多出一个 `title: null`，shape fixture 都可能照绿。
//!
//! 真相来源是**替换前抓下来的输出**（`wxlike-mcp --print-tools`，当时还是手写分派），
//! 提交在 `snapshots/tools_list.json`。替换后两条通道都要过：诊断通道
//! `Server::tools_dump()`，与协议通道真帧 `tools/list` 的 `result.tools`。
//!
//! 基线换过一次，来历记在这：`cb446c9f` 那份抓于 #315 之前，#315 撤掉三处装饰参数
//! 声明之后它就该红（守卫照做了）。翻新**不能**用 rmcp 自己的输出重抓——那是自证循环。
//! 新基线来自 `origin/main`（`b49496db`，仍是手写分派）编出的二进制，与 rmcp 版的
//! `--print-tools` 逐字节比对：两份各 31,693 字节、`cmp` 无差异，这才是"实现换了而
//! 表面没换"的证据。以后 catalog 再动，照同一条路子翻新。
//!
//! 元工具口径 4 个（list / execute / inspect / jobs）+ catalog 26 条 = **30 个**。

use serde_json::Value;
use wxlike_mcp::server::Server;

use wxlike_client::execute::Executor;
use wxlike_client::Http;
use wxlike_cmd::policy::Policy;
use wxlike_cmd::CATALOG;

const BASELINE: &str = include_str!("snapshots/tools_list.json");
/// `snapshots/tools_list.json` 的 SHA-256：动基线必须留下说明。
const BASELINE_SHA256: &str = "c802be568d4e843a7d303e4b191ae419efbf61d830dd380339a80a87ad9cbe7a";

fn server() -> Server {
    let policy = Policy::none();
    Server::new(
        Executor::new(Http::new("http://127.0.0.1:1"), policy.clone()),
        policy,
    )
}

fn frame(srv: &mut Server, id: i64, method: &str) -> Value {
    let line = srv
        .handle_line(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}"}}"#
        ))
        .expect("response");
    serde_json::from_str(&line).expect("json")
}

/// 基线文件自己要先可信：它必须还是当初那份，不是后来被顺手改过的。
#[test]
fn baseline_is_30_tools_and_matches_its_own_hash() {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(BASELINE.as_bytes());
    assert_eq!(
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        BASELINE_SHA256,
        "快照文件被动过"
    );
    let base: Value = serde_json::from_str(BASELINE).expect("baseline json");
    assert_eq!(base["tools"].as_array().expect("tools").len(), 30);
}

/// 诊断通道：逐字节。
#[test]
fn tools_dump_is_byte_identical_to_the_baseline() {
    let dump = server().tools_dump();
    let want = BASELINE.trim_end_matches('\n');
    if dump != want {
        for (i, (a, b)) in dump.lines().zip(want.lines()).enumerate() {
            assert!(a == b, "第 {} 行起不一致：\n现在: {a}\n基线: {b}", i + 1);
        }
        panic!(
            "行数不同：现在 {} 行，基线 {} 行",
            dump.lines().count(),
            want.lines().count()
        );
    }
}

/// 协议通道：经 rmcp 序列化再解析回来，必须与基线是同一份 JSON。
#[test]
fn tools_list_frame_matches_the_baseline() {
    let mut srv = server();
    srv.handle_line(
        r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
    )
    .expect("initialize");
    let base: Value = serde_json::from_str(BASELINE).expect("baseline json");
    assert_eq!(
        frame(&mut srv, 1, "tools/list")["result"]["tools"],
        base["tools"],
        "tools/list 与基线不一致"
    );
}

/// 数量口径写死：26 条命令 + 4 个元工具，多的少的都算回归。
#[test]
fn tool_count_is_commands_plus_four_meta_tools() {
    let dump: Value = serde_json::from_str(&server().tools_dump()).expect("dump json");
    let tools = dump["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), CATALOG.len() + 4, "元工具口径是 4 个");
    for spec in CATALOG {
        assert!(
            tools
                .iter()
                .any(|t| t["name"] == spec.tool_name() && t["_meta"]["wxlike/actionId"] == spec.id),
            "缺工具 {}",
            spec.tool_name()
        );
    }
    for meta in ["im_list_commands", "im_execute", "im_inspect", "im_jobs"] {
        assert!(tools.iter().any(|t| t["name"] == meta), "缺元工具 {meta}");
    }
}
