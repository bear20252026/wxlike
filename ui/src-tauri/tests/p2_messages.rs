//! P2 integration tests: rich messages, avatar→media, the local message
//! library and (optionally) the video pipeline — all against the real server
//! stack (platform HTTP + QUIC hub + video-service), per the task book's
//! no-mocks rule at this boundary.

use std::time::Duration;

use wxlike_ui_lib::AppState;

mod support;

use crate::support::{hex_id, pins, stack};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p2_voice_file_sticker_roundtrip() {
    let (http, quic, platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register("bob", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice_id = alice.register("alice", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice
        .connect_hub(&quic.to_string(), &alice_id, &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob_id, &pins())
        .unwrap();
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);

    // voice: upload the "audio" then send the payload with a 40-bar waveform
    let audio: Vec<u8> = (0..512u32).map(|i| (i * 7 % 251) as u8).collect();
    let up = alice.upload_media(audio, "audio/webm", None).unwrap();
    let audio_id = up["media_id"].as_str().unwrap().to_string();
    let waveform: Vec<u8> = (0..40u32).map(|i| (i * 5 % 100) as u8 + 1).collect();
    alice
        .send_voice(
            &conv,
            vec![bob_id.clone()],
            &audio_id,
            3200,
            waveform.clone(),
        )
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(15), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("voice arrives");
    assert_eq!(ev["kind"], "msg.voice");
    assert_eq!(ev["payload"]["media_id"], audio_id);
    assert_eq!(ev["payload"]["duration_ms"], 3200);
    assert_eq!(
        ev["payload"]["waveform"].as_array().unwrap().len(),
        40,
        "waveform bars travel intact"
    );

    // client-side validation: a malformed payload is refused before send
    assert!(alice
        .send_voice(&conv, vec![bob_id.clone()], "short", 100, waveform.clone())
        .is_err());
    assert!(
        alice
            .send_voice(&conv, vec![bob_id.clone()], &audio_id, 0, waveform)
            .is_err(),
        "zero duration is refused"
    );

    // file
    let file_bytes: Vec<u8> = (0..1024u32).map(|i| (i % 255) as u8).collect();
    let fup = alice
        .upload_media(file_bytes, "application/pdf", None)
        .unwrap();
    let file_id = fup["media_id"].as_str().unwrap().to_string();
    alice
        .send_file(
            &conv,
            vec![bob_id.clone()],
            &file_id,
            "季度报表.pdf",
            1024,
            "",
        )
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(15), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("file arrives");
    assert_eq!(ev["kind"], "msg.file");
    assert_eq!(ev["payload"]["name"], "季度报表.pdf");
    assert_eq!(ev["payload"]["size"], 1024);
    // path-shaped names are refused client-side
    assert!(alice
        .send_file(&conv, vec![bob_id.clone()], &file_id, "../evil", 1, "")
        .is_err());

    // sticker
    alice
        .send_sticker(&conv, vec![bob_id.clone()], &file_id, "my", "s-1")
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(15), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("sticker arrives");
    assert_eq!(ev["kind"], "msg.sticker");
    assert_eq!(ev["payload"]["pack"], "my");

    // all three live in server history as content frames
    tokio::time::sleep(Duration::from_millis(300)).await;
    let token = platform.create_session(&bob_id, 60_000).unwrap();
    let rows: serde_json::Value = ureq::get(&format!("{http}/api/history/{conv}"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    let kinds: Vec<&str> = rows["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"msg.voice")
            && kinds.contains(&"msg.file")
            && kinds.contains(&"msg.sticker"),
        "all rich kinds persisted: {kinds:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p2_avatar_wired_to_media_store() {
    let (http, _quic, platform) = stack().await;
    let alice = AppState::new(&http);
    let alice_id = alice.register("alice", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();

    // a tiny valid PNG (1x1) so the media magic-sniffer agrees
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let media_id = alice.upload_avatar(png.clone(), "image/png").unwrap();
    assert_eq!(media_id.len(), 64);

    // the profile now points at the media object
    let me = alice.me().unwrap();
    assert_eq!(me["avatar"], media_id);

    // a peer can fetch the profile AND the bytes through the media path
    let bob = AppState::new(&http);
    bob.register("bob", "pw").unwrap();
    let token = platform
        .create_session(&bob.user_id().unwrap(), 60_000)
        .unwrap();
    let profile: serde_json::Value = ureq::get(&format!("{http}/api/profile/{alice_id}"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(
        profile["avatar"], media_id,
        "public profile carries the avatar id"
    );
    let blob = bob.download_media(&media_id, false).unwrap();
    let decoded =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &blob.base64).unwrap();
    assert_eq!(blob.content_type, "image/png");
    assert_eq!(decoded, png);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p2_local_message_library_survives_restart() {
    let (http, quic, _platform) = stack().await;
    let db = std::env::temp_dir().join(format!(
        "wxlike-local-{}-{}.db",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));

    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register("bob", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice_id = alice.register("alice", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice
        .connect_hub(&quic.to_string(), &alice_id, &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob_id, &pins())
        .unwrap();
    bob.local_open(&db).unwrap();
    assert!(bob.local_ready());
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);

    for i in 0..3 {
        alice
            .send_text(&conv, vec![bob_id.clone()], &format!("local {i}"))
            .unwrap();
    }
    for _ in 0..3 {
        let _ = tokio::time::timeout(Duration::from_secs(15), bob.next_event())
            .await
            .unwrap()
            .unwrap()
            .expect("event");
    }

    // a "restarted" client over the same db sees the rows with no hub at all
    let bob2 = AppState::new(&http);
    bob2.local_open(&db).unwrap();
    let page = bob2.local_history(&conv, 0, 50).unwrap();
    let msgs = page["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3, "persisted from the pump: {page}");
    assert_eq!(msgs[0]["text"], "local 0");
    assert_eq!(msgs[2]["text"], "local 2");

    // transient signals never enter the library
    alice.send_typing(&conv, vec![bob_id.clone()]).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(15), bob.next_event()).await;
    let page = bob2.local_history(&conv, 0, 50).unwrap();
    assert_eq!(
        page["messages"].as_array().unwrap().len(),
        3,
        "typing is not persisted"
    );

    std::fs::remove_file(&db).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs ffmpeg on PATH for the sample clip: cargo test --test ui_commands p2_video -- --ignored"]
async fn p2_video_upload_transcode_and_message() {
    let (http, quic, _platform) = stack().await;
    let alice = AppState::new(&http);
    let bob = AppState::new(&http);
    let bob_id = bob.register("bob", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alice_id = alice.register("alice", "pw").unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_string();
    alice
        .connect_hub(&quic.to_string(), &alice_id, &pins())
        .unwrap();
    bob.connect_hub(&quic.to_string(), &bob_id, &pins())
        .unwrap();
    let conv = wxlike_ui_lib::direct_conv_id(&alice_id, &bob_id);

    // synthesize a 1-second clip (ffmpeg is on PATH per the ignore note)
    let dir = std::env::temp_dir().join(format!("wxlike-ui-video-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sample_path = dir.join("sample.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=320x240:rate=15",
            sample_path.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "sample synthesis failed");
    let bytes = std::fs::read(&sample_path).unwrap();

    // upload through the command layer → video-service on the same server
    let ingest = alice.upload_video(bytes).unwrap();
    let video_id = ingest["video_id"].as_str().unwrap().to_string();
    assert_eq!(video_id.len(), 64);
    assert_eq!(
        ingest["duration_ms"].as_u64().unwrap(),
        1000,
        "ffprobe duration"
    );
    assert!(ingest["playlist_url"]
        .as_str()
        .unwrap()
        .ends_with("playlist.m3u8"));
    assert!(ingest["play_url"].as_str().unwrap().ends_with("play.mp4"));

    // send the message reference (the same shape api.ts's sendVideo builds)
    let payload = serde_json::json!({
        "video_id": ingest["video_id"],
        "playlist_url": ingest["playlist_url"],
        "play_url": ingest["play_url"],
        "thumb_media_id": "",
        "duration_ms": ingest["duration_ms"],
        "speeds": [0.5, 1.0, 1.5, 2.0],
    });
    alice
        .send_video(&conv, vec![bob_id.clone()], &payload)
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(15), bob.next_event())
        .await
        .unwrap()
        .unwrap()
        .expect("video message arrives");
    assert_eq!(ev["kind"], "msg.video");
    assert_eq!(ev["payload"]["video_id"], video_id);

    // the progressive MP4 plays back through the allowlisted downloader
    let blob = bob
        .download_video(ingest["play_url"].as_str().unwrap())
        .unwrap();
    assert_eq!(blob.content_type, "video/mp4");
    assert!(!blob.base64.is_empty());

    // the allowlist refuses anything outside /api/video
    assert!(bob.download_video("/api/favorites").is_err());
    assert!(bob.download_video("/api/video/../secret/play.mp4").is_err());

    std::fs::remove_dir_all(&dir).ok();
}

// keep hex_id referenced (shared helper lives in support.rs)
#[allow(dead_code)]
fn _unused() {
    let _ = hex_id(&serde_json::json!("x"));
}
