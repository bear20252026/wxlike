//! 网络诊断的端到端验收：对**真 hub** 跑 DNS → TCP → HTTP → QUIC 四步。
//!
//! 单元测试用的是假 HTTP 服务（`crates/net/src/diag.rs`），只能证明每一步的
//! 逻辑对；这里要证明的是"对着我们自己的服务端，四步都能过"——尤其是 QUIC 那步
//! 的 `hello`（服务端认这个用户），以及 `/health` 真的存在。

use std::time::Duration;

use serde_json::Value;
use wxlike_net::diag::{DiagOptions, DiagTarget, Step};
use wxlike_server::{start_stack, StackOptions};

fn opts(name: &str) -> StackOptions {
    let dir = std::env::temp_dir().join(format!("wxlike-diag-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("wxlike.db");
    StackOptions {
        hub_id: "diag".into(),
        db_path: Some(path.to_string_lossy().to_string()),
        mq_mode: "memory".into(),
        http_bind: "127.0.0.1:0".parse().unwrap(),
        quic_bind: "127.0.0.1:0".parse().unwrap(),
    }
}

fn register(base: &str, name: &str) -> String {
    let reply: Value = ureq::post(&format!("{base}/api/register"))
        .send_json(serde_json::json!({"name": name, "password": "pw"}))
        .expect("register")
        .into_json()
        .unwrap();
    reply["user_id"].as_str().expect("user_id").to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn all_four_steps_pass_against_a_real_hub() {
    let stack = start_stack(opts("ok")).await.expect("stack");
    let base = format!("http://{}", stack.http_addr);
    let alice = register(&base, "alice");

    let pins = std::env::temp_dir().join(format!("wxlike-diag-pins-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&pins);

    let target = DiagTarget::new("127.0.0.1", stack.http_addr.port(), stack.quic_addr.port());
    let mut o = DiagOptions::new(&pins);
    o.per_step_timeout = Duration::from_secs(5);
    o.user_id = Some(alice.clone());

    let report = wxlike_net::diag::run(&target, &o).await;
    assert!(report.ok(), "四步应当全过：{report:#?}");
    assert_eq!(report.steps.len(), 4);
    assert_eq!(report.steps[0].step, Step::Dns);
    assert_eq!(report.steps[3].step, Step::Quic);

    // HTTP 那步要看到真实状态码
    assert!(
        report.steps[2].status.detail().contains("200"),
        "{}",
        report.steps[2].status.detail()
    );
    // QUIC 那步要走完 hello（给了 user_id 就必须证明服务端认这个用户）
    assert!(
        report.steps[3].status.detail().contains("hello"),
        "{}",
        report.steps[3].status.detail()
    );
    assert!(report.summary.contains("全部通过"), "{}", report.summary);

    let _ = std::fs::remove_file(&pins);
}

/// 数据面端口写错时，只有 QUIC 那步红——前三个控制面步骤不受影响。
/// 这正是"卡在哪一跳"的价值：不然用户只会看到"发送失败"。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_quic_port_fails_only_the_last_step() {
    let stack = start_stack(opts("badquic")).await.expect("stack");
    let base = format!("http://{}", stack.http_addr);
    let _ = register(&base, "alice");

    let pins = std::env::temp_dir().join(format!("wxlike-diag-pins2-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&pins);

    // 一个刚释放掉的端口：几乎必然没人听
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let target = DiagTarget::new("127.0.0.1", stack.http_addr.port(), dead);
    let mut o = DiagOptions::new(&pins);
    o.per_step_timeout = Duration::from_secs(2);

    let report = wxlike_net::diag::run(&target, &o).await;
    assert!(report.steps[0].status.is_ok());
    assert!(report.steps[1].status.is_ok());
    assert!(report.steps[2].status.is_ok());
    assert!(!report.steps[3].status.is_ok(), "{:?}", report.steps[3]);
    assert_eq!(report.first_failure().map(|s| s.step), Some(Step::Quic));
    assert!(report.summary.contains("quic"), "{}", report.summary);

    let _ = std::fs::remove_file(&pins);
}
