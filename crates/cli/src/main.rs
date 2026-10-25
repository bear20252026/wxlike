//! `wxlike-cli`：命令面的**人类与脚本入口**。
//!
//! 它做的事和 MCP server 一样——把 `wxlike-cmd` 的命令 id 翻译成 HTTP 调用，
//! 权限一律先过 `policy.authorize`。区别只在协议形态：这里是命令行，MCP 是
//! stdio JSON-RPC。**两者共用同一个 catalog 与同一个 Policy**，不重复实现。
//!
//! 用法：
//! ```text
//! wxlike list [关键词]                                     # 命令表
//! wxlike help video                                        # 某域帮助（含参数与类型）
//! wxlike msg unread --param conv='d-a~b' --json            # 域 + 动作
//! wxlike video job get --param job_id=j1
//! wxlike inspect | policy | doctor
//! wxlike run --cmd 'conv.unread={"conv":"d-a~b"}'          # 老写法，仍可用
//! ```
//!
//! 退出码见 [`run`] 模块文档——**它是脚本的契约**，不要随便改。

mod args;
mod run;

use std::process::ExitCode;

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match args::parse(&raw) {
        Ok(invocation) => run::execute(invocation),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(run::EXIT_USAGE)
        }
    }
}
