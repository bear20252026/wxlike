//! `wxlike-cmd`：命令目录与权限的**自检工具**（不是执行器）。
//!
//! 两个子命令，回答两个运维问题：
//! * `list`  —— 有哪些命令、各自多危险；
//! * `policy` —— 按当前环境变量，**每条命令到底能不能执行**，不能的话为什么。
//!
//! 第二个是重点：给 agent 配好权限后，人需要一条命令确认"它到底能干什么"，
//! 而不是等 agent 撞墙。输出与 `--check` 同风格：直接、可复制。

use std::process::ExitCode;

use wxlike_cmd::policy::Policy;
use wxlike_cmd::{search, CATALOG};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("list") => {
            let query = args.get(1).map(String::as_str).unwrap_or("");
            list(query);
            ExitCode::SUCCESS
        }
        Some("policy") => {
            policy();
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("用法:");
            eprintln!("  wxlike-cmd list [关键词]   列出命令（id / 能力 / 说明）");
            eprintln!("  wxlike-cmd policy          按当前环境变量打印每条命令的判定");
            eprintln!();
            eprintln!("权限来自环境变量：");
            eprintln!("  WXLIKE_AGENT_ALLOW_CONV=模式[,模式…]   会话白名单，* 通配；空=全拒");
            eprintln!("  WXLIKE_AGENT_ALLOW_WRITE=1             允许写命令");
            eprintln!("  WXLIKE_AGENT_ALLOW_DESTRUCTIVE=1       允许破坏性命令");
            ExitCode::FAILURE
        }
    }
}

fn list(query: &str) {
    let specs = search(query);
    println!("{} 条命令（共 {} 条）", specs.len(), CATALOG.len());
    println!();
    println!("{:<20} {:<12} 说明", "ID", "能力");
    for s in specs {
        println!("{:<20} {:<12} {}", s.id, s.capability, s.summary);
    }
}

fn policy() {
    let p = Policy::from_env();
    let convs = p.conv_patterns();
    println!("权限控制中心 · 当前生效策略");
    println!(
        "  会话白名单   {}",
        if convs.is_empty() {
            "（空 = 任何会话都不允许）".to_string()
        } else {
            convs.join(", ")
        }
    );
    println!("  写权限       {}", yes_no(p.allow_write()));
    println!("  破坏性权限   {}", yes_no(p.allow_destructive()));
    println!();
    println!("{:<20} {:<12} 判定", "ID", "能力");
    let mut allowed = 0usize;
    for s in CATALOG {
        // 用一个占位会话试权限：只关心"这条命令在当前策略下能不能过"。
        let probe = serde_json::json!({
            "conv": probe_conv(&p),
            "to_conv": probe_conv(&p),
            "from_conv": probe_conv(&p),
        });
        match p.authorize(s.id, &probe) {
            Ok(_) => {
                allowed += 1;
                println!("{:<20} {:<12} 允许", s.id, s.capability);
            }
            Err(err) => println!("{:<20} {:<12} 拒绝 — {err}", s.id, s.capability),
        }
    }
    println!();
    println!("可执行 {allowed} / {} 条", CATALOG.len());
}

/// 拿白名单里的第一个模式当探针；没有白名单就随便给一个必然被拒的 id。
fn probe_conv(p: &Policy) -> String {
    match p.conv_patterns().first() {
        Some(pat) if !pat.contains('*') => pat.clone(),
        Some(pat) => pat.replace('*', "probe"),
        None => "probe".to_string(),
    }
}

fn yes_no(v: bool) -> &'static str {
    if v {
        "开"
    } else {
        "关"
    }
}
