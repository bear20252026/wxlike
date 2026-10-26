//! 与新 UI（TS 形状）及仓库契约目录的一致性测试。
//!
//! 两件事：
//!
//! 1. **TS 形状往返**（`apps/desktop-web/src/shell/core-contract.ts`）：
//!    UI 发的是 `{action_id, request_id, schema_version:1, params}`，没有
//!    `trace_id` / `contract`；解开后按 Rust 形状回包，UI 端的
//!    `{ok:true,data} | {ok:false,error{code,message,retryable}}` 分流用的
//!    `to_wire_code` 必须落在它的 `ErrorCode` 全集里。
//! 2. **`contracts/common/` 的 fixture**：线上信封形状在兼容窗口规则下
//!    （未知字段忽略、未知枚举收敛、缺字段不整体失败）必须能解。

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use wxlike_types::{
    is_empty_payload, ActionOutcome, ActionRequest, ActionResult, DomainError, EventEnvelope,
    IdempotencyKey, TraceId,
};

/// TS 端 `newRequest()` 产出的字面形状（schema_version:1，无 trace_id）。
const TS_REQUEST: &str = r#"{
  "action_id": "msg.send",
  "request_id": "ui-lx2zmk-3",
  "schema_version": 1,
  "idempotency_key": "desk.chat.send:c0ffee",
  "params": {"conv": "d-alice~bob", "text": "你好"}
}"#;

/// TS 形状的请求解开后，按 Rust 契约回一条结果，UI 端能按
/// `{ok:true,data}|{ok:false,error}` 分流。
#[test]
fn ts_shaped_request_round_trips_into_a_ts_shaped_response() {
    let request: ActionRequest = serde_json::from_str(TS_REQUEST).expect("TS 形状必须可解");
    assert_eq!(request.schema_version, Some(1));
    assert_eq!(request.request_id.as_deref(), Some("ui-lx2zmk-3"));
    assert_eq!(request.trace_id, TraceId::default());

    // 回应：核心返回的三态 → TS 的两态（empty 折成 data:null）
    let result = ActionResult::ok(
        request.action_id.clone(),
        json!({"msg_id": "00112233445566778899aabbccddeeff"}),
        1_700_000_000_000,
    );
    let response = ts_response(&result);
    assert_eq!(response["ok"], true);
    assert_eq!(response["data"]["msg_id"], "00112233445566778899aabbccddeeff");

    let empty = ActionResult::empty(request.action_id.clone(), 1);
    let response = ts_response(&empty);
    assert_eq!(response["ok"], true, "空结果不是错误");
    assert!(response["data"].is_null());

    let failed = ActionResult::failed(
        request.action_id.clone(),
        DomainError::RateLimited {
            message: "请求过于频繁".to_string(),
            retry_after_ms: 3_000,
        },
        2,
    );
    let response = ts_response(&failed);
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "RATE_LIMITED");
    assert_eq!(response["error"]["retryable"], true);
    assert_eq!(response["error"]["retry_after_ms"], 3_000);
}

/// `ActionResult`（Rust 三态）→ TS 两态 JSON。`client-web` 的 wasm 出口
/// 与这里保持同一形状（这是形状的钉子，不是实现）。
fn ts_response(result: &ActionResult) -> Value {
    match &result.outcome {
        ActionOutcome::Ok { data } => json!({"ok": true, "data": data}),
        ActionOutcome::Empty => json!({"ok": true, "data": null}),
        ActionOutcome::Failed { error } => {
            let mut out = json!({
                "ok": false,
                "error": {
                    "code": error.to_wire_code(),
                    "message": if error.is_user_facing() {
                        error.message()
                    } else {
                        "内部错误，请稍后重试"
                    },
                    "retryable": error.is_retryable(),
                }
            });
            if let DomainError::RateLimited { retry_after_ms, .. } = error {
                out["error"]["retry_after_ms"] = json!(retry_after_ms);
            }
            out
        }
    }
}

/// TS 端错误码全集（`core-contract.ts` 的 `isErrorCode`）。`to_wire_code`
/// 的输出必须钉在这份表里，否则 UI 的 switch 走到 default。
#[test]
fn every_domain_error_maps_into_the_ts_error_code_set() {
    let ts_codes = [
        "UNAUTHENTICATED",
        "FORBIDDEN",
        "VALIDATION",
        "CONFLICT",
        "RATE_LIMITED",
        "REQUIRES_CONFIRMATION",
        "DEPENDENCY_UNAVAILABLE",
        "NOT_IMPLEMENTED",
        "OFFLINE",
        "CANCELLED",
        "INTERNAL",
    ];
    let samples = [
        DomainError::invalid("x"),
        DomainError::unauthorized("x"),
        DomainError::forbidden("x"),
        DomainError::not_found("x"),
        DomainError::conflict("x"),
        DomainError::Precondition {
            message: "x".to_string(),
        },
        DomainError::RateLimited {
            message: "x".to_string(),
            retry_after_ms: 1,
        },
        DomainError::timeout("x"),
        DomainError::cancelled(),
        DomainError::unavailable("x"),
        DomainError::internal("x"),
    ];
    for sample in &samples {
        assert!(
            ts_codes.contains(&sample.to_wire_code()),
            "{} 的 wire 码 {} 不在 TS 端码表里",
            sample.code(),
            sample.to_wire_code()
        );
    }
}

/// Rust → TS → Rust 的幂等键不丢：UI 用 `newIdempotencyKey` 生成的键
/// 必须能通过 `IdempotencyKey::parse`（写命令的硬要求）。
#[test]
fn ts_idempotency_keys_are_contract_legal() {
    for key in [
        "desk.chat.send:c0ffee",
        "ui-send:0.7v3s8x9m2k1",
        "conv.pin:5127f19b",
    ] {
        assert!(IdempotencyKey::parse(key).is_ok(), "{key} 必须合法");
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace 根目录")
        .to_path_buf()
}

/// `contracts/common/fixtures/protocol-v1.json` 里的信封类 fixture：
/// 按 schema 兼容窗口规则逐条过一遍——**解析成功**是硬要求，
/// 未知字段被忽略、未知枚举收敛为 Unknown、大整数 seq 走字符串。
#[test]
fn contracts_common_envelope_fixtures_parse_leniently() {
    let path = repo_root().join("contracts/common/fixtures/protocol-v1.json");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("读 {}: {err}", path.display()));
    let doc: Value = serde_json::from_str(&text).expect("fixture 文件是合法 JSON");
    let fixtures = doc["fixtures-v1"]
        .as_array()
        .expect("fixtures-v1 是数组");

    let mut checked = 0;
    for fixture in fixtures {
        let input = &fixture["input"];
        if input.get("seq").is_none() {
            // 非信封类 fixture（如写命令幂等）：不属于本测试的形状
            continue;
        }
        let id = fixture["fixture_id"].as_str().unwrap_or_default();
        let envelope: EventEnvelope = serde_json::from_value(input.clone())
            .unwrap_or_else(|err| panic!("{id}: schema 合法信封解析失败：{err}"));
        // 未知枚举确定性收敛（Text/Blob/System 在事件侧都不认识）
        let raw_kind = input["kind"].as_str().unwrap_or_default().to_string();
        assert!(
            !envelope.kind.is_known() || envelope.kind.as_str() == raw_kind,
            "{id}: 未知枚举必须原样保留"
        );
        // 大整数 seq 不经 f64 往返
        if let Some(raw) = input["seq"].as_str() {
            let expected = raw.parse::<u64>().expect("fixture seq 是十进制");
            assert_eq!(envelope.seq.get(), expected, "{id}: seq 失真");
        }
        checked += 1;
    }
    assert!(checked >= 3, "信封类 fixture 太少（{checked}），fixture 文件变了？");
}

/// 幂等 fixture（FX-write-idempotency）：同一 key 的请求重放必须命中
/// 同一条 `ActionRequest`（幂等键是去重的唯一依据）。
#[test]
fn write_idempotency_fixture_survives_a_rust_round_trip() {
    let raw = json!({
        "action_id": "message.send",
        "request_id": "ui-retry-1",
        "schema_version": 1,
        "idempotency_key": "idem-1",
        "params": {"conv": "d-a~b", "text": "hi"}
    });
    let first: ActionRequest = serde_json::from_value(raw.clone()).expect("de");
    let second: ActionRequest = serde_json::from_value(raw).expect("de");
    assert_eq!(first.idempotency_key, second.idempotency_key);
    assert_eq!(
        first.idempotency_key.as_ref().map(IdempotencyKey::as_str),
        Some("idem-1")
    );
    // 序列化回 Rust 形状再解：键不丢
    let wire = serde_json::to_value(&first).expect("ser");
    let back: ActionRequest = serde_json::from_value(wire).expect("de");
    assert_eq!(back, first);
}

/// 空结果判定在 crate 层共享：`is_empty_payload` 是 cmd / client-core
/// 两处分流的唯一口径。
#[test]
fn empty_semantics_come_from_one_place() {
    assert!(is_empty_payload(&json!(null)));
    assert!(is_empty_payload(&json!([])));
    assert!(is_empty_payload(&json!({})));
    assert!(is_empty_payload(&json!({"count": 0, "messages": []})));
    assert!(!is_empty_payload(&json!({"messages": [{"seq": 1}]})));
}
