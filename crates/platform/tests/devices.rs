//! WP-06: device registration endpoints — register, latest-per-platform
//! replacement, multi-platform coexistence, auth.

mod common;

use axum::http::StatusCode;
use serde_json::json;
use wxlike_platform::DeviceReg;

use common::{app, call, platform, req, signup};

#[tokio::test]
async fn register_and_list_own_devices() {
    let state = platform();
    let app = app(state.clone());
    let (_, token) = signup(&app, "alice").await;

    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/devices",
            Some(json!({"platform": "android", "device_token": "tok-1"})),
            Some(&token),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = call(&app, req("GET", "/api/devices", None, Some(&token))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["platform"], "android");
    assert_eq!(rows[0]["device_token"], "tok-1");
}

#[tokio::test]
async fn same_platform_reregister_replaces_token() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, token) = signup(&app, "alice").await;

    for tok in ["old-token", "new-token"] {
        call(
            &app,
            req(
                "POST",
                "/api/devices",
                Some(json!({"platform": "ios", "device_token": tok})),
                Some(&token),
            ),
        )
        .await;
    }
    let devices = state.devices_of(&alice_id).unwrap();
    assert_eq!(devices.len(), 1, "one row per platform: {devices:?}");
    assert_eq!(devices[0].device_token, "new-token", "latest wins");
}

#[tokio::test]
async fn different_platforms_coexist() {
    let state = platform();
    let app = app(state.clone());
    let (alice_id, token) = signup(&app, "alice").await;

    for p in ["android", "ios", "desktop"] {
        call(
            &app,
            req(
                "POST",
                "/api/devices",
                Some(json!({"platform": p, "device_token": format!("tok-{p}")})),
                Some(&token),
            ),
        )
        .await;
    }
    let mut platforms: Vec<_> = state
        .devices_of(&alice_id)
        .unwrap()
        .into_iter()
        .map(|d| d.platform)
        .collect();
    platforms.sort();
    assert_eq!(platforms, vec!["android", "desktop", "ios"]);
}

#[tokio::test]
async fn devices_endpoints_require_auth() {
    let state = platform();
    let app = app(state.clone());
    let (status, _) = call(
        &app,
        req(
            "POST",
            "/api/devices",
            Some(json!({"platform": "android", "device_token": "t"})),
            Some("bogus"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&app, req("GET", "/api/devices", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn malformed_platforms_and_tokens_are_refused() {
    let state = platform();
    let app = app(state.clone());
    let (_, token) = signup(&app, "alice").await;

    for bad in [
        json!({"platform": "", "device_token": "t"}),
        json!({"platform": "bad platform!", "device_token": "t"}),
        json!({"platform": "ok", "device_token": ""}),
    ] {
        let (status, body) = call(
            &app,
            req("POST", "/api/devices", Some(bad.clone()), Some(&token)),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body} for {bad}");
    }
}

#[test]
fn library_roundtrip_replaces_and_keeps() {
    let platform = wxlike_platform::Platform::in_memory().unwrap();
    platform.register("alice", "pw").unwrap();
    let id = platform.user_id_by_name("alice").unwrap().unwrap();

    assert!(!platform
        .register_device(
            &id,
            &DeviceReg {
                platform: "android".into(),
                device_token: "a1".into(),
            },
        )
        .unwrap());
    assert!(
        platform
            .register_device(
                &id,
                &DeviceReg {
                    platform: "android".into(),
                    device_token: "a2".into(),
                },
            )
            .unwrap(),
        "second register on the same platform replaces"
    );
    platform
        .register_device(
            &id,
            &DeviceReg {
                platform: "web".into(),
                device_token: "w1".into(),
            },
        )
        .unwrap();
    assert_eq!(platform.devices_of(&id).unwrap().len(), 2);
}
