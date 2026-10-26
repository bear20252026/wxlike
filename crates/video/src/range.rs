//! E1 之一：`Range` 请求支持。
//!
//! `play.mp4` 之前整文件返回：`<video>` 一拖进度条就得重拉整个文件，或者干脆
//! 不让 seek（媒体元素对没有 `Accept-Ranges` 的响应普遍不做拖动）。这一层把
//! `bytes=a-b` / `bytes=a-` / `bytes=-N` 翻译成闭区间，只读请求那一段。
//!
//! **语义与 `crates/platform/src/media.rs` 的 `parse_range` 对齐**（同仓库参照
//! 重写，不是照搬代码：Zone E 的所有权只到 `crates/video/**`，platform 是热点
//! 文件）。两份应当合并进 `crates/core`，这条已写进 PR 交集成方。

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::Context as _;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use wxlike_api::ApiError;

/// 闭区间字节范围（保证 `start <= end`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    /// 闭区间末尾，含。
    pub end: u64,
}

impl ByteRange {
    /// 区间字节数（合法区间恒 ≥ 1）。
    pub fn byte_count(&self) -> u64 {
        self.end - self.start + 1
    }
}

/// 一个 `Range` 头的处理结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    /// 整体返回（没有 Range 头，或请求恰好覆盖全文件）→ 200。
    Whole { total: u64 },
    /// 部分返回 → 206 + `Content-Range`。
    Partial { range: ByteRange, total: u64 },
    /// 带了 Range 头但不可满足/无法解析 → 416 + `Content-Range: bytes */total`。
    Unsatisfiable { total: u64 },
}

/// 解析 `Range: bytes=a-b` / `bytes=a-` / `bytes=-suffix`，夹到 `[0, total)`。
///
/// `None` = 没有可用单区间（语法错误、不可满足、多区间）。返回的整体区间由
/// [`serve_range`] 折算成 200，这一层只负责"哪些字节"。
pub fn parse_range(value: &str, total: u64) -> Option<ByteRange> {
    let spec = value.strip_prefix("bytes=")?;
    if total == 0 {
        return None;
    }
    // 多区间不实现（与 platform 的媒体下载同一取舍：解析不了就是不可满足）
    if spec.contains(',') {
        return None;
    }
    let last = total - 1;
    let (first_s, last_s) = spec.split_once('-')?;
    let first_s = first_s.trim();
    let last_s = last_s.trim();
    if first_s.is_empty() {
        // 后缀形式：最后 N 个字节
        let n: u64 = last_s.parse().ok()?;
        if n == 0 {
            return None;
        }
        return Some(ByteRange {
            start: last.saturating_sub(n - 1),
            end: last,
        });
    }
    let start: u64 = first_s.parse().ok()?;
    if start > last {
        return None;
    }
    let end = if last_s.is_empty() {
        last
    } else {
        last_s.parse::<u64>().ok()?.min(last)
    };
    if end < start {
        return None;
    }
    Some(ByteRange { start, end })
}

/// 决定该回 200、206 还是 416。
///
/// 空文件永远整体返回：给 0 字节 body 发 206 会让播放器以为拿到了某段，
/// 结果什么都解不出来——那是比 416 更难查的坏。
pub fn serve_range(header_value: Option<&str>, total: u64) -> Served {
    let Some(value) = header_value else {
        return Served::Whole { total };
    };
    if total == 0 {
        return Served::Whole { total };
    }
    match parse_range(value, total) {
        None => Served::Unsatisfiable { total },
        Some(range) if range.start == 0 && range.end + 1 == total => Served::Whole { total },
        Some(range) => Served::Partial { range, total },
    }
}

/// 只读 `[start..=end]`，不把整个文件吃进内存。
pub fn read_range(path: &Path, range: ByteRange) -> anyhow::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let want = usize::try_from(range.byte_count())
        .map_err(|_| anyhow::anyhow!("range larger than addressable memory"))?;
    let mut buf = Vec::with_capacity(want.min(16 * 1024 * 1024));
    file.seek(SeekFrom::Start(range.start))?;
    let got = file.take(range.byte_count()).read_to_end(&mut buf)?;
    anyhow::ensure!(
        got == want,
        "short read on {}: asked {want} at {}, got {got}",
        path.display(),
        range.start
    );
    Ok(buf)
}

/// 带 `Range` 的文件响应：200/206/416/404 都在这一处决定。
///
/// 同步阻塞读——handler 用 `spawn_blocking` 包一层；seek 一次只读一段，
/// 比原先"整文件进内存"轻得多。
///
/// # 已知欠账：无 Range 的整文件响应仍要一次性进内存
///
/// `Served::Whole`（浏览器 `<video>` 首次加载常发 `Range: bytes=0-`，它也会
/// 折算成 Whole）目前把整个文件读进 `Vec`。真正的分块流式响应需要一个
/// Stream→Body 适配器（`tokio_util::io::ReaderStream` 或 `futures` channel），
/// 而 `wxlike-video` 的依赖里两者都没有，axum 0.8 也没有把
/// `futures_core::Stream` / `http_body::Frame` re-export 出来，自己实现
/// `HttpBody` 都点不出 `Frame` 这个类型。加依赖是被本轮明确禁止的
/// （Cargo.toml 冻结），所以先做两件不越界的事：超阈值告警（内存压力可见）
/// 与单次精确分配（不反复扩容）。依赖解冻后把 Whole 分支换成 ReaderStream。
pub fn respond_with_range(
    path: &Path,
    content_type: &'static str,
    headers: &HeaderMap,
) -> Response {
    let total = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(_) => return missing(),
    };
    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    match serve_range(range_header.as_deref(), total) {
        Served::Unsatisfiable { total } => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{total}"))],
        )
            .into_response(),
        Served::Whole { .. } => match read_whole_exact(path, total) {
            Ok(body) => {
                if total > WHOLE_SERVE_WARN_BYTES {
                    tracing::warn!(
                        path = %path.display(),
                        bytes = total,
                        "serving a whole video file from memory (no Range); \
                         see the known-debt note on respond_with_range"
                    );
                }
                (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, content_type.to_string()),
                        (header::ACCEPT_RANGES, "bytes".to_string()),
                    ],
                    body,
                )
                    .into_response()
            }
            Err(_) => missing(),
        },
        Served::Partial { range, total } => match read_range(path, range) {
            Ok(body) => (
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::CONTENT_TYPE, content_type.to_string()),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (
                        header::CONTENT_RANGE,
                        format!("bytes {}-{}/{total}", range.start, range.end),
                    ),
                ],
                body,
            )
                .into_response(),
            Err(_) => missing(),
        },
    }
}

/// 无 Range 响应的告警阈值：超过它的整读值得让运维看见（4K 长片的
/// `play.mp4` 轻松过 200 MiB）。
const WHOLE_SERVE_WARN_BYTES: u64 = 64 * 1024 * 1024;

/// 一次打开、按已知大小精确分配、一次读完——不做 `fs::read` 的
/// metadata/容量试探第二遍，也不让 `read_to_end` 走扩容链。
fn read_whole_exact(path: &Path, total: u64) -> anyhow::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut file =
        std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let want = usize::try_from(total)
        .map_err(|_| anyhow::anyhow!("file larger than addressable memory"))?;
    let mut buf = Vec::with_capacity(want);
    file.read_to_end(&mut buf)
        .with_context(|| format!("read {}", path.display()))?;
    anyhow::ensure!(
        buf.len() == want,
        "file changed size while reading {}: expected {want}, got {}",
        path.display(),
        buf.len()
    );
    Ok(buf)
}

/// 缺失对象一律 404（与媒体下载同一个形状）。
///
/// 以前这里手拼 `r#"{"error":"..."}"#`：形状对，但**契约是硬编码字符串**——
/// 改一个键名就得回来改字面量，而且没人保证它和平台的错误体一致。
/// 现在走 `ApiError`，一份定义。
fn missing() -> Response {
    ApiError::not_found("no such video artifact").into_response()
}
