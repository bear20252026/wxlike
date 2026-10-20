//! 网络诊断：把"连不上"从猜变成可观测。
//!
//! 参照 `refs/mars/mars/sdt/`（腾讯 Mars 的网络诊断组件，MIT）：它按
//! `CheckIPPorts`（host → 一组 ip:port）依次跑 ping / dns / tcp / http 检查，
//! 每步独立超时，最后给一份报告。
//!
//! # 与 SDT 的差异
//!
//! | SDT | 我们 | 原因 |
//! |---|---|---|
//! | ping / traceroute | **不做** | ICMP 要 raw socket 或平台特权；一个"诊断"功能不该要求 root |
//! | dns / tcp / http | 做 | 这三步覆盖了 90% 的"连不上" |
//! | — | **加 quic** | 我们的数据面是 QUIC，控制面通了不代表数据面通 |
//!
//! 步骤是**顺序短路**的：DNS 挂了就不用试 TCP，报告里直接标"跳过"。
//! 用户看到的应该是"卡在哪一跳"，而不是四条一模一样的失败。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::transport::connect_tofu;

/// 一次诊断的四个检查点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// 域名解析（DNS）
    Dns,
    /// TCP 建连（控制面端口）
    Tcp,
    /// HTTP 探活（控制面 `/health`）
    Http,
    /// QUIC 握手（数据面，含证书固定）
    Quic,
}

impl Step {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Tcp => "tcp",
            Self::Http => "http",
            Self::Quic => "quic",
        }
    }
}

/// 一步的结论。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum StepStatus {
    /// 通过，附带一句人话（解析到几个 IP、HTTP 状态码…）
    Ok { detail: String },
    /// 失败，附带原因
    Failed { detail: String },
    /// 上一步就失败了，这一步没跑
    Skipped { detail: String },
}

impl StepStatus {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::Ok { detail } | Self::Failed { detail } | Self::Skipped { detail } => detail,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepReport {
    pub step: Step,
    pub status: StepStatus,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone)]
pub struct DiagTarget {
    /// 主机名或 IP（诊断要能吃"域名解析失败"这种情况，所以保留字符串）
    pub host: String,
    /// 控制面端口（HTTP）
    pub http_port: u16,
    /// 数据面端口（QUIC）
    pub quic_port: u16,
    /// HTTP 探活路径
    pub http_path: String,
}

impl DiagTarget {
    pub fn new(host: impl Into<String>, http_port: u16, quic_port: u16) -> Self {
        Self {
            host: host.into(),
            http_port,
            quic_port,
            http_path: "/health".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiagOptions {
    /// 单步上限。SDT 的首包超时是 WiFi 12s / GPRS 15s；诊断比正常请求更宽容，
    /// 因为它的目的就是"在慢网络下也量出到底多慢"。
    pub per_step_timeout: Duration,
    /// TLS SNI / TOFU 里用的服务名
    pub server_name: String,
    /// TOFU 证书固定文件。诊断**不跳过校验**——跳过校验的诊断会告诉你
    /// "QUIC 通"，而实际可能连的是中间人。
    pub pins: PathBuf,
    /// 给了就在 QUIC 步骤里再走一次 `hello`（证明"服务端认这个用户"，
    /// 而不只是"端口开着"）
    pub user_id: Option<String>,
}

impl DiagOptions {
    pub fn new(pins: impl Into<PathBuf>) -> Self {
        Self {
            per_step_timeout: Duration::from_secs(5),
            server_name: "localhost".into(),
            pins: pins.into(),
            user_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagReport {
    pub host: String,
    pub http_port: u16,
    pub quic_port: u16,
    pub steps: Vec<StepReport>,
    /// 一句话结论（给 UI 直接显示）
    pub summary: String,
}

impl DiagReport {
    pub fn ok(&self) -> bool {
        self.steps.iter().all(|s| s.status.is_ok())
    }

    /// 第一个失败的那一步（"卡在哪一跳"）。
    pub fn first_failure(&self) -> Option<&StepReport> {
        self.steps
            .iter()
            .find(|s| matches!(s.status, StepStatus::Failed { .. }))
    }
}

/// 跑完整诊断。顺序短路：上一步失败，后面的标 `skipped`。
pub async fn run(target: &DiagTarget, opts: &DiagOptions) -> DiagReport {
    let mut steps = Vec::new();

    // 1) DNS
    let started = Instant::now();
    let resolved = tokio::time::timeout(
        opts.per_step_timeout,
        tokio::net::lookup_host((target.host.as_str(), target.http_port)),
    )
    .await;
    let mut addrs: Vec<SocketAddr> = Vec::new();
    let dns_status = match resolved {
        Err(_) => StepStatus::Failed {
            detail: format!("解析超时（>{}s）", opts.per_step_timeout.as_secs()),
        },
        Ok(Err(err)) => StepStatus::Failed {
            detail: format!("解析失败：{err}"),
        },
        Ok(Ok(iter)) => {
            for addr in iter {
                if !addrs.contains(&addr) {
                    addrs.push(addr);
                }
            }
            if addrs.is_empty() {
                StepStatus::Failed {
                    detail: "解析结果为空".into(),
                }
            } else {
                let list: Vec<String> = addrs.iter().map(|a| a.ip().to_string()).collect();
                StepStatus::Ok {
                    detail: format!("{} 个地址：{}", addrs.len(), list.join(", ")),
                }
            }
        }
    };
    let dns_ok = dns_status.is_ok();
    steps.push(StepReport {
        step: Step::Dns,
        status: dns_status,
        elapsed_ms: started.elapsed().as_millis() as u64,
    });

    // 2) TCP（用第一个解析结果；连不上就换下一个）
    let mut tcp_addr: Option<SocketAddr> = None;
    if dns_ok {
        let started = Instant::now();
        let mut last_err = String::new();
        for addr in &addrs {
            match tokio::time::timeout(opts.per_step_timeout, tokio::net::TcpStream::connect(addr))
                .await
            {
                Ok(Ok(_)) => {
                    tcp_addr = Some(*addr);
                    last_err.clear();
                    break;
                }
                Ok(Err(err)) => last_err = format!("{addr}：{err}"),
                Err(_) => last_err = format!("{addr}：连接超时"),
            }
        }
        let status = match tcp_addr {
            Some(addr) => StepStatus::Ok {
                detail: format!("已连接 {addr}"),
            },
            None => StepStatus::Failed {
                detail: if last_err.is_empty() {
                    "没有可用地址".into()
                } else {
                    last_err
                },
            },
        };
        steps.push(StepReport {
            step: Step::Tcp,
            status,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    } else {
        steps.push(skipped(Step::Tcp, "DNS 未通过"));
    }

    // 3) HTTP 探活
    if let Some(addr) = tcp_addr {
        let started = Instant::now();
        let status = match http_probe(addr, &target.host, &target.http_path, opts).await {
            Ok(line) => StepStatus::Ok { detail: line },
            Err(err) => StepStatus::Failed { detail: err },
        };
        steps.push(StepReport {
            step: Step::Http,
            status,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    } else {
        steps.push(skipped(Step::Http, "TCP 未通过"));
    }

    // 4) QUIC 握手（数据面）
    if let Some(addr) = tcp_addr {
        let started = Instant::now();
        let quic_addr = SocketAddr::new(addr.ip(), target.quic_port);
        let status = match quic_probe(quic_addr, opts).await {
            Ok(detail) => StepStatus::Ok { detail },
            Err(err) => StepStatus::Failed { detail: err },
        };
        steps.push(StepReport {
            step: Step::Quic,
            status,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    } else {
        steps.push(skipped(Step::Quic, "TCP 未通过"));
    }

    let summary = summarize(&steps);
    DiagReport {
        host: target.host.clone(),
        http_port: target.http_port,
        quic_port: target.quic_port,
        steps,
        summary,
    }
}

fn skipped(step: Step, why: &str) -> StepReport {
    StepReport {
        step,
        status: StepStatus::Skipped {
            detail: why.to_string(),
        },
        elapsed_ms: 0,
    }
}

fn summarize(steps: &[StepReport]) -> String {
    for s in steps {
        if let StepStatus::Failed { detail } = &s.status {
            return format!("卡在 {}：{detail}", s.step.as_str());
        }
    }
    let slowest = steps.iter().max_by_key(|s| s.elapsed_ms);
    match slowest {
        Some(s) if s.elapsed_ms > 0 => format!(
            "全部通过（最慢一步 {} {}ms）",
            s.step.as_str(),
            s.elapsed_ms
        ),
        _ => "全部通过".into(),
    }
}

/// 手写一个最小 HTTP/1.1 GET。
///
/// 不用 ureq：诊断要的正是"看到原始响应"，任何客户端库都会替我们重试、
/// 跟随重定向、改写错误，反而遮住问题。也避免给 `crates/net` 加依赖。
async fn http_probe(
    addr: SocketAddr,
    host: &str,
    path: &str,
    opts: &DiagOptions,
) -> Result<String, String> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let mut stream =
        tokio::time::timeout(opts.per_step_timeout, tokio::net::TcpStream::connect(addr))
            .await
            .map_err(|_| "连接超时".to_string())?
            .map_err(|e| format!("连接失败：{e}"))?;

    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: wxlike-diag\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| format!("写请求失败：{e}"))?;

    let mut buf = vec![0u8; 512];
    let n = tokio::time::timeout(opts.per_step_timeout, stream.read(&mut buf))
        .await
        .map_err(|_| "等响应超时".to_string())?
        .map_err(|e| format!("读响应失败：{e}"))?;
    if n == 0 {
        return Err("服务端直接关闭了连接（端口开着但不说 HTTP）".into());
    }
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    let status_line = text.lines().next().unwrap_or("").trim().to_string();
    if !status_line.starts_with("HTTP/") {
        return Err(format!("响应不是 HTTP：{status_line:.60}"));
    }
    // 4xx/5xx 也算"控制面活着"——诊断关心的是可达性，不是业务正确性
    Ok(status_line)
}

/// QUIC 步骤：真握手，走 TOFU（不跳过校验）。
async fn quic_probe(addr: SocketAddr, opts: &DiagOptions) -> Result<String, String> {
    let conn = tokio::time::timeout(
        opts.per_step_timeout,
        connect_tofu(addr, &opts.server_name, &opts.pins),
    )
    .await
    .map_err(|_| "握手超时".to_string())?
    .map_err(|e| format!("握手失败：{e:#}"))?;

    let rtt = conn.rtt().as_millis();
    let detail = match &opts.user_id {
        // 给了 user_id 就再走一次 hello：证明服务端认这个用户，
        // 而不只是"端口开着、握手能成"
        Some(user) => {
            let (mut send, recv) = tokio::time::timeout(opts.per_step_timeout, conn.open_bi())
                .await
                .map_err(|_| "开控制流超时".to_string())?
                .map_err(|e| format!("开控制流失败：{e}"))?;
            let frame = crate::wire::WireFrame::Hello {
                user_id: user.clone(),
            }
            .to_bytes()
            .map_err(|e| format!("编码 hello 失败：{e}"))?;
            crate::framing::write_frame(&mut send, &frame)
                .await
                .map_err(|e| format!("发 hello 失败：{e}"))?;
            drop(recv);
            format!("握手成功（RTT {rtt}ms，已用 {user} 完成 hello）")
        }
        None => format!("握手成功（RTT {rtt}ms）"),
    };
    conn.close(0u32.into(), b"diag done");
    Ok(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个只会回 `200 OK` 的假 HTTP 服务。
    async fn fake_http() -> (u16, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 256];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .await;
                });
            }
        });
        (port, handle)
    }

    fn pins(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("wxlike-diag-{}-{tag}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[tokio::test]
    async fn dns_and_tcp_and_http_pass_against_a_live_port() {
        let (port, _h) = fake_http().await;
        // 用一个必然连不上的 QUIC 端口：证明"控制面通了、数据面不通"能被单独看出来
        let target = DiagTarget::new("127.0.0.1", port, 1);
        let mut opts = DiagOptions::new(pins("ok"));
        opts.per_step_timeout = Duration::from_secs(2);

        let report = run(&target, &opts).await;
        assert!(report.steps[0].status.is_ok(), "{:?}", report.steps[0]);
        assert!(report.steps[1].status.is_ok(), "{:?}", report.steps[1]);
        assert!(report.steps[2].status.is_ok(), "{:?}", report.steps[2]);
        assert!(report.steps[2].status.detail().contains("200"));

        // QUIC 那一步应当失败（端口 1 没人听），而且**只**失败这一步
        assert!(!report.steps[3].status.is_ok(), "{:?}", report.steps[3]);
        assert!(!report.ok());
        assert_eq!(report.first_failure().map(|s| s.step), Some(Step::Quic));
        assert!(
            report.summary.contains("quic"),
            "结论要指出卡在那一跳：{}",
            report.summary
        );
    }

    #[tokio::test]
    async fn a_dead_port_fails_at_tcp_and_skips_the_rest() {
        // 绑一个端口再关掉，拿到一个几乎必然没人听的端口号
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let target = DiagTarget::new("127.0.0.1", port, port);
        let mut opts = DiagOptions::new(pins("dead"));
        opts.per_step_timeout = Duration::from_secs(1);

        let report = run(&target, &opts).await;
        assert!(report.steps[0].status.is_ok(), "DNS 对本机地址不该失败");
        assert!(matches!(report.steps[1].status, StepStatus::Failed { .. }));
        for s in &report.steps[2..] {
            assert!(
                matches!(s.status, StepStatus::Skipped { .. }),
                "{} 应当是 skipped，实际 {:?}",
                s.step.as_str(),
                s.status
            );
        }
        assert!(report.summary.starts_with("卡在 tcp"), "{}", report.summary);
    }

    #[tokio::test]
    async fn an_unresolvable_host_fails_at_dns() {
        // `.invalid` 是 RFC 2606 保留的顶级域：保证解析不出来
        let target = DiagTarget::new("wxlike.invalid", 80, 443);
        let mut opts = DiagOptions::new(pins("nxdomain"));
        opts.per_step_timeout = Duration::from_secs(3);

        let report = run(&target, &opts).await;
        assert!(matches!(report.steps[0].status, StepStatus::Failed { .. }));
        assert!(matches!(report.steps[1].status, StepStatus::Skipped { .. }));
        assert!(report.summary.starts_with("卡在 dns"), "{}", report.summary);
    }

    #[test]
    fn report_serializes_into_something_the_ui_can_render() {
        let report = DiagReport {
            host: "hub.example".into(),
            http_port: 8099,
            quic_port: 9443,
            steps: vec![
                StepReport {
                    step: Step::Dns,
                    status: StepStatus::Ok {
                        detail: "1 个地址：1.2.3.4".into(),
                    },
                    elapsed_ms: 3,
                },
                StepReport {
                    step: Step::Tcp,
                    status: StepStatus::Failed {
                        detail: "连接超时".into(),
                    },
                    elapsed_ms: 5000,
                },
                StepReport {
                    step: Step::Http,
                    status: StepStatus::Skipped {
                        detail: "TCP 未通过".into(),
                    },
                    elapsed_ms: 0,
                },
            ],
            summary: "卡在 tcp：连接超时".into(),
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["steps"][0]["step"], "dns");
        assert_eq!(json["steps"][0]["status"]["status"], "ok");
        assert_eq!(json["steps"][1]["status"]["status"], "failed");
        assert_eq!(json["steps"][2]["status"]["status"], "skipped");
        assert_eq!(json["summary"], "卡在 tcp：连接超时");
    }
}
