//! `wxlike-cli`：命令面的**人类入口**（P1）。
//!
//! 它做的事和将来的 MCP server 一样——把 `wxlike-cmd` 的命令 id 翻译成 HTTP
//! 调用，权限一律先过 `policy.authorize`。区别只在协议：这里是命令行，MCP 是
//! stdio JSON-RPC。**两者共用同一个 catalog 与同一个 Policy**，不重复实现。
//!
//! 用法：
//! ```text
//! wxlike-cli --server http://127.0.0.1:8099 --token $T list
//! wxlike-cli ... inspect
//! wxlike-cli ... run --cmd 'conv.unread={"conv":"d-a~b"}'
//! wxlike-cli policy
//! ```

use std::process::ExitCode;

use serde_json::{json, Value};
use wxlike_cmd::policy::Policy;
use wxlike_cmd::{search, CATALOG};

/// 命令执行器：只认识 `wxlike-client`，不认识 UI。
struct Exec<'a> {
    http: wxlike_client::Http,
    policy: &'a Policy,
}

impl Exec<'_> {
    fn run(&self, id: &str, params: &Value) -> Result<Value, String> {
        // **权限先于网络**：被拒的命令不该产生任何请求。
        let spec = self
            .policy
            .authorize(id, params)
            .map_err(|e| e.to_string())?;
        match spec.id {
            "me.info" => self.http.expect_json("GET", "/api/me", None),
            "conv.unread" => self.http.conv_unread(str_param(params, "conv")?),
            "conv.sync" => self.http.conv_sync(
                str_param(params, "conv")?,
                params["version"].as_u64().unwrap_or(0),
            ),
            "conv.history" => {
                let conv = str_param(params, "conv")?;
                let before = params["before_seq"].as_i64().unwrap_or(0);
                let limit = params["limit"].as_u64().unwrap_or(50);
                self.http.expect_json(
                    "GET",
                    &format!("/api/history/{conv}?before_seq={before}&limit={limit}"),
                    None,
                )
            }
            "msg.search" => {
                let q = str_param(params, "q")?;
                let limit = params["limit"].as_u64().unwrap_or(20);
                self.http.expect_json(
                    "GET",
                    &format!("/api/search/messages?q={}&limit={limit}", urlencode(q)),
                    None,
                )
            }
            "group.info" => {
                let conv = str_param(params, "conv")?;
                self.http
                    .expect_json("GET", &format!("/api/groups/{conv}"), None)
            }
            "group.members" => {
                let conv = str_param(params, "conv")?;
                self.http
                    .expect_json("GET", &format!("/api/groups/{conv}/members"), None)
            }
            "conv.list" => self.conv_list(),
            // 写与破坏性命令：P3 才实现。这里明确报"未实现"而不是静默成功。
            // 调用方会补上 id 前缀，这里不要重复
            other => Err(format!(
                "尚未在 CLI 实现（P1 只做只读；写命令见 P3）: {other}"
            )),
        }
    }

    /// 会话清单：群 + 直聊（由好友列表推导），并附未读。
    fn conv_list(&self) -> Result<Value, String> {
        let me = self.http.expect_json("GET", "/api/me", None)?;
        let me_id = me["user_id"].as_str().unwrap_or_default().to_string();

        let mut out: Vec<Value> = Vec::new();
        if let Ok(groups) = self.http.expect_json("GET", "/api/groups/mine", None) {
            for g in groups.as_array().cloned().unwrap_or_default() {
                let id = g["id"].as_str().unwrap_or_default().to_string();
                out.push(json!({
                    "conv": id,
                    "kind": "group",
                    "title": g["name"],
                    "members": g["member_count"],
                }));
            }
        }
        if let Ok(friends) = self.http.expect_json("GET", "/api/friends", None) {
            for f in friends.as_array().cloned().unwrap_or_default() {
                let uid = f["friend_id"].as_str().unwrap_or_default().to_string();
                if uid.is_empty() {
                    continue;
                }
                out.push(json!({
                    "conv": wxlike_client::direct_conv_id(&me_id, &uid),
                    "kind": "direct",
                    "title": uid,
                }));
            }
        }

        // 未读：逐个问信箱；被白名单挡掉的会话直接不出现在结果里。
        for item in out.iter_mut() {
            let conv = item["conv"].as_str().unwrap_or_default().to_string();
            if !self.policy.conv_allowed(&conv) {
                item["unread"] = json!(null);
                item["note"] = json!("不在白名单，未读数不查");
                continue;
            }
            match self.http.conv_unread(&conv) {
                Ok(u) => item["unread"] = u["unread"].clone(),
                Err(e) => item["unread_error"] = json!(e),
            }
        }
        // 白名单外的会话不返回——列会话不该泄露名单外的东西
        out.retain(|v| v["unread"].is_number() || v["unread"].is_null() && v["note"].is_null());
        Ok(Value::Array(out))
    }
}

fn str_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("缺少参数 {key}"))
}

/// 最小 URL 编码：只要把空格、`&`、`#`、`+`、`?`、`%` 处理掉。
/// 不做完整 percent-encoding——搜索词来自命令行，够用即可。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            ' ' => out.push_str("%20"),
            '&' => out.push_str("%26"),
            '#' => out.push_str("%23"),
            '+' => out.push_str("%2B"),
            '?' => out.push_str("%3F"),
            '%' => out.push_str("%25"),
            c => out.push(c),
        }
    }
    out
}

/// 解析 `--cmd`：`id` 或 `id={json}`。
fn parse_cmd(raw: &str) -> Result<(String, Value), String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("空的 --cmd".into());
    }
    match raw.split_once('=') {
        None => Ok((raw.to_string(), json!({}))),
        Some((id, rest)) => {
            let v: Value =
                serde_json::from_str(rest).map_err(|e| format!("{id} 的参数不是合法 JSON: {e}"))?;
            if !v.is_object() {
                return Err(format!("{id} 的参数必须是对象"));
            }
            Ok((id.to_string(), v))
        }
    }
}

fn main() -> ExitCode {
    // 先剥掉带值的 flag，剩下的第一个是子命令。
    // 不能直接 `args.next()`：`--server URL inspect` 会把 `--server` 当成子命令。
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        if raw[i].starts_with("--") {
            i += 2; // flag + 它的值（所有 flag 都带值）
        } else {
            positional.push(raw[i].clone());
            i += 1;
        }
    }
    let sub = positional.first().cloned().unwrap_or_default();
    let rest: Vec<String> = raw; // flag 仍从完整参数里读

    let server = flag(&rest, "--server")
        .or_else(|| std::env::var("WXLIKE_SERVER").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8099".to_string());
    let token = flag(&rest, "--token").or_else(|| std::env::var("WXLIKE_TOKEN").ok());
    let policy = Policy::from_env();

    match sub.as_str() {
        "list" => {
            let q = positional.get(1).cloned().unwrap_or_default();
            for s in search(&q) {
                println!("{:<20} {:<12} {}", s.id, s.capability, s.summary);
            }
            ExitCode::SUCCESS
        }
        "policy" => {
            dump_policy(&policy);
            ExitCode::SUCCESS
        }
        "inspect" | "run" => {
            let mut http = wxlike_client::Http::new(server);
            http.token = token;
            if http.token.is_none() {
                eprintln!("缺少 token：--token 或 WXLIKE_TOKEN");
                return ExitCode::FAILURE;
            }
            let exec = Exec {
                http,
                policy: &policy,
            };

            if sub == "inspect" {
                let mut out = serde_json::Map::new();
                for id in ["me.info", "conv.list"] {
                    match exec.run(id, &json!({})) {
                        Ok(v) => {
                            out.insert(id.to_string(), v);
                        }
                        Err(e) => {
                            eprintln!("{id}: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
                println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
                return ExitCode::SUCCESS;
            }

            // run：按出现顺序执行所有 --cmd
            let cmds: Vec<String> = collect_flag_values(&rest, "--cmd");
            if cmds.is_empty() {
                eprintln!("run 需要至少一个 --cmd 'id[={{json}}]'");
                return ExitCode::FAILURE;
            }
            for raw in cmds {
                let (id, params) = match parse_cmd(&raw) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("{e}");
                        return ExitCode::FAILURE;
                    }
                };
                match exec.run(&id, &params) {
                    Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
                    Err(e) => {
                        // 与 WordCraft 一致：一条失败就停，且不假装成功
                        eprintln!("{id}: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("用法:");
            eprintln!("  wxlike-cli [--server URL] [--token T] list [关键词]");
            eprintln!("  wxlike-cli [--server URL] [--token T] inspect");
            eprintln!("  wxlike-cli [--server URL] [--token T] run --cmd 'conv.unread={{\"conv\":\"d-a~b\"}}'");
            eprintln!("  wxlike-cli policy");
            eprintln!();
            eprintln!(
                "权限来自环境变量：WXLIKE_AGENT_ALLOW_CONV / _ALLOW_WRITE / _ALLOW_DESTRUCTIVE"
            );
            ExitCode::FAILURE
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

fn collect_flag_values(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn dump_policy(p: &Policy) {
    println!("权限控制中心 · 当前生效策略");
    let convs = p.conv_patterns();
    println!(
        "  会话白名单   {}",
        if convs.is_empty() {
            "（空 = 任何会话都不允许）".to_string()
        } else {
            convs.join(", ")
        }
    );
    println!(
        "  写权限       {}",
        if p.allow_write() { "开" } else { "关" }
    );
    println!(
        "  破坏性权限   {}",
        if p.allow_destructive() { "开" } else { "关" }
    );
    println!();
    println!("{:<20} {:<12} 判定", "ID", "能力");
    for s in CATALOG {
        let probe = json!({
            "conv": p.conv_patterns().first().map(|c| c.replace('*', "probe")).unwrap_or_else(|| "probe".into()),
        });
        match p.authorize(s.id, &probe) {
            Ok(_) => println!("{:<20} {:<12} 允许", s.id, s.capability),
            Err(e) => println!("{:<20} {:<12} 拒绝 — {e}", s.id, s.capability),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cmd_accepts_bare_id_and_json() {
        assert_eq!(parse_cmd("me.info").unwrap(), ("me.info".into(), json!({})));
        let (id, p) = parse_cmd(r#"conv.unread={"conv":"d-a~b"}"#).unwrap();
        assert_eq!(id, "conv.unread");
        assert_eq!(p["conv"], "d-a~b");
    }

    #[test]
    fn parse_cmd_rejects_bad_json_and_non_object() {
        assert!(parse_cmd("conv.unread={oops}").is_err());
        assert!(parse_cmd("conv.unread=[1,2]").is_err());
        assert!(parse_cmd("  ").is_err());
    }

    #[test]
    fn urlencode_handles_query_metacharacters() {
        assert_eq!(urlencode("a b&c#d+e?f%g"), "a%20b%26c%23d%2Be%3Ff%25g");
    }

    /// 被权限拒绝时**不该发任何请求**：给一个必然连不上的地址，
    /// 如果错误是"连接失败"就说明权限没拦住。
    #[test]
    fn policy_denies_before_any_network_call() {
        let policy = Policy::none();
        let exec = Exec {
            http: wxlike_client::Http::new("http://127.0.0.1:1"),
            policy: &policy,
        };
        let err = exec
            .run("conv.unread", &json!({"conv": "d-a~b"}))
            .unwrap_err();
        assert!(err.contains("not allowed"), "{err}");
        assert!(!err.contains("connect"), "不该走到网络：{err}");
    }
}
