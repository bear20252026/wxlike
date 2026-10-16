//! wxlike UI backend: Tauri commands bridging the webview to wxlike-net.
//!
//! Scaffold: one hub connection per window, text messages only. The recv
//! pump forwards envelopes to the frontend as `im-in` events.

use std::sync::Mutex as StdMutex;

use tauri::{Emitter, State};
use wxlike_core::Envelope;
use wxlike_net::Client;

struct AppState {
    client: StdMutex<Option<Client>>,
    name: StdMutex<String>,
    seq: StdMutex<u64>,
}

#[tauri::command]
fn health_cmd() -> String {
    format!("wxlike ui {} ok", env!("CARGO_PKG_VERSION"))
}

#[tauri::command]
async fn connect_hub(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    addr: String,
    name: String,
) -> Result<String, String> {
    let sock: std::net::SocketAddr = addr.parse().map_err(|e| format!("bad addr: {e}"))?;
    let client = Client::connect(sock, "localhost")
        .await
        .map_err(|e| format!("connect: {e:#}"))?;
    *state.client.lock().unwrap() = Some(client);
    *state.name.lock().unwrap() = name.clone();
    let _ = app; // receiver pump keeps running from start_recv_pump
    Ok(format!("quic {sock}"))
}

#[tauri::command]
async fn send_text(state: State<'_, AppState>, text: String) -> Result<(), String> {
    let client = state.client.lock().unwrap().clone();
    let Some(client) = client else {
        return Err("not connected".into());
    };
    let sender = state.name.lock().unwrap().clone();
    let mut seq = state.seq.lock().unwrap();
    *seq += 1;
    client
        .send(&Envelope::text("conv1", sender, *seq, text))
        .await
        .map_err(|e| format!("send: {e:#}"))
}

#[tauri::command]
async fn start_recv_pump(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let Some(client) = state.client.lock().unwrap().clone() else {
        return Err("not connected".into());
    };
    tauri::async_runtime::spawn(async move {
        loop {
            match client.next_envelope().await {
                Ok(env) => {
                    let text = match &env.body {
                        wxlike_core::MsgBody::Text(t) => t.clone(),
                        wxlike_core::MsgBody::Notice(n) => format!("*{n}"),
                        wxlike_core::MsgBody::Bytes(b) => format!("<{} bytes>", b.len()),
                    };
                    let _ = app.emit("im-in", serde_json::json!({ "sender": env.sender, "text": text }));
                }
                Err(_) => break,
            }
        }
    });
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            client: StdMutex::new(None),
            name: StdMutex::new("ui".into()),
            seq: StdMutex::new(0),
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
