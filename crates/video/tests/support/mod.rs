//! 集成测试共用的小工具（每个 `tests/*.rs` 都是一个独立二进制，所以 helper 放这里）。
//!
//! 约定与本 crate 既有测试一致：需要 ffmpeg 的用例一律 `#[ignore]`，
//! 用 `cargo test -p wxlike-video -- --ignored` 跑。

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 每次调用一个独立目录：独立 `video.db`、独立对象集，用例之间不共享状态。
pub fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wxlike-video-{tag}-{}-{}",
        std::process::id(),
        wxlike_core::types::now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 一个只回固定状态码的 TCP 接收器。返回 (URL, 收到的请求列表)。
///
/// 用裸 TCP 而不是起一个 HTTP 服务：这里要验的是**客户端**发出去的东西
/// （方法、路径、头、JSON 形状）和它对各种状态码的分类，不需要一个真服务器。
pub fn spawn_sink(status: u16, reason: &str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let status_line =
        format!("HTTP/1.1 {status} {reason}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut lines = Vec::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim_end().to_string();
                if trimmed.is_empty() {
                    break;
                }
                let lower = trimmed.to_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
                lines.push(trimmed);
            }
            let mut body = vec![0u8; content_length];
            if content_length > 0 {
                reader.read_exact(&mut body).ok();
            }
            let mut text = lines.join("\n");
            text.push('\n');
            text.push_str(&String::from_utf8_lossy(&body));
            sink.lock().unwrap().push(text);
            stream.write_all(status_line.as_bytes()).ok();
            stream.flush().ok();
        }
    });
    (format!("http://{addr}/cb"), received)
}

/// ffmpeg/ffprobe 不在就明确报错，别让用例报一个看不懂的 spawn 失败。
pub fn require_tools() {
    assert!(
        wxlike_video::tools_available(),
        "ffmpeg/ffprobe not found — install ffmpeg or set WXLIKE_FFMPEG/WXLIKE_FFPROBE"
    );
}

/// 用 ffmpeg 自己合成样片（仓库不放二进制 fixture）。
pub fn make_sample_video(dir: &Path, width: u32, height: u32, seconds: u32, name: &str) -> PathBuf {
    let out = dir.join(name);
    let status = std::process::Command::new(wxlike_video::ffmpeg_bin())
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc=duration={seconds}:size={width}x{height}:rate=24"),
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:duration={seconds}"),
            // 每 2 秒一个关键帧（24fps × 2 = 48）：TS 切片只能在关键帧处断开，
            // 否则 `-c copy` 切出来永远只有一片，测试就验不到"多片窗口"。
            "-g",
            "48",
            "-keyint_min",
            "48",
            "-shortest",
            out.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("spawn ffmpeg to synthesize the sample");
    assert!(status.success(), "sample synthesis failed");
    out
}

/// 把一个文件切成 `segment_seconds` 长的 MPEG-TS 分片（**不产清单**）：
/// 这就是外部接入节点（Xiu / ffmpeg）终止 RTMP 之后交给我们的东西。
pub fn cut_ts_segments(source: &Path, out_dir: &Path, segment_seconds: u32) -> Vec<PathBuf> {
    std::fs::create_dir_all(out_dir).unwrap();
    let pattern = out_dir.join("seg_%03d.ts");
    let status = std::process::Command::new(wxlike_video::ffmpeg_bin())
        .args([
            "-y",
            "-i",
            source.to_str().unwrap(),
            "-c",
            "copy",
            "-f",
            "segment",
            // 只用 -segment_format：ffmpeg 9 的 segment 复用器没有 -segment_type
            // （那是 fMP4 分片的字段），写上去整条命令会被判"未知选项"
            "-segment_format",
            "mpegts",
            "-segment_time",
            &segment_seconds.to_string(),
            "-reset_timestamps",
            "1",
            pattern.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("spawn ffmpeg to cut segments");
    assert!(status.success(), "segment cut failed");
    let mut files: Vec<PathBuf> = std::fs::read_dir(out_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("ts"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "ffmpeg produced no segments");
    files
}

/// ffprobe 实测时长（秒）。用来证明"这份清单+这些分片是真能播的"。
pub fn probe_seconds(target: &Path) -> f64 {
    let out = std::process::Command::new(wxlike_video::ffprobe_bin())
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(target)
        .stderr(std::process::Stdio::null())
        .output()
        .expect("spawn ffprobe");
    assert!(out.status.success(), "ffprobe failed on {:?}", target);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let text = value["format"]["duration"].as_str().unwrap_or("0");
    text.parse().unwrap_or(0.0)
}
