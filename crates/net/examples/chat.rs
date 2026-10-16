//! Minimal chat client demo:
//! `cargo run -p wxlike-net --example chat -- <hub-addr> <name>`
//!
//! Works with both `wxlike-server` (MQ-routed hub) and the legacy hub.

use std::io::BufRead;

use wxlike_core::Envelope;
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

    let reader = client.clone();
    tokio::spawn(async move {
        loop {
            match reader.next_envelope().await {
                Ok(env) => println!("[{}] {}: {}", env.conv_id, env.sender, text_of(&env)),
                Err(e) => {
                    eprintln!("[recv] closed: {e}");
                    break;
                }
            }
        }
    });

    let stdin = std::io::stdin();
    let mut seq = 0u64;
    for line in stdin.lock().lines() {
        let line = line?;
        if line == ":quit" {
            break;
        }
        seq += 1;
        // "name> text" directs to one user; plain text broadcasts
        let env = match line.split_once("> ") {
            Some((to, text)) => Envelope::direct("conv1", who.clone(), vec![to.trim().into()], seq, text),
            None => Envelope::text("conv1", who.clone(), seq, line),
        };
        client.send(&env).await?;
    }
    Ok(())
}

fn text_of(env: &Envelope) -> String {
    match &env.body {
        wxlike_core::MsgBody::Text(t) => t.clone(),
        wxlike_core::MsgBody::Notice(n) => format!("*{n}"),
        wxlike_core::MsgBody::Bytes(b) => format!("<{} bytes>", b.len()),
    }
}
