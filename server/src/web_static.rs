//! A2 — 服务端直接发网页，不需要额外 nginx。
//!
//! `WXLIKE_WEB_DIR` 指向 `scripts/build-web.sh` 的产物目录（`ui/dist-web`），
//! 里面只有三样东西：
//!
//! ```text
//! index.html              ← /app、/app/            Cache-Control: no-store
//! assets/app.<hash>.js    ┐
//! assets/styles.<hash>.css┘ /app/assets/*          Cache-Control: …, immutable
//! ```
//!
//! 缓存策略是这套文件名换来的：资源名里带内容摘要，改一个字节就换名字，所以可以
//! `immutable` 一年；壳必须每次拿新的，否则用户会拿到引用旧 bundle 的页面。
//!
//! 未配置（或配了一个不存在的目录）时 `/app` 明确返回 404 并说明原因——
//! 不 panic，也不发一个看起来能用、其实空白页。

use std::path::{Path, PathBuf};

use axum::http::header::{HeaderValue, CACHE_CONTROL};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::set_header::SetResponseHeader;

/// 网页包目录的环境变量名。
pub const ENV_WEB_DIR: &str = "WXLIKE_WEB_DIR";

/// 文件名带内容摘要，因此可以放心长存。
const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// 壳不能缓存：它引用的是带摘要的资源名。
const CACHE_NO_STORE: &str = "no-store";

/// 读环境并挂载。装配处（`server/src/lib.rs`）只需要 `.merge(web_static::routes())`。
pub fn routes() -> Router {
    routes_from(web_dir().as_deref())
}

/// 环境变量里的目录；空或未设置 = 不发网页。
pub fn web_dir() -> Option<PathBuf> {
    let raw = std::env::var(ENV_WEB_DIR).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

/// 纯函数版本：测试不必改全局环境变量（并发改 env 会让测试互相污染）。
pub fn routes_from(dir: Option<&Path>) -> Router {
    let Some(dir) = dir.filter(|p| p.is_dir()) else {
        return disabled_routes(dir);
    };
    let index = dir.join("index.html");
    let assets = dir.join("assets");
    tracing::info!(
        "网页端已挂载：/app → {}（{}）",
        dir.display(),
        if index.is_file() {
            "index.html 就绪"
        } else {
            "缺 index.html"
        }
    );
    // 壳挂两处（`/app` 与 `/app/`）：同一个类型，`.clone()` 即可
    let shell = SetResponseHeader::overriding(
        ServeFile::new(index),
        CACHE_CONTROL,
        HeaderValue::from_static(CACHE_NO_STORE),
    );
    Router::new()
        .route_service("/app", shell.clone())
        .route_service("/app/", shell)
        .nest_service(
            "/app/assets",
            SetResponseHeader::overriding(
                ServeDir::new(assets),
                CACHE_CONTROL,
                HeaderValue::from_static(CACHE_IMMUTABLE),
            ),
        )
        .route("/app/{*rest}", any(not_found))
}

/// `/app` 明确 404 并说清原因：不 panic，也不发一个看着能用其实空白的壳。
fn disabled_routes(dir: Option<&Path>) -> Router {
    let reason = match dir {
        None => format!("{ENV_WEB_DIR} 未设置"),
        Some(p) => format!("{ENV_WEB_DIR}={} 不是目录", p.display()),
    };
    tracing::warn!("网页端未挂载：{reason}；/app 将返回 404");
    Router::new()
        .route("/app", any(disabled))
        .route("/app/", any(disabled))
        .route("/app/{*rest}", any(disabled))
}

async fn disabled() -> Response {
    (
        axum::http::StatusCode::NOT_FOUND,
        format!("网页端未启用：设置 {ENV_WEB_DIR} 指向 ui/dist-web 后重启"),
    )
        .into_response()
}

async fn not_found() -> Response {
    (
        axum::http::StatusCode::NOT_FOUND,
        format!("{ENV_WEB_DIR} 目录下没有这个文件"),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use std::path::PathBuf;

    /// 一个最小网页包：壳 + 一个带摘要的资源 + 一个"目录外"的敏感文件。
    fn fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "wxlike-web-static-{}-{}",
            std::process::id(),
            uuid()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("assets")).expect("create fixture");
        std::fs::write(
            root.join("index.html"),
            "<!doctype html><title>wxlike</title>",
        )
        .unwrap();
        std::fs::write(root.join("assets/app.deadbeefcafe.js"), "console.log(1)").unwrap();
        // 越界目标：不在 assets/ 里，任何路径都不该把它发出去
        std::fs::write(root.join("secret.txt"), "token=leak").unwrap();
        root
    }

    /// 每个 fixture 一个唯一后缀。
    ///
    /// 只靠纳秒时间戳是不够的：Windows 的时钟粒度约 15ms，两个并行用例
    /// 会拿到同一个值，于是共用一个目录——其中一个用例收尾时
    /// `remove_dir_all`，另一个就读到 404（CI 上真的这么红过）。计数器
    /// 保证同进程内绝不相同。
    fn uuid() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        nanos
            .wrapping_mul(1_000_003)
            .wrapping_add(SEQ.fetch_add(1, Ordering::Relaxed))
    }

    /// 起真 HTTP 服务（与 `server/tests/*` 同一思路：不 mock 传输层）。
    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    /// 测试用 agent：4xx/5xx **不是**错误——这些用例断言的正是 404/405。
    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into()
    }

    fn head(url: &str) -> (StatusCode, Option<String>, Option<String>) {
        // ureq 3：状态码不是错误（agent 上关了 http_status_as_error），
        // 404 也走 Ok；响应头从 `http::HeaderMap` 取。
        let resp = agent()
            .head(url)
            .call()
            .unwrap_or_else(|e| panic!("head {url}: {e}"));
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        (
            resp.status(),
            header("content-type"),
            header("cache-control"),
        )
    }

    fn get(url: &str) -> (StatusCode, Option<String>, Option<String>, String) {
        let mut resp = agent()
            .get(url)
            .call()
            .unwrap_or_else(|e| panic!("get {url}: {e}"));
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        (
            resp.status(),
            header("content-type"),
            header("cache-control"),
            resp.body_mut().read_to_string().unwrap_or_default(),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unconfigured_answers_404_without_panicking() {
        let base = serve(routes_from(None)).await;
        for path in ["/app", "/app/", "/app/whatever.js"] {
            let (status, _, _, body) = get(&format!("{base}{path}"));
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(
                body.contains("WXLIKE_WEB_DIR"),
                "{path} 要说明怎么开启，实际: {body}"
            );
        }
        // HEAD（`curl -I`）也必须给出结论，不能 405
        let (status, _, _) = head(&format!("{base}/app/"));
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shell_is_html_and_never_cached() {
        let root = fixture();
        let base = serve(routes_from(Some(&root))).await;
        let (status, ctype, cache) = head(&format!("{base}/app/"));
        assert_eq!(status, StatusCode::OK, "A2 验收：curl -I /app/ → 200");
        assert_eq!(ctype.as_deref(), Some("text/html"), "content type");
        assert_eq!(cache.as_deref(), Some(CACHE_NO_STORE));
        let (status, _, cache2, body) = get(&format!("{base}/app"));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cache2.as_deref(), Some(CACHE_NO_STORE));
        assert!(body.contains("wxlike"), "壳的内容: {body}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hashed_assets_are_immutable() {
        let root = fixture();
        let base = serve(routes_from(Some(&root))).await;
        let (status, ctype, cache, body) = get(&format!("{base}/app/assets/app.deadbeefcafe.js"));
        assert_eq!(status, StatusCode::OK);
        // mime 0.6 起 `.js` 归 `text/javascript`（同一个类型，别名不再接受）
        assert_eq!(ctype.as_deref(), Some("text/javascript"), "{ctype:?}");
        assert_eq!(cache.as_deref(), Some(CACHE_IMMUTABLE));
        assert_eq!(body, "console.log(1)");
        let (missing, _, _, _) = get(&format!("{base}/app/assets/app.000000000000.js"));
        assert_eq!(missing, StatusCode::NOT_FOUND, "换名字就是没有");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cannot_walk_out_of_the_asset_dir() {
        let root = fixture();
        let base = serve(routes_from(Some(&root))).await;
        // 目录外的文件在同目录里（secret.txt 不在 assets/），两种写法都不该读到内容
        for path in [
            "/app/assets/../secret.txt",
            "/app/assets/%2e%2e/secret.txt",
            "/app/assets/..%2fsecret.txt",
        ] {
            let (status, _, _, body) = &get(&format!("{base}{path}"));
            assert_ne!(*status, StatusCode::OK, "{path} 越界成功");
            assert!(!body.contains("token=leak"), "{path} 泄露了内容");
        }
        // 非 GET/HEAD 的方法不能变成写入口
        let status = agent()
            .post(&format!("{base}/app/assets/app.deadbeefcafe.js"))
            .send_empty()
            .map(|r| r.status().as_u16())
            .expect("POST 不该成功");
        assert_eq!(status, 405, "静态目录只读");
        let _ = std::fs::remove_dir_all(&root);
    }
}
