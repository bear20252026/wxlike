//! 服务化外壳的验收：能不能被 supervisor **正确停止**。
//!
//! 这一层和业务无关，但错了会留下真实的损坏（SQLite 停在半检查点状态、
//! 在飞请求被腰斩）。两个测试分工：
//!
//! * `shutdown_stops_accepting`（跨平台）：调 `Stack::shutdown` 后端口不再
//!   accept，且调用在上限内返回——证明 graceful shutdown 真的挂上了。
//! * `sigterm_exits_zero`（仅 Unix）：起**真进程**、发**真 SIGTERM**、
//!   断言退出码 0 且日志出现"graceful shutdown 完成"。这是 launchd/systemd
//!   实际会走的路径，`tower::oneshot` 之类的进程内测试完全覆盖不到。

use std::time::Duration;

use wxlike_server::{start_stack, StackOptions};

fn opts() -> StackOptions {
    StackOptions {
        hub_id: "test".into(),
        db_path: None,
        mq_mode: "memory".into(),
        http_bind: "127.0.0.1:0".parse().unwrap(),
        quic_bind: "127.0.0.1:0".parse().unwrap(),
    }
}

#[tokio::test]
async fn shutdown_stops_accepting() {
    let stack = start_stack(opts()).await.expect("stack 应能启动");
    let addr = stack.http_addr;

    // 起来就能连
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_ok(),
        "启动后应能连上 HTTP"
    );

    tokio::time::timeout(
        Duration::from_secs(5),
        stack.shutdown(Duration::from_secs(5)),
    )
    .await
    .expect("shutdown 必须在上限内返回（否则 supervisor 会一直等）");

    // 给监听器一点时间真正 drop
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_err(),
        "关闭后不应再接受新连接"
    );
}

#[cfg(unix)]
mod unix {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// 真进程 + 真 SIGTERM。
    #[test]
    fn sigterm_exits_zero_and_logs_graceful_shutdown() {
        let exe = env!("CARGO_BIN_EXE_wxlike-server");
        let media = std::env::temp_dir().join(format!("wxlike-grace-{}", std::process::id()));
        let mut child = Command::new(exe)
            .env("WXLIKE_DB", ":memory:")
            .env("WXLIKE_HTTP", "127.0.0.1:0")
            .env("WXLIKE_QUIC", "127.0.0.1:0")
            .env("WXLIKE_MEDIA_DIR", &media)
            // 推流密钥缺失时二进制会**拒绝启动**（安全审计要求）。
            // 测试用显式开发开关放行——这也顺带证明了那条拒绝逻辑存在。
            .env("WXLIKE_DEV_INSECURE", "1")
            .env("WXLIKE_LOG", "info")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动服务进程");

        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

        // 等"监听已就绪"的那行
        let mut log: Vec<String> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut up = false;
        while Instant::now() < deadline && !up {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    up = line.contains("HTTP  on");
                    log.push(line);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
        assert!(up, "服务没起来，日志：{log:#?}");

        // 发 SIGTERM（launchctl bootout / systemctl stop / docker stop 都是它）
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }

        // 等退出（上限 15s，超过就强杀并判失败）
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            match child.try_wait().expect("try_wait") {
                Some(s) => break s,
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    panic!("SIGTERM 后 15s 仍未退出，日志：{log:#?}");
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        };

        // 收尾：把剩余日志读出来
        while let Ok(line) = rx.recv_timeout(Duration::from_millis(100)) {
            log.push(line);
        }

        assert_eq!(
            status.code(),
            Some(0),
            "正常停止应退 0（KeepAlive.SuccessfulExit=false 靠这个区分崩溃），日志：{log:#?}"
        );
        assert!(
            log.iter().any(|l| l.contains("graceful shutdown 完成")),
            "日志里应有优雅关闭完成标记：{log:#?}"
        );
    }

    /// `--check` 只打印配置就退出：部署前自检用。
    #[test]
    fn check_flag_prints_config_and_exits() {
        let exe = env!("CARGO_BIN_EXE_wxlike-server");
        let out = Command::new(exe)
            .arg("--check")
            .env("WXLIKE_HTTP", "127.0.0.1:1234")
            .output()
            .expect("运行 --check");
        assert!(out.status.success(), "--check 应退 0");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("配置自检"), "输出：{text}");
        assert!(
            text.contains("127.0.0.1:1234"),
            "应回显解析后的绑定地址：{text}"
        );
    }
}
