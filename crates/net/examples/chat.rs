//! Minimal chat client demo:
//! `cargo run -p wxlike-net --example chat -- <hub-addr> <name>`
//!
//! Works with both `wxlike-server` (MQ-routed hub) and the legacy hub.

use std::io::BufRead;
use std::sync::{Arc, Mutex};

use wxlike_core::{Envelope, MsgId};
use wxlike_net::Client;

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
    let reader = client.clone();
    let reader_state = received.clone();
    tokio::spawn(async move {
        loop {
            match reader.next_envelope().await {
                Ok(env) => {
                    println!("[{}] {}: {}", env.conv_id, env.sender, text_of(&env));
                    *reader_state.lock().expect("demo lock") = Some(env);
                }
                Err(e) => {
                    eprintln!("[recv] closed: {e}");
                    break;
                }
            }
        }
    });

    let stdin = std::io::stdin();
    let mut seq = 0u64;
    let mut last_sent: Option<MsgId> = None;
    for line in stdin.lock().lines() {
        let line = line?;
        if line == ":quit" {
            break;
        }
        // ":recall" un-sends the last message this client wrote
        if line.starts_with(":recall") {
            let Some(target) = last_sent else {
                println!("(nothing to recall yet)");
                continue;
            };
            client
                .send(&Envelope::recall("conv1", who.clone(), target))
                .await?;
            println!("(recall sent)");
            continue;
        }
        // ":read" acknowledges the last message received, back to its author
        if line.starts_with(":read") {
            let last_received = received.lock().expect("demo lock").clone();
            let Some(msg) = last_received else {
                println!("(nothing read yet)");
                continue;
            };
            let ack = msg.receipt_from(
                who.clone(),
                wxlike_core::Receipt::Read {
                    up_to_msg_id: msg.msg_id,
                },
            );
            client.send(&ack).await?;
            println!("(receipt sent)");
            continue;
        }
        seq += 1;
        // "name> text" directs to one user; plain text broadcasts
        let env = match line.split_once("> ") {
            Some((to, text)) => {
                Envelope::direct("conv1", who.clone(), vec![to.trim().into()], seq, text)
            }
            None => Envelope::text("conv1", who.clone(), seq, line),
        };
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
