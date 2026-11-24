//! Minimal chat client demo:
//! `cargo run -p wxlike-net --example chat -- <hub-addr> <name>`
//!
//! Works with both `wxlike-server` (MQ-routed hub) and the legacy hub.

use std::io::BufRead;
use std::sync::{Arc, Mutex};

use wxlike_core::{Envelope, MsgId};
use wxlike_net::Client;

/// 后台收包：每条消息打印并把最后一条留给 `:read`。
fn spawn_reader(mut reader: Client, state: Arc<Mutex<Option<Envelope>>>) {
    tokio::spawn(async move {
        while let Some(env) = next_or_log(&mut reader).await {
            println!("[{}] {}: {}", env.conv_id, env.sender, text_of(&env));
            *state.lock().expect("demo lock") = Some(env);
        }
    });
}

/// 拉一条；断线就记一行并返回 `None`，让收包循环退出。
async fn next_or_log(reader: &mut Client) -> Option<Envelope> {
    match reader.next_envelope().await {
        Ok(env) => Some(env),
        Err(e) => {
            eprintln!("[recv] closed: {e}");
            None
        }
    }
}

/// 处理 `:recall` / `:read`；返回 true 表示这一行已被消费。
async fn handle_special(
    line: &str,
    client: &Client,
    who: &str,
    received: &Arc<Mutex<Option<Envelope>>>,
    last_sent: &mut Option<MsgId>,
) -> anyhow::Result<bool> {
    // ":recall" un-sends the last message this client wrote
    if line.starts_with(":recall") {
        let Some(target) = last_sent.clone() else {
            println!("(nothing to recall yet)");
            return Ok(true);
        };
        client
            .send(&Envelope::recall("conv1", who.to_string(), target))
            .await?;
        println!("(recall sent)");
        return Ok(true);
    }
    // ":read" acknowledges the last message received, back to its author
    if line.starts_with(":read") {
        let last_received = received.lock().expect("demo lock").clone();
        let Some(msg) = last_received else {
            println!("(nothing read yet)");
            return Ok(true);
        };
        let ack = msg.receipt_from(
            who.to_string(),
            wxlike_core::Receipt::Read {
                up_to_msg_id: msg.msg_id,
            },
        );
        client.send(&ack).await?;
        println!("(receipt sent)");
        return Ok(true);
    }
    Ok(false)
}

/// "name> text" 定向到一个人，纯文本广播。
fn outgoing(line: &str, who: &str, seq: u64) -> Envelope {
    match line.split_once("> ") {
        Some((to, text)) => {
            Envelope::direct("conv1", who.to_string(), vec![to.trim().into()], seq, text)
        }
        None => Envelope::text("conv1", who.to_string(), seq, line),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let addr: std::net::SocketAddr = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "127.0.0.1:9443".into())
        .parse()?;
    let who = args.get(2).cloned().unwrap_or_else(|| "anon".into());

    let client = Client::connect(addr, "localhost", who.clone()).await?;
    println!("[{who}] connected to {addr}");

    // last inbound message, shared with the input loop for ":read"
    let received: Arc<Mutex<Option<Envelope>>> = Arc::new(Mutex::new(None));
    spawn_reader(client.clone(), received.clone());

    let stdin = std::io::stdin();
    let mut seq = 0u64;
    let mut last_sent: Option<MsgId> = None;
    for line in stdin.lock().lines() {
        let line = line?;
        if line == ":quit" {
            break;
        }
        if handle_special(&line, &client, &who, &received, &mut last_sent).await? {
            continue;
        }
        seq += 1;
        let env = outgoing(&line, &who, seq);
        last_sent = Some(env.msg_id);
        client.send(&env).await?;
    }
    Ok(())
}

fn text_of(env: &Envelope) -> String {
    match &env.body {
        wxlike_core::MsgBody::Text(t) => t.clone(),
        wxlike_core::MsgBody::Notice(n) => format!("*{n}"),
        wxlike_core::MsgBody::Bytes(b) => format!("<{} bytes>", b.len()),
        wxlike_core::MsgBody::Recall { target } => {
            let prefix: String = target[..4].iter().map(|b| format!("{b:02x}")).collect();
            format!("(recall of {prefix}…)")
        }
        wxlike_core::MsgBody::Custom { kind, payload } => {
            format!("<{kind}> {}", payload)
        }
    }
}
