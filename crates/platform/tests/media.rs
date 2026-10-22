//! WP-05 A1 + A3: upload/download roundtrip, content-addressed dedup, Range,
//! thumbnails, size cap, auth, and the two adversarial cases (digest mismatch
//! and a path-traversal id).

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::{app, call};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use wxlike_platform::{Platform, PlatformOptions, PlatformState};

/// Small caps keep the 413 and thumbnail tests fast; the shipped default is
/// 100 MB (asserted in `production_default_cap_is_100mb`).
fn test_platform(max_media_bytes: usize) -> PlatformState {
    let opts = PlatformOptions {
        session_ttl_ms: 60_000,
        pbkdf2_rounds: 1_000,
        max_media_bytes,
        ..Default::default()
    };
    Platform::in_memory_with(opts).unwrap().shared()
}

fn sha_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Builds a multipart/form-data body with `text` fields and one `file` part.
fn multipart_text(boundary: &str, fields: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, value) in fields {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

fn multipart_file(boundary: &str, fields: &[(&str, &str)], file: (&str, &[u8], &str)) -> Vec<u8> {
    let mut out = multipart_text(boundary, &[]);
    // strip the closing boundary, append the file part, then close
    out.clear();
    for (name, value) in fields {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    let (field, bytes, ctype) = file;
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"{field}\"; filename=\"blob\"\r\n\
             Content-Type: {ctype}\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(bytes);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

async fn upload_with(
    app: &axum::Router,
    token: &str,
    body: Vec<u8>,
    ctype: &str,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/api/media")
        .header(header::CONTENT_TYPE, ctype)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body))
        .unwrap();
    call(app, req).await
}

async fn upload(app: &axum::Router, token: &str, bytes: &[u8]) -> serde_json::Value {
    let boundary = "wxlike-test-boundary";
    let ctype = format!("multipart/form-data; boundary={boundary}");
    let body = multipart_file(
        boundary,
        &[("sha256", &sha_hex(bytes))],
        ("file", bytes, "application/octet-stream"),
    );
    let (status, json) = upload_with(app, token, body, &ctype).await;
    assert_eq!(status, StatusCode::OK, "upload: {json}");
    json
}

async fn get_bytes(
    app: &axum::Router,
    path: &str,
    range: Option<&str>,
) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    get_bytes_as(app, path, range, None).await
}

/// 带凭据下载（issue #14：媒体下载现在要授权）。
async fn get_bytes_as(
    app: &axum::Router,
    path: &str,
    range: Option<&str>,
    token: Option<&str>,
) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
    let mut b = Request::builder().method("GET").uri(path);
    if let Some(r) = range {
        b = b.header(header::RANGE, r);
    }
    if let Some(token) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let resp = {
        use tower::ServiceExt as _;
        app.clone()
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap()
    };
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .map(|c| c.to_bytes().to_vec())
        .unwrap_or_default();
    (status, bytes, headers)
}

async fn signup_pair(app: &axum::Router) -> (String, String) {
    let (status, body) = call(
        app,
        common::req(
            "POST",
            "/api/register",
            Some(serde_json::json!({"name": "alice", "password": "pw"})),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    (
        body["user_id"].as_str().unwrap().to_string(),
        body["token"].as_str().unwrap().to_string(),
    )
}

fn png_of(width: u32, height: u32) -> Vec<u8> {
    let img = image::DynamicImage::new_rgb8(width, height).into_rgba8();
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[tokio::test]
async fn upload_and_download_roundtrip() {
    let state = test_platform(8 * 1024 * 1024);
    let app = app(state);
    let (_uid, token) = signup_pair(&app).await;

    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let reply = upload(&app, &token, &payload).await;
    assert_eq!(reply["media_id"].as_str().unwrap(), sha_hex(&payload));
    assert_eq!(reply["size"].as_u64(), Some(payload.len() as u64));
    assert_eq!(reply["newly_stored"].as_bool(), Some(true));
    // opaque bytes are not an image
    assert_eq!(
        reply["content_type"].as_str().unwrap(),
        "application/octet-stream"
    );

    let (status, got, headers) = get_bytes_as(
        &app,
        &format!("/api/media/{}", reply["media_id"].as_str().unwrap()),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got, payload, "bytes come back identical");
    assert_eq!(
        headers[header::CONTENT_LENGTH].to_str().unwrap(),
        payload.len().to_string()
    );
    assert_eq!(
        headers[header::CONTENT_TYPE].to_str().unwrap(),
        "application/octet-stream"
    );
    assert_eq!(headers[header::ACCEPT_RANGES].to_str().unwrap(), "bytes");
}

#[tokio::test]
async fn same_content_is_stored_once() {
    let state = test_platform(8 * 1024 * 1024);
    let app = app(state.clone());
    let (_uid, token) = signup_pair(&app).await;

    let payload = b"twice uploaded, one object".to_vec();
    let first = upload(&app, &token, &payload).await;
    let second = upload(&app, &token, &payload).await;

    assert_eq!(
        first["media_id"].as_str(),
        second["media_id"].as_str(),
        "content addressing makes the id a function of the bytes"
    );
    assert_eq!(second["newly_stored"].as_bool(), Some(false), "dedup hit");

    // exactly one object for that digest, and it holds the bytes
    let id = first["media_id"].as_str().unwrap();
    let store = state.media_store();
    assert_eq!(store.size_of(id).await.unwrap(), Some(payload.len() as u64));
    assert_eq!(
        store
            .get(id, 0, payload.len() as u64 - 1)
            .await
            .unwrap()
            .unwrap(),
        payload
    );
}

#[tokio::test]
async fn range_returns_a_partial_slice() {
    let state = test_platform(8 * 1024 * 1024);
    let app = app(state);
    let (_uid, token) = signup_pair(&app).await;

    let payload: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
    let reply = upload(&app, &token, &payload).await;
    let url = format!("/api/media/{}", reply["media_id"].as_str().unwrap());

    // A1 asks specifically for bytes=0-99
    let (status, got, headers) = get_bytes_as(&app, &url, Some("bytes=0-99"), Some(&token)).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(got.len(), 100);
    assert_eq!(got, &payload[..100]);
    assert_eq!(
        headers[header::CONTENT_RANGE].to_str().unwrap(),
        "bytes 0-99/1000"
    );

    // open-ended
    let (_, tail, headers) = get_bytes_as(&app, &url, Some("bytes=900-"), Some(&token)).await;
    assert_eq!(tail, &payload[900..]);
    assert_eq!(
        headers[header::CONTENT_RANGE].to_str().unwrap(),
        "bytes 900-999/1000"
    );

    // suffix form: last 10 bytes
    let (_, last10, headers) = get_bytes_as(&app, &url, Some("bytes=-10"), Some(&token)).await;
    assert_eq!(last10, &payload[990..]);
    assert_eq!(
        headers[header::CONTENT_RANGE].to_str().unwrap(),
        "bytes 990-999/1000"
    );

    // middle slice
    let (_, mid, _) = get_bytes_as(&app, &url, Some("bytes=100-199"), Some(&token)).await;
    assert_eq!(mid.len(), 100);
    assert_eq!(mid, &payload[100..200]);

    // unsatisfiable range
    let (status, _, _) = get_bytes_as(&app, &url, Some("bytes=5000-6000"), Some(&token)).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
}

#[tokio::test]
async fn unknown_and_malformed_ids_are_404() {
    let state = test_platform(8 * 1024 * 1024);
    let app = app(state);
    let (_uid, token) = signup_pair(&app).await;
    let unknown = "a".repeat(64);
    // 匿名先被 401 挡住（issue #14）：连"这个 id 存不存在"都不告诉外人
    let (status, _, _) = get_bytes(&app, &format!("/api/media/{unknown}"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // 登录之后才轮到 404
    let (status, _, _) =
        get_bytes_as(&app, &format!("/api/media/{unknown}"), None, Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A3: traversal-shaped ids never touch the filesystem
    for evil in [
        "../../etc/passwd",
        "..%2f..%2fetc%2fpasswd",
        "....//....//etc/hosts",
        "",
        &"b".repeat(63),
        &"c".repeat(65),
        &"g".repeat(64), // right length, not hex
    ] {
        let (status, body, _) =
            get_bytes_as(&app, &format!("/api/media/{evil}"), None, Some(&token)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "id {evil:?} -> {status}");
        assert!(
            !String::from_utf8_lossy(&body).contains("root:"),
            "no /etc/passwd content may leak"
        );
    }
}

#[tokio::test]
async fn oversized_upload_is_413() {
    // cap lowered to 1 KiB to exercise the same code path as the 100 MB default
    let state = test_platform(1_024);
    let app = app(state);
    let (_uid, token) = signup_pair(&app).await;

    let big = vec![7u8; 2_048];
    let boundary = "wxlike-big";
    let body = multipart_file(boundary, &[], ("file", &big, "application/octet-stream"));
    let (status, json) = upload_with(
        &app,
        &token,
        body,
        &format!("multipart/form-data; boundary={boundary}"),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{json}");

    // and a small upload still works through the same platform
    let payload = b"under the cap".to_vec();
    let reply = upload(&app, &token, &payload).await;
    let id = reply["media_id"].as_str().unwrap().to_string();
    let (status, small, _) =
        get_bytes_as(&app, &format!("/api/media/{id}"), None, Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(small, b"under the cap");
}

#[tokio::test]
async fn thumbnails_are_scaled_to_the_longest_edge() {
    let state = test_platform(16 * 1024 * 1024);
    let app = app(state);
    let (_uid, token) = signup_pair(&app).await;

    let png = png_of(1_024, 768);
    let reply = upload(&app, &token, &png).await;
    let id = reply["media_id"].as_str().unwrap().to_string();
    assert_eq!(reply["content_type"].as_str().unwrap(), "image/png");
    assert_eq!(reply["has_thumb"].as_bool(), Some(true));

    let (status, thumb, headers) = get_bytes_as(
        &app,
        &format!("/api/media/{id}?thumb=1"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE].to_str().unwrap(), "image/png");
    let img = image::load_from_memory(&thumb).expect("thumb decodes");
    assert!(
        img.width().max(img.height()) <= 256,
        "longest edge {}x{}",
        img.width(),
        img.height()
    );
    // aspect ratio kept: 1024x768 -> 256x192
    assert_eq!((img.width(), img.height()), (256, 192));

    // the original is untouched and still full size
    let (_, original, _) =
        get_bytes_as(&app, &format!("/api/media/{id}"), None, Some(&token)).await;
    assert_eq!(original, png);
}

#[tokio::test]
async fn small_images_get_a_thumb_at_original_size() {
    let state = test_platform(16 * 1024 * 1024);
    let app = app(state);
    let (_uid, token) = signup_pair(&app).await;
    let png = png_of(64, 48);
    let reply = upload(&app, &token, &png).await;
    let id = reply["media_id"].as_str().unwrap().to_string();
    let (status, thumb, _) = get_bytes_as(
        &app,
        &format!("/api/media/{id}?thumb=1"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let img = image::load_from_memory(&thumb).unwrap();
    assert_eq!((img.width(), img.height()), (64, 48), "no upscaling");
}

#[tokio::test]
async fn upload_requires_a_token() {
    let state = test_platform(8 * 1024 * 1024);
    let app = app(state);
    let boundary = "wxlike-anon";
    let body = multipart_file(boundary, &[], ("file", b"no auth".as_slice(), "text/plain"));
    let (status, json) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/media")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{json}");
}

#[tokio::test]
async fn declared_sha256_must_match_the_bytes() {
    // A3
    let state = test_platform(8 * 1024 * 1024);
    let app = app(state);
    let (_uid, token) = signup_pair(&app).await;

    let payload = b"honest content".to_vec();
    let lie = "f".repeat(64);
    let boundary = "wxlike-liar";
    let body = multipart_file(
        boundary,
        &[("sha256", &lie)],
        ("file", &payload, "application/octet-stream"),
    );
    let (status, json) = upload_with(
        &app,
        &token,
        body,
        &format!("multipart/form-data; boundary={boundary}"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{json}");

    // the declared digest may be uppercase — same content then succeeds
    let upper = sha_hex(&payload).to_uppercase();
    let body = multipart_file(
        boundary,
        &[("sha256", &upper)],
        ("file", &payload, "application/octet-stream"),
    );
    let (status, json) = upload_with(
        &app,
        &token,
        body,
        &format!("multipart/form-data; boundary={boundary}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["media_id"].as_str().unwrap(), sha_hex(&payload));
}

#[tokio::test]
async fn empty_and_missing_parts_are_refused() {
    let state = test_platform(8 * 1024 * 1024);
    let app = app(state);
    let (_uid, token) = signup_pair(&app).await;
    let boundary = "wxlike-empty";

    // file part present but zero bytes
    let body = multipart_file(boundary, &[], ("file", &[], "application/octet-stream"));
    let (status, _) = upload_with(
        &app,
        &token,
        body,
        &format!("multipart/form-data; boundary={boundary}"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "empty upload");

    // no file part at all
    let body = multipart_text(boundary, &[("sha256", &"a".repeat(64))]);
    let (status, json) = upload_with(
        &app,
        &token,
        body,
        &format!("multipart/form-data; boundary={boundary}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");

    // an unknown field name is rejected rather than silently stored
    let body = multipart_file(boundary, &[], ("payload", b"x".as_slice(), "text/plain"));
    let (status, _) = upload_with(
        &app,
        &token,
        body,
        &format!("multipart/form-data; boundary={boundary}"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "unexpected field");
}

#[tokio::test]
async fn content_type_is_sniffed_not_trusted() {
    let state = test_platform(8 * 1024 * 1024);
    let app = app(state);
    let (_uid, token) = signup_pair(&app).await;

    let png = png_of(300, 200);
    let boundary = "wxlike-sniff";
    // declared as text/plain, actually a PNG
    let body = multipart_file(boundary, &[], ("file", &png, "text/plain"));
    let (status, json) = upload_with(
        &app,
        &token,
        body,
        &format!("multipart/form-data; boundary={boundary}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["content_type"].as_str().unwrap(), "image/png");
    assert_eq!(json["has_thumb"].as_bool(), Some(true));
}

#[tokio::test]
async fn thumb_flag_is_false_for_opaque_bytes() {
    let state = test_platform(8 * 1024 * 1024);
    let app = app(state);
    let (_uid, token) = signup_pair(&app).await;
    let reply = upload(&app, &token, b"just a zip-ish blob").await;
    assert_eq!(reply["has_thumb"].as_bool(), Some(false));
    let id = reply["media_id"].as_str().unwrap().to_string();
    let (status, _, _) = get_bytes_as(
        &app,
        &format!("/api/media/{id}?thumb=1"),
        None,
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "no thumbnail was generated");
}

#[test]
fn production_default_cap_is_100mb() {
    assert_eq!(
        PlatformOptions::default().max_media_bytes,
        100 * 1024 * 1024
    );
    assert_eq!(wxlike_platform::MAX_MEDIA_BYTES, 100 * 1024 * 1024);
    assert_eq!(wxlike_platform::THUMB_MAX_EDGE, 256);
}

#[tokio::test]
async fn injected_store_is_used_for_read_and_write() {
    // The trait is the seam WP-05 asked for: swap the backing store and the
    // handlers follow, which is what an S3/MinIO implementation would rely on.
    #[derive(Default)]
    struct InMemory(std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>);

    #[async_trait::async_trait]
    impl wxlike_platform::MediaStore for InMemory {
        async fn put(&self, sha: &str, bytes: &[u8]) -> wxlike_platform::Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(sha.to_string(), bytes.to_vec());
            Ok(())
        }
        async fn get(
            &self,
            sha: &str,
            start: u64,
            end: u64,
        ) -> wxlike_platform::Result<Option<Vec<u8>>> {
            let guard = self.0.lock().unwrap();
            let Some(all) = guard.get(sha) else {
                return Ok(None);
            };
            if all.is_empty() {
                return Ok(Some(Vec::new()));
            }
            let last = (end as usize).min(all.len() - 1);
            let first = (start as usize).min(all.len() - 1);
            Ok(Some(all[first..=last].to_vec()))
        }
        async fn size_of(&self, sha: &str) -> wxlike_platform::Result<Option<u64>> {
            Ok(self.0.lock().unwrap().get(sha).map(|b| b.len() as u64))
        }
        async fn put_thumb(&self, sha: &str, png: &[u8]) -> wxlike_platform::Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(format!("{sha}.thumb"), png.to_vec());
            Ok(())
        }
        async fn get_thumb(&self, sha: &str) -> wxlike_platform::Result<Option<Vec<u8>>> {
            Ok(self.0.lock().unwrap().get(&format!("{sha}.thumb")).cloned())
        }
    }

    let state = test_platform(8 * 1024 * 1024);
    // point the platform at a directory it will not write into, then override
    let scratch = state.media_store();
    let custom: Arc<dyn wxlike_platform::MediaStore> = Arc::new(InMemory::default());
    state.set_media_store(custom.clone());
    let app = app(state);
    let (_uid, token) = signup_pair(&app).await;

    let payload = b"through the injected store".to_vec();
    let reply = upload(&app, &token, &payload).await;
    let id = reply["media_id"].as_str().unwrap().to_string();

    // the custom store has it...
    assert_eq!(
        custom.size_of(&id).await.unwrap(),
        Some(payload.len() as u64)
    );
    // ...and the default local-directory store does not
    assert_eq!(scratch.size_of(&id).await.unwrap(), None);

    let (status, got, headers) = get_bytes_as(
        &app,
        &format!("/api/media/{id}"),
        Some("bytes=0-9"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(got, &payload[..10]);
    assert_eq!(
        headers[header::CONTENT_RANGE].to_str().unwrap(),
        format!("bytes 0-9/{}", payload.len())
    );
}
