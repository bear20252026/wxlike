//! 契约 fixture 的 Rust 端一致性测试。
//!
//! `fixtures/contract-v1/*.json` 是**三端共用**的样例：Rust、TS、Dart 各自
//! 解析同一个 `value`，再按同一条规则（对象键排序、无空白、数字一律字符串）
//! 输出规范化 JSON，必须与文件里的 `canonical` **逐字节**相同。
//!
//! 为什么用"规范化文本"而不是"各自断言字段"：字段断言只能覆盖你想到的字段，
//! 规范化文本能覆盖**全部**字段与顺序差异。三端跑同一份 fixture 的输出一致，
//! 才叫"同源"。
//!
//! TS 端：`fixtures/run_conformance.mjs`；Dart 端：`fixtures/run_conformance.dart`。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wxlike_types::{
    ActionRequest, ActionResult, Cursor, CursorKind, DomainError, EventEnvelope, MessageId, Money,
    Seq,
};

/// 规范化：对象键升序、无空白、其余交给 serde_json（数字都已用字符串承载）。
fn canonical(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push(':');
                write_canonical(&map[key.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&serde_json::to_string(scalar).unwrap_or_default()),
    }
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/contract-v1")
}

/// 按 `type` 字段把 `value` 解成强类型，再序列化回 `Value`。
///
/// 返回 `None` 表示这个 fixture 的类型本端还没有对应实现（新增 fixture 时
/// 先落文件、再补实现），测试会明确失败而不是静默跳过。
fn reencode(kind: &str, value: &Value) -> Option<Value> {
    let out = match kind {
        "Money" => serde_json::to_value(serde_json::from_value::<Money>(value.clone()).ok()?),
        "Seq" => serde_json::to_value(serde_json::from_value::<Seq>(value.clone()).ok()?),
        "Cursor" => serde_json::to_value(serde_json::from_value::<Cursor>(value.clone()).ok()?),
        "MessageId" => {
            serde_json::to_value(serde_json::from_value::<MessageId>(value.clone()).ok()?)
        }
        "DomainError" => {
            serde_json::to_value(serde_json::from_value::<DomainError>(value.clone()).ok()?)
        }
        "ActionRequest" => {
            serde_json::to_value(serde_json::from_value::<ActionRequest>(value.clone()).ok()?)
        }
        "ActionResult" => {
            serde_json::to_value(serde_json::from_value::<ActionResult>(value.clone()).ok()?)
        }
        "EventEnvelope" => {
            serde_json::to_value(serde_json::from_value::<EventEnvelope>(value.clone()).ok()?)
        }
        _ => None?,
    };
    out.ok()
}

#[test]
fn every_fixture_matches_its_canonical_form() {
    let dir = fixture_dir();
    let mut checked = 0usize;
    for entry in fs::read_dir(&dir).expect("fixtures 目录存在") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("读 fixture");
        let doc: Value = serde_json::from_str(&text).expect("fixture 是合法 JSON");
        let kind = doc["type"].as_str().expect("fixture 有 type");
        let expected = doc["canonical"].as_str().expect("fixture 有 canonical");
        let encoded = reencode(kind, &doc["value"])
            .unwrap_or_else(|| panic!("{}: 类型 {kind} 没有对应实现", path.display()));
        assert_eq!(
            canonical(&encoded),
            expected,
            "{}: 规范化结果与 canonical 不一致",
            path.display()
        );
        checked += 1;
    }
    assert!(checked >= 10, "fixture 太少（{checked}），契约覆盖不足");
}

#[test]
fn cursor_fixture_is_the_one_this_crate_produces() {
    // 游标是不透明的：fixture 里的 token 必须是本端 `Cursor::at` 的原样输出，
    // 否则"客户端存下来再传回服务端"这条路就已经断了。
    let path = fixture_dir().join("cursor.event.json");
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(path).expect("读 fixture")).expect("json");
    let token = doc["value"].as_str().expect("token 是字符串");
    assert_eq!(token, Cursor::at(CursorKind::Event, Seq::new(42)).as_str());
}

#[test]
fn big_numbers_survive_as_exact_integers() {
    let path = fixture_dir().join("money.big.json");
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(path).expect("读 fixture")).expect("json");
    let money: Money = serde_json::from_value(doc["value"].clone()).expect("money");
    assert_eq!(money.minor(), 9_007_199_254_740_993);
    // 用 f64 走一遍会丢：证明"为什么必须是字符串"
    let lossy = 9_007_199_254_740_993_f64 as i64;
    assert_ne!(lossy, money.minor());
}
