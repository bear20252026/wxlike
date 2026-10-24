//! G14-04 验收：商户隔离、退款双人审批、补账分录、对账差异、导出防护。

#[path = "common/mod.rs"]
mod common;
#[path = "g14_common/mod.rs"]
mod g14;

use axum::http::StatusCode;
use serde_json::{json, Value};
use wxlike_admin::commerce::{Order, ReconDiff};
use wxlike_admin::read::now_ms;

use g14::Env;

fn seed(env: &Env, merchant: &str, order: &str, amount: i64) {
    let c = &env.app.g14().commerce;
    if !c.merchants().unwrap().iter().any(|m| m.id == merchant) {
        c.open_merchant(merchant, "测试商户", "6222000012345678")
            .unwrap();
    }
    c.ingest_order(&Order {
        id: order.to_string(),
        merchant_id: merchant.to_string(),
        internal_no: format!("in-{order}"),
        external_no: format!("out-{order}"),
        amount_cents: amount,
        currency: "CNY".into(),
        state: "paid".into(),
        created_at: now_ms(),
    })
    .unwrap();
}

#[tokio::test]
async fn merchant_scope_is_enforced_at_the_service_layer() {
    let mut env = Env::new("g14-04-scope");
    seed(&env, "m1", "o1", 10_000);
    seed(&env, "m2", "o2", 20_000);
    let fin = env.token("fin");
    let root = env.token("root");

    let (st, body) = env
        .call("GET", "/api/g14/commerce/orders", Some(&fin), None)
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["page"]["total"], 2, "{body}");

    // 超管可以显式切到单商户视角
    let (st, body) = env
        .call(
            "GET",
            "/api/g14/commerce/orders?as_merchant=m1",
            Some(&root),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["page"]["total"], 1, "{body}");
    assert_eq!(body["page"]["rows"][0]["merchant_id"], "m1");

    // 非超管不能伪造商户视角
    let (st, body) = env
        .call(
            "GET",
            "/api/g14/commerce/orders?as_merchant=m1",
            Some(&fin),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");

    // 只读岗能看订单（读面），但写不了：退款申请要求 Order 的 Write
    let view = env.token("view");
    let (st, _) = env
        .call("GET", "/api/g14/commerce/orders", Some(&view), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/commerce/orders/o1/refunds",
            Some(&view),
            Some(json!({"amount_cents":100,"reason":"退","idem":"v"})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn refund_over_threshold_needs_two_different_approvers() {
    let mut env = Env::new("g14-04-refund");
    seed(&env, "m1", "o9", 900_000);
    let fin = env.ready("fin").await;
    let root = env.ready("root").await;

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/commerce/orders/o9/refunds",
            Some(&fin),
            Some(json!({"amount_cents":600_000,"reason":"重复扣款","idem":"r-1"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let id = body["refund"]["id"].as_str().unwrap().to_string();

    // 审批人不能是申请人？这里允许（同一人可以申请），但**不能自己凑双人**
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/commerce/refunds/{id}/approve"),
            Some(&fin),
            Some(json!({"note":"同意"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["refund"]["state"], "partially_approved", "{body}");
    assert!(env.money.calls().is_empty(), "一人批准不能打款");

    // 同一人再批：拒
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/commerce/refunds/{id}/approve"),
            Some(&fin),
            Some(json!({"note":"再同意"})),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("两个不同的人"),
        "{body}"
    );

    // 第二人：打款
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/commerce/refunds/{id}/approve"),
            Some(&root),
            Some(json!({"note":"复核通过"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["refund"]["state"], "approved", "{body}");
    assert_eq!(env.money.calls().len(), 1);

    // 重复审批：幂等，不打第二次款
    let (st, _) = env
        .call(
            "POST",
            &format!("/api/g14/commerce/refunds/{id}/approve"),
            Some(&root),
            Some(json!({"note":"复核通过"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(env.money.calls().len(), 1, "重复审批不能重复打款");
}

#[tokio::test]
async fn small_refund_needs_only_one_approval() {
    let mut env = Env::new("g14-04-small");
    seed(&env, "m1", "o10", 50_000);
    let fin = env.ready("fin").await;
    let (_, body) = env
        .call(
            "POST",
            "/api/g14/commerce/orders/o10/refunds",
            Some(&fin),
            Some(json!({"amount_cents":1_000,"reason":"少发一件","idem":"r-2"})),
        )
        .await;
    let id = body["refund"]["id"].as_str().unwrap().to_string();
    let (st, body) = env
        .call(
            "POST",
            &format!("/api/g14/commerce/refunds/{id}/approve"),
            Some(&fin),
            Some(json!({"note":"同意"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["refund"]["state"], "approved", "{body}");
    assert_eq!(env.money.calls().len(), 1);
}

#[tokio::test]
async fn journal_must_balance_and_is_append_only_over_http() {
    let mut env = Env::new("g14-04-journal");
    let fin = env.ready("fin").await;

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/commerce/journal",
            Some(&fin),
            Some(json!({
                "entry_no":"E1","memo":"补账","source":"manual",
                "lines":[
                    {"account":"user_wallet:u1","direction":"credit","amount_cents":100},
                    {"account":"platform_revenue","direction":"debit","amount_cents":99}
                ]
            })),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("借贷不平"),
        "{body}"
    );

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/commerce/journal",
            Some(&fin),
            Some(json!({
                "entry_no":"E1","memo":"补账","source":"manual",
                "lines":[
                    {"account":"user_wallet:u1","direction":"credit","amount_cents":100},
                    {"account":"platform_revenue","direction":"debit","amount_cents":100}
                ]
            })),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["entries"].as_array().unwrap().len(), 2, "{body}");

    // 分录号重放：拒（不可改，只能冲销）
    let (st, body) = env
        .call(
            "POST",
            "/api/g14/commerce/journal",
            Some(&fin),
            Some(json!({
                "entry_no":"E1","memo":"补账","source":"manual",
                "lines":[
                    {"account":"user_wallet:u1","direction":"credit","amount_cents":100},
                    {"account":"platform_revenue","direction":"debit","amount_cents":100}
                ]
            })),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body["error"].as_str().unwrap().contains("已存在"), "{body}");

    let (st, body) = env
        .call("GET", "/api/g14/commerce/journal", Some(&fin), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["entries"].as_array().unwrap().len(), 2, "{body}");
}

#[tokio::test]
async fn recon_diff_resolution_converges_the_batch() {
    let mut env = Env::new("g14-04-recon");
    let fin = env.ready("fin").await;
    env.app
        .g14()
        .commerce
        .import_diff(&ReconDiff {
            id: "d1".into(),
            batch: "2026-10-23".into(),
            external_no: "out-1".into(),
            internal_no: "in-1".into(),
            kind: "amount_mismatch".into(),
            amount_delta_cents: -100,
            state: "open".into(),
            note: String::new(),
            resolved_by: None,
            resolved_at: None,
        })
        .unwrap();

    let (st, body) = env
        .call(
            "GET",
            "/api/g14/commerce/recon?batch=2026-10-23",
            Some(&fin),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["diffs"].as_array().unwrap().len(), 1, "{body}");

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/commerce/recon/d1/resolve",
            Some(&fin),
            Some(json!({"decision":"adjusted","note":"已补 1 元"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["diff"]["state"], "adjusted", "{body}");

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/commerce/recon/d1/resolve",
            Some(&fin),
            Some(json!({"decision":"adjusted","note":"重复"})),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("不能重复处理"),
        "{body}"
    );
}

#[tokio::test]
async fn csv_export_needs_pii_scope_and_blocks_formula_injection() {
    let mut env = Env::new("g14-04-export");
    // 订单 id 会进路径段（有形状校验），注入串放在商户可控的 external_no 上
    seed(&env, "m1", "o_csv", 1_000);
    env.app
        .g14()
        .commerce
        .ingest_order(&Order {
            id: "o_csv2".into(),
            merchant_id: "m1".into(),
            internal_no: "in-csv2".into(),
            external_no: "=cmd|'/c calc'!A1".into(),
            amount_cents: 2_000,
            currency: "CNY".into(),
            state: "paid".into(),
            created_at: now_ms(),
        })
        .expect("external_no 不该参与路径形状校验");
    let root = env.ready("root").await;
    let fin = env.token("fin");

    // 财务没有 ExportPii
    let (st, body) = env
        .call(
            "GET",
            "/api/g14/commerce/orders/export.csv",
            Some(&fin),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");

    let (st, body) = env
        .call(
            "GET",
            "/api/g14/commerce/orders/export.csv",
            Some(&root),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let csv = body.as_str().unwrap_or_default().to_string();
    assert!(csv.starts_with("order_id,"), "{csv}");
    assert!(csv.contains("'=cmd"), "公式必须被前缀化: {csv}");
}

#[tokio::test]
async fn refund_request_is_idempotent_and_capped_by_order_amount() {
    let mut env = Env::new("g14-04-cap");
    seed(&env, "m1", "o11", 5_000);
    let fin = env.token("fin");

    let (st, body) = env
        .call(
            "POST",
            "/api/g14/commerce/orders/o11/refunds",
            Some(&fin),
            Some(json!({"amount_cents":9_999,"reason":"超额","idem":"k"})),
        )
        .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let (st, a) = env
        .call(
            "POST",
            "/api/g14/commerce/orders/o11/refunds",
            Some(&fin),
            Some(json!({"amount_cents":1_000,"reason":"少发","idem":"k"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    let (_, b): (StatusCode, Value) = env
        .call(
            "POST",
            "/api/g14/commerce/orders/o11/refunds",
            Some(&fin),
            Some(json!({"amount_cents":1_000,"reason":"少发","idem":"k"})),
        )
        .await;
    assert_eq!(
        a["refund"]["id"], b["refund"]["id"],
        "同 idem 不能建第二张单"
    );
}
