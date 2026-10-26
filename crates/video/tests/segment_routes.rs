//! 播放面路由（审计第 5 轮）：档位清单里的**相对**分片 URI 必须能被解析。
//!
//! ffmpeg 产出的 `hls/{height}/playlist.m3u8` 写的是 `seg_000.ts` 这种相对
//! 名；标准播放器相对清单 URL 解析，落点是 `/api/video/{id}/{height}/seg_000.ts`
//! （没有 `/seg/` 段）。这些用例不起 ffmpeg：空对象目录下命中路由 = 404
//! **"no such video artifact"**（错误文案来自 handler），没命中路由 = axum
//! 的裸 404（空 body）——用 body 区分"路由在、对象没有"与"路由不在"。
//! 顺带钉住 `../` 变体在碰文件系统之前就被 `check_segment_name` 拒掉。

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;
use wxlike_video::{check_segment_ok, routes, VideoState, VideoStore};

fn app(tag: &str) -> axum::Router {
    // 目录不必清理：`support::temp_root` 每次调用唯一，留给本次测试的库文件
    routes(VideoState {
        store: VideoStore::new(support::temp_root(tag)),
    })
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, String) {
    let res = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = String::from_utf8_lossy(&res.into_body().collect().await.unwrap().to_bytes())
        .into_owned();
    (status, body)
}

/// 64 位 hex 的合法对象 id（内容寻址，不必真存在——我们要的是路由行为）。
const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn relative_segment_names_resolve_to_a_route() {
    let app = app("relative-seg");

    // 相对名（档位清单里的写法）：命中 segment_named → "对象缺失"的 404
    let (status, body) = get(&app, &format!("/api/video/{ID}/720/seg_000.ts")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body.contains("no such video artifact"),
        "路由必须命中（handler 的 404 文案），got: {body}"
    );

    // 老的 /seg/ 写法继续可用
    let (status, body) = get(&app, &format!("/api/video/{ID}/720/seg/seg_000.ts")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("no such video artifact"), "{body}");

    // 档位清单的静态路由不被通配吃掉
    let (status, body) = get(&app, &format!("/api/video/{ID}/720/playlist.m3u8")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("no such video artifact"), "{body}");
}

#[tokio::test]
async fn malformed_names_and_renditions_are_refused_before_touching_disk() {
    let app = app("malformed-seg");

    // 越权名：`..` 一律 404（malformed），文案来自校验器而非存储层
    for uri in [
        format!("/api/video/{ID}/720/../meta.json"),
        format!("/api/video/{ID}/seg/..%2F..%2Fmeta.json"),
    ] {
        let (status, _) = get(&app, &uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} must not resolve");
    }
    // 非数字档位：malformed rendition
    let (status, body) = get(&app, &format!("/api/video/{ID}/7x2/seg_000.ts")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // 校验器的纯函数口径（供上面的路由行为对照）
    assert!(check_segment_ok("seg_000.ts"));
    assert!(check_segment_ok("720/seg_000.ts"));
    assert!(!check_segment_ok("../meta.json"));
}
