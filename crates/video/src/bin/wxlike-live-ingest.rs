//! wxlike-live-ingest — 直播**接入节点**（E2）。
//!
//! # 它做什么
//!
//! 把一路实时源（RTMP 拉流、RTSP、或任何 ffmpeg 读得动的输入）切成 MPEG-TS
//! 分片，逐片推给 video-service 的直播打包面：
//!
//! ```text
//! OBS ──RTMP──▶ 终止节点(Xiu/ffmpeg) ──rtmp://…──▶ 本节点 ──HTTP PUT 分片──▶ wxlike-server
//!                                                                     └──▶ /live/{key}/index.m3u8
//! ```
//!
//! # 为什么协议不在本仓库里
//!
//! ADR-21 与 `docs/p5-report.md`：不在进程内跑媒体服务器。本节点只做"终止之后"
//! 的搬运与实测时长，HLS 清单与窗口由服务端的 `crates/video/src/live.rs` 生成
//! ——这样两个进程只有一份窗口真相，改窗口策略不用同时改两处。
//!
//! # 用法
//!
//! ```bash
//! # 开播（拿到 ingest_token 与 stream_key）
//! TOK=$(curl -s -X POST http://127.0.0.1:8099/api/live/{channel}/start \
//!        -H "authorization: Bearer $TOKEN" -d '{"recording":false}' | jq -r .ingest_token)
//! KEY=$(… | jq -r .session.stream_key)
//!
//! cargo run -p wxlike-video --bin wxlike-live-ingest -- \
//!     --source rtmp://127.0.0.1:1935/live/$KEY \
//!     --stream-key $KEY --token $TOK \
//!     --server http://127.0.0.1:8099 --segment-seconds 4
//! ```
//!
//! 退出码即结论：0 = 正常收尾（已写 `#EXT-X-ENDLIST`）；非 0 = 有分片没推上去或
//! ffmpeg 异常退出，stderr 说明原因。**不静默丢片**。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use wxlike_video::{LIVE_SEGMENT_MS_HEADER, LIVE_TOKEN_HEADER};

/// 默认切片时长（秒）。与服务端 `LIVE_TARGET_MS` 一致。
const DEFAULT_SEGMENT_SECONDS: u32 = 4;
/// 单片推送重试次数（不含首次）。直播对延迟敏感，重试太多不如丢一片继续跑。
const PUSH_ATTEMPTS: u32 = 3;
/// 重试间隔基数（毫秒），线性递增。
const PUSH_RETRY_MS: u64 = 250;
/// 轮询分片文件的间隔。
const POLL_MS: u64 = 120;
/// ffmpeg 无输出多久后认定它卡死（毫秒）。0 = 不检测。
const STALL_EXIT_MS: u64 = 60_000;

/// 运行参数。
struct Config {
    /// ffmpeg 读得动的输入：`rtmp://…`、`rtsp://…`、本地文件（自测用）
    source: String,
    /// 平台开播时下发的流密钥
    stream_key: String,
    /// 平台开播时下发的自包含推流凭据 `<exp_ms>.<hmac>`
    token: String,
    /// video-service 地址
    server: String,
    segment_seconds: u32,
    /// 分片暂存目录（默认临时目录下的 `wxlike-ingest-{key}`）
    work_dir: PathBuf,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("wxlike-live-ingest 退出: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let cfg = Config::from_args()?;
    std::fs::create_dir_all(&cfg.work_dir)
        .with_context(|| format!("create work dir {}", cfg.work_dir.display()))?;
    println!(
        "接入节点启动: source={} key={} server={} 切片={}s 暂存={}",
        cfg.source,
        cfg.stream_key,
        cfg.server,
        cfg.segment_seconds,
        cfg.work_dir.display()
    );

    let pattern = cfg.work_dir.join("seg_%05d.ts");
    let mut child = spawn_ffmpeg(&cfg, &pattern)?;
    let packager = Packager::new(&cfg);
    let mut next_index: usize = 0;
    let mut lost: Vec<String> = Vec::new();
    let mut last_change = Instant::now();

    loop {
        let running = child.try_wait().with_context(|| "poll ffmpeg")?.is_none();
        if path_at(&cfg.work_dir, next_index).is_file()
            && (!running || path_at(&cfg.work_dir, next_index + 1).is_file())
        {
            let seg = path_at(&cfg.work_dir, next_index);
            // 只推已经写完的片：下一片出现（或 ffmpeg 已退出）才算写完
            match packager.push(&seg) {
                Ok(()) => {
                    last_change = Instant::now();
                    next_index += 1;
                    std::fs::remove_file(&seg).ok();
                }
                Err(err) => {
                    eprintln!("分片 {} 推送失败: {err:#}", seg.display());
                    lost.push(
                        seg.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    );
                    next_index += 1;
                    std::fs::remove_file(&seg).ok();
                }
            }
        } else if !running {
            break;
        } else if STALL_EXIT_MS > 0 && last_change.elapsed() > Duration::from_millis(STALL_EXIT_MS)
        {
            anyhow::bail!(
                "ffmpeg {STALL_EXIT_MS}ms 没有产出新分片，判定卡死（源断了？），退出以免一直占着这路流"
            );
        } else {
            std::thread::sleep(Duration::from_millis(POLL_MS));
        }
    }

    // 收尾：把还没推的碎片推完，再告诉服务端"停播"
    while path_at(&cfg.work_dir, next_index).is_file() {
        let seg = path_at(&cfg.work_dir, next_index);
        if let Err(err) = packager.push(&seg) {
            eprintln!("收尾分片 {} 失败: {err:#}", seg.display());
            lost.push(seg.file_name().unwrap().to_string_lossy().into_owned());
        }
        std::fs::remove_file(&seg).ok();
        next_index += 1;
    }
    let status = child.wait().with_context(|| "wait ffmpeg")?;
    match packager.stop() {
        Ok(()) => println!("已通知服务端停播（清单已写 #EXT-X-ENDLIST）"),
        Err(err) => eprintln!("通知停播失败: {err:#}"),
    }
    if next_index == 0 {
        anyhow::bail!("ffmpeg 一个分片都没产出（source 读不到？），不留下半成品");
    }
    if !lost.is_empty() {
        anyhow::bail!(
            "ffmpeg 退出码 {status:?}，但有 {} 个分片没推上去: {:?}",
            lost.len(),
            lost
        );
    }
    println!("正常收尾，共推送 {next_index} 片");
    Ok(())
}

impl Config {
    fn from_args() -> anyhow::Result<Self> {
        let mut source: Option<String> = None;
        let mut stream_key: Option<String> = None;
        let mut token: Option<String> = None;
        let mut server = Some("http://127.0.0.1:8099".to_string());
        let mut segment_seconds = DEFAULT_SEGMENT_SECONDS;
        let mut work_dir: Option<PathBuf> = None;
        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| anyhow::anyhow!("{flag} 需要一个值"))
            };
            match flag.as_str() {
                "--source" => source = Some(value()?),
                "--stream-key" => stream_key = Some(value()?),
                "--token" => token = Some(value()?),
                "--server" => server = Some(value()?),
                "--segment-seconds" => {
                    segment_seconds = value()?
                        .parse()
                        .with_context(|| "--segment-seconds 必须是整数")?
                }
                "--work-dir" => work_dir = Some(PathBuf::from(value()?)),
                other => anyhow::bail!("未知参数 {other}"),
            }
        }
        let stream_key = stream_key.context("缺 --stream-key（平台开播时下发）")?;
        let source = source.context("缺 --source（RTMP 拉流地址或任何 ffmpeg 读得动的输入）")?;
        let token = token.context("缺 --token（平台开播时下发的自包含推流凭据）")?;
        anyhow::ensure!(segment_seconds >= 1, "--segment-seconds 至少 1 秒");
        let base = server.unwrap_or_default().trim_end_matches('/').to_string();
        let work_dir = work_dir
            .unwrap_or_else(|| std::env::temp_dir().join(format!("wxlike-ingest-{stream_key}")));
        Ok(Self {
            source,
            stream_key,
            token,
            server: base,
            segment_seconds,
            work_dir,
        })
    }
}

/// 第 `index` 个分片的路径（与 ffmpeg 的 `seg_%05d.ts` 命名一致）。
fn path_at(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("seg_{index:05}.ts"))
}

fn spawn_ffmpeg(cfg: &Config, pattern: &Path) -> anyhow::Result<std::process::Child> {
    let status = Command::new(wxlike_video::ffmpeg_bin())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &cfg.source,
            // 不重编码：接入节点不该是算力瓶颈，转码是 VOD 流水线的事
            "-c",
            "copy",
            "-f",
            "segment",
            "-segment_format",
            "mpegts",
            "-segment_time",
            &cfg.segment_seconds.to_string(),
            "-reset_timestamps",
            "1",
            pattern.to_str().context("ffmpeg pattern path")?,
        ])
        .stdin(Stdio::null())
        // stderr 继承：ffmpeg 的错误要让人在终端看见
        .stdout(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn ffmpeg ({} 在 PATH 吗？)", wxlike_video::ffmpeg_bin()))?;
    Ok(status)
}

/// 服务端直播打包面的客户端。
struct Packager {
    base: String,
    stream_key: String,
    token: String,
}

impl Packager {
    fn new(cfg: &Config) -> Self {
        Self {
            base: cfg.server.clone(),
            stream_key: cfg.stream_key.clone(),
            token: cfg.token.clone(),
        }
    }

    /// 推一片：时长由 ffprobe 实测后随头带过去（服务端不猜时长）。
    fn push(&self, segment: &Path) -> anyhow::Result<()> {
        let name = segment
            .file_name()
            .context("segment has no file name")?
            .to_string_lossy()
            .to_string();
        let duration_ms = measured_duration_ms(segment)?;
        let bytes =
            std::fs::read(segment).with_context(|| format!("read {}", segment.display()))?;
        let url = format!("/live/{}/segment/{name}", self.stream_key);
        self.send("PUT", &url, &bytes, Some(duration_ms))
    }

    /// 停播。
    fn stop(&self) -> anyhow::Result<()> {
        self.send(
            "POST",
            &format!("/live/{}/stop", self.stream_key),
            b"",
            None,
        )
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        duration_ms: Option<u64>,
    ) -> anyhow::Result<()> {
        let url = format!("{}{path}", self.base);
        let mut last_err = None;
        for attempt in 0..=PUSH_ATTEMPTS {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(PUSH_RETRY_MS * u64::from(attempt)));
            }
            let mut request = match method {
                "PUT" => ureq::put(&url),
                "POST" => ureq::post(&url),
                other => anyhow::bail!("internal: {other}"),
            };
            request = request
                .timeout(Duration::from_secs(15))
                .set(LIVE_TOKEN_HEADER, &self.token)
                .set("content-type", "video/mp2t");
            if let Some(ms) = duration_ms {
                request = request.set(LIVE_SEGMENT_MS_HEADER, &ms.to_string());
            }
            // send(&[u8])：TS 是二进制，走字符串会被按 UTF-8 改写而损坏
            match request.send(body) {
                Ok(response) if (200..300).contains(&response.status()) => return Ok(()),
                Ok(response) => {
                    let code = response.status();
                    last_err = Some(anyhow::anyhow!("服务端回了 {code}"));
                    // 4xx 是配置/凭据问题，重试不会变好——一次就退
                    if (400..500).contains(&code) {
                        break;
                    }
                }
                Err(err) => last_err = Some(anyhow::anyhow!("{err}")),
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("推送失败")))
    }
}

/// ffprobe 实测分片时长（毫秒）。
fn measured_duration_ms(segment: &Path) -> anyhow::Result<u64> {
    let out = Command::new(wxlike_video::ffprobe_bin())
        .args(["-v", "quiet", "-print_format", "json", "-show_format"])
        .arg(segment)
        .stderr(Stdio::piped())
        .output()
        .with_context(|| {
            format!(
                "spawn ffprobe ({} 在 PATH 吗？)",
                wxlike_video::ffprobe_bin()
            )
        })?;
    if !out.status.success() {
        let mut pipe = std::io::stderr();
        let _ = pipe.write_all(&out.stderr);
        anyhow::bail!("ffprobe 失败于 {}", segment.display());
    }
    let probe: wxlike_video::Probe =
        serde_json::from_slice(&out.stdout).context("parse ffprobe json")?;
    let ms = probe.duration_ms()?;
    anyhow::ensure!(ms > 0, "ffprobe 报出 0 时长：{segment:?}");
    Ok(u64::from(ms))
}
