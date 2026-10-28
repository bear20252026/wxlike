//! `tools/list` 的逐字节基线（#320 的替换门）。
//!
//! 口径：元工具 4 个（list / execute / inspect / jobs）+ catalog 32 条 = **36 个**。
//!
//! 翻新流程（别走自证循环）：基线里的旧项一律保留，新增项按契约手工插入，
//! **不得**用被测 rmcp 的输出整体覆盖预期；改完更新下面的 SHA-256 并在提交里写理由。
//! #320 那次是拿 `origin/main` 上手写分派编出的二进制 `--print-tools` 与 rmcp 版逐字节比对；
//! #316 这次是保留原 30 项字段与相对顺序、按六条好友契约插入 6 项，再用诊断与协议两条通道比对。

use serde_json::Value;
use wxlike_mcp::server::Server;

use wxlike_client::execute::Executor;
use wxlike_client::Http;
use wxlike_cmd::policy::Policy;
use wxlike_cmd::CATALOG;

const BASELINE: &str = include_str!("snapshots/tools_list.json");
/// `snapshots/tools_list.json` 的 SHA-256：动基线必须留下说明。
const BASELINE_SHA256: &str = "01bac1fffec7fa3def6e3fce12fd5734c31e7804a1623aeef0c1466dca1c6337";

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
fn baseline_is_36_tools_and_matches_its_own_hash() {
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
    assert_eq!(base["tools"].as_array().expect("tools").len(), 36);
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

/// 数量口径写死：32 条命令 + 4 个元工具，多的少的都算回归。
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
