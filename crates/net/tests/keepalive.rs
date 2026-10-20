//! QUIC 保活：连接不能因为"闲着"就死。
//!
//! quinn 0.11 的默认值 `max_idle_timeout = 30s` + `keep_alive_interval = None`
//! 意味着**空闲 30 秒连接就断，且不发包维持 NAT 映射**。桌面端勉强能用，
//! 移动端一进后台必断（运营商 UDP NAT 常见 30–120 秒过期）。
//!
//! 这条测试就是那个缺陷的回归防线：等 40 秒（超过默认的 30 秒），连接必须还活着。
//! 代价是 40 秒测试时长——但"手机放兜里 30 秒消息就收不到"是必须被抓住的问题。
//!
//! 参照 `refs/mars/mars/stn/config.h`：Mars 的应用层心跳区间是 3.5–10 分钟
//! （TCP 长连，没有 keepalive 选项可用）；QUIC 只能靠 PING 帧，所以取 30 秒更保守。

use std::time::Duration;

use wxlike_net::transport::{connect_tofu, make_server_endpoint};

/// 默认 idle 是 30 秒。等 40 秒：默认配置下连接必已关闭。
const BEYOND_DEFAULT_IDLE: Duration = Duration::from_secs(40);

fn hub() -> (quinn::Endpoint, std::net::SocketAddr) {
    let ep = make_server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = ep.local_addr().unwrap();
    let accept_ep = ep.clone();
    tokio::spawn(async move {
        // 必须持续 accept 并把连接留住，否则 drop 掉就等于服务端主动关闭
        let mut held = Vec::new();
        while let Some(incoming) = accept_ep.accept().await {
            if let Ok(conn) = incoming.await {
                held.push(conn);
            }
        }
    });
    (ep, addr)
}

#[tokio::test]
async fn connection_survives_the_default_idle_timeout() {
    let pins = std::env::temp_dir().join(format!("wxlike-keepalive-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&pins);
    let (_hub, addr) = hub();

    let conn = connect_tofu(addr, "localhost", &pins)
        .await
        .expect("connect");
    tokio::time::sleep(BEYOND_DEFAULT_IDLE).await;

    assert!(
        conn.close_reason().is_none(),
        "空闲 {:?} 后连接被关闭：{:?}——保活没生效（默认 30s 空闲即断）",
        BEYOND_DEFAULT_IDLE,
        conn.close_reason()
    );

    // 不只是"没关"，还得真能收发：打开一条流并写一点数据
    let (mut send, _recv) = conn.open_bi().await.expect("open stream after idle");
    send.write_all(b"still here")
        .await
        .expect("write after idle");
    send.finish().expect("finish");

    let _ = std::fs::remove_file(&pins);
}
