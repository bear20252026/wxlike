//! issue #12：QUIC 证书要**持久化**，否则每次重启都像中间人攻击。
//!
//! 客户端按 TOFU 记住叶证书的 SHA-256 指纹（`TofuPins`）。服务端每次启动
//! 重新生成自签证书，指纹就变了——而 `check_or_pin` 对"指纹变了"的反应是
//! **拒绝连接**，与真的遇到 MITM 完全一样。运维看到的是"服务重启后所有客户端
//! 都连不上"，日志里只有一句 certificate fingerprint changed。
//!
//! 四条断言：
//!
//! 1. 同一个目录重启 → 证书与私钥逐字节相同；
//! 2. 不同目录 → 不同证书（不是全局单例）；
//! 3. 只有一半材料 → **报错**，不猜；
//! 4. 目录不可写 → 报错（不静默退回"每次新建"）。

use std::path::PathBuf;

use wxlike_net::transport::make_server_endpoint_persistent;

fn temp_dir(tag: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "wxlike-cert-{}-{}-{tag}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn local() -> std::net::SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// 1) 同目录重启：证书与私钥必须逐字节相同。
#[tokio::test]
async fn cert_and_key_are_reused_after_restart() {
    let dir = temp_dir("reuse");

    let first = make_server_endpoint_persistent(local(), &dir).expect("首次生成");
    let cert1 = std::fs::read(dir.join("quic-cert.der")).expect("cert 落盘");
    let key1 = std::fs::read(dir.join("quic-key.der")).expect("key 落盘");
    assert!(!cert1.is_empty() && !key1.is_empty());
    drop(first);

    // 模拟重启：同一个目录再起一次
    let second = make_server_endpoint_persistent(local(), &dir).expect("重启后复用");
    let cert2 = std::fs::read(dir.join("quic-cert.der")).unwrap();
    let key2 = std::fs::read(dir.join("quic-key.der")).unwrap();
    drop(second);

    assert_eq!(
        cert1, cert2,
        "重启后证书变了 —— 客户端 pin 会全部失效，表现与中间人攻击相同"
    );
    assert_eq!(key1, key2, "重启后私钥变了");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 2) 不同目录 → 不同证书（说明不是进程级单例，而是跟着数据目录走）。
#[tokio::test]
async fn different_dirs_get_different_certs() {
    let a = temp_dir("a");
    let b = temp_dir("b");
    let _ea = make_server_endpoint_persistent(local(), &a).unwrap();
    let _eb = make_server_endpoint_persistent(local(), &b).unwrap();

    let cert_a = std::fs::read(a.join("quic-cert.der")).unwrap();
    let cert_b = std::fs::read(b.join("quic-cert.der")).unwrap();
    assert_ne!(cert_a, cert_b, "两个数据目录不该共用同一张证书");

    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}

/// 3) 只有一半材料 → 报错。**不能猜**：猜"缺哪个补哪个"会生成新证书，
///    让所有客户端被 pin 拒绝，而日志里看不出原因。
#[tokio::test]
async fn half_material_is_an_error_not_a_guess() {
    let dir = temp_dir("half");
    let _ep = make_server_endpoint_persistent(local(), &dir).unwrap();
    std::fs::remove_file(dir.join("quic-key.der")).unwrap();

    let err = match make_server_endpoint_persistent(local(), &dir) {
        Ok(_) => panic!("只剩证书时不该静默重建"),
        Err(err) => format!("{err:#}"),
    };
    assert!(
        err.contains("incomplete") && err.contains("cert=true"),
        "错误要说清是材料不完整、缺的是哪一半：{err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
