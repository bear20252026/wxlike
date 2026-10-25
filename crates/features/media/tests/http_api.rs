//! HTTP contract tests: authentication, error shape, signed/revocable
//! downloads, range reads and the platform fixture.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;
use wxlike_media::http::{fixture_verifier, MediaApp};
use wxlike_media::*;

struct Port;
impl AudiencePort for Port {
    fn may_read_conversation(&self, _: &str, _: &str) -> bool {
        false
    }
    fn may_read_group(&self, _: &str, _: &str) -> bool {
        false
    }
}

fn service(root: &std::path::Path) -> Arc<MediaService> {
    Arc::new(MediaService::open(MediaConfig::new(root), None, Arc::new(Port)).unwrap())
}

fn app(service: Arc<MediaService>) -> Router {
    MediaApp::new(
        service,
        fixture_verifier(&[("tok-a", "u1"), ("tok-b", "u2")]),
    )
    .router()
}

fn authed(method: &str, uri: &str, token: &str, body: Option<serde_json::Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    match body {
        Some(value) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap()
}

#[tokio::test]
async fn http_upload_access_download_and_revoke() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(service(dir.path()));
    let (status, _, body) = send(
        &app,
        authed("GET", "/api/v1/media/capabilities", "tok-a", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["max_object_bytes"], GIB);

    let payload = b"hello http media";
    let (status, _, raw) = send(
        &app,
        authed(
            "POST",
            "/api/v1/uploads",
            "tok-a",
            Some(serde_json::json!({
                "purpose": "file",
                "name": "hello.txt",
                "size": payload.len(),
                "mime": "text/plain",
                "digest": ids::sha256_hex(payload),
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{:?}", json(&raw));
    let start = json(&raw);
    let upload_id = start["upload_id"].as_str().unwrap().to_string();
    let chunk = Request::builder()
        .method("PUT")
        .uri(format!("/api/v1/uploads/{upload_id}/chunks?offset=0"))
        .header(header::AUTHORIZATION, "Bearer tok-a")
        .header("x-chunk-sha256", ids::sha256_hex(payload))
        .body(Body::from(payload.to_vec()))
        .unwrap();
    let (status, _, body) = send(&app, chunk).await;
    assert_eq!(status, StatusCode::OK, "{:?}", json(&body));
    let (status, _, raw) = send(
        &app,
        authed(
            "POST",
            &format!("/api/v1/uploads/{upload_id}/complete"),
            "tok-a",
            Some(serde_json::json!({ "digest": ids::sha256_hex(payload) })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{:?}", json(&raw));
    let done = json(&raw);
    let object_id = done["object_id"].as_str().unwrap().to_string();

    let (status, _, body) = send(
        &app,
        authed(
            "POST",
            &format!("/api/v1/objects/{object_id}/access"),
            "tok-a",
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let signed = json(&body);
    let url = signed["url"].as_str().unwrap().to_string();

    let (status, headers, body) = send(
        &app,
        Request::builder().uri(&url).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, payload);
    assert_eq!(headers[header::CONTENT_TYPE], "text/plain");
    assert_eq!(headers[header::ACCEPT_RANGES], "bytes");

    let ranged = Request::builder()
        .uri(&url)
        .header(header::RANGE, "bytes=0-4")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(&app, ranged).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, b"hello");
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 0-4/16");

    let (status, _, body) = send(
        &app,
        authed(
            "POST",
            &format!("/api/v1/objects/{object_id}/revoke"),
            "tok-a",
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["epoch"], 1);
    let (status, _, _) = send(
        &app,
        Request::builder().uri(&url).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
}

#[tokio::test]
async fn http_denies_foreign_and_forged_objects() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path());
    let app = app(Arc::clone(&service));
    let start = service
        .upload_start(
            "u1",
            UploadStart {
                purpose: Purpose::File,
                name: "a.txt".into(),
                size: 1,
                mime: "text/plain".into(),
                digest: ids::sha256_hex(b"a"),
            },
        )
        .unwrap();
    service
        .upload_chunk("u1", &start.upload_id, 0, b"a", None)
        .unwrap();
    let done = service
        .upload_complete("u1", &start.upload_id, Some(&ids::sha256_hex(b"a")))
        .unwrap();
    let (status, _, body) = send(
        &app,
        authed(
            "POST",
            &format!("/api/v1/objects/{}/access", done.object_id),
            "tok-b",
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let _ = body;
    let (status, _, _) = send(
        &app,
        authed("POST", "/api/v1/objects/not-a-hash/access", "tok-b", None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = send(
        &app,
        authed("GET", "/api/v1/media/capabilities", "bad", None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn http_file_center_and_platform_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let service = service(dir.path());
    let start = service
        .upload_start(
            "u1",
            UploadStart {
                purpose: Purpose::File,
                name: "doc.txt".into(),
                size: 3,
                mime: "text/plain".into(),
                digest: ids::sha256_hex(b"doc"),
            },
        )
        .unwrap();
    service
        .upload_chunk("u1", &start.upload_id, 0, b"doc", None)
        .unwrap();
    let done = service
        .upload_complete("u1", &start.upload_id, Some(&ids::sha256_hex(b"doc")))
        .unwrap();
    let app = app(service);
    let (status, _, body) = send(
        &app,
        authed("GET", "/api/v1/files?sort=name", "tok-a", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["items"][0]["name"], "doc.txt");
    assert_eq!(json(&body)["has_more"], false);

    let (status, _, body) = send(
        &app,
        authed(
            "PATCH",
            &format!("/api/v1/files/{}", done.file_id),
            "tok-a",
            Some(serde_json::json!({ "name": "renamed.txt" })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["name"], "renamed.txt");

    let (status, _, body) = send(
        &app,
        authed(
            "GET",
            "/api/v1/platform/capabilities?platform=web",
            "tok-a",
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["requires_user_gesture"], true);

    let (status, _, body) = send(
        &app,
        authed(
            "POST",
            "/api/v1/platform/requests",
            "tok-a",
            Some(serde_json::json!({
                "platform": "web",
                "request": {
                    "request_id": "r1",
                    "user_gesture": false,
                    "capability": "capture_screen",
                    "interactive": true
                }
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&body)["status"], "permission_denied");
}
