//! 传输层：只负责"读一行、写一行"，协议语义在 [`crate::server`]。
//!
//! 现在只有 stdio 一种。**远程 transport 故意不在这里开**：
//! MCP 安全最佳实践要求远程端做 audience/主体/会话/Origin 绑定，那些是
//! G15-02 的交付物；先放一个没有授权的 HTTP 监听，比没有更糟。
//! 需要远程时，把 `--transport http` 的申请交给 G00（见
//! `integration-requests/G15/G15-01.yaml`），在 G15-02 的鉴权合入后再开。

use std::io::{BufRead as _, Write as _};

use crate::server::Server;

/// 支持的传输方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// 标准输入输出，一行一帧（本地 agent 的默认形态）。
    Stdio,
}

impl Transport {
    /// 解析 `--transport` 的值。不认识的一律**启动失败**，
    /// 不静默退回 stdio——那会让"我配了 http 其实没生效"变成谜。
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "stdio" => Ok(Transport::Stdio),
            other => Err(format!(
                "不支持的 transport {other:?}：当前只有 stdio；\
                 远程 transport 需要 G15-02 的会话与 audience 鉴权，见 \
                 integration-requests/G15/G15-01.yaml"
            )),
        }
    }
}

/// 在 stdio 上跑循环，直到 stdin 结束。
pub fn serve_stdio(server: &mut Server) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = server.handle_line(&line) {
            if writeln!(stdout, "{resp}").is_err() {
                break;
            }
            let _ = stdout.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_stdio_is_accepted_and_others_fail_loudly() {
        assert_eq!(Transport::parse("stdio").unwrap(), Transport::Stdio);
        for bad in ["http", "sse", ""] {
            let err = Transport::parse(bad).unwrap_err();
            assert!(err.contains("G15-02"), "错误要指出找谁：{err}");
        }
    }
}
