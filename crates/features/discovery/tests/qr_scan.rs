use std::sync::Mutex;

use async_trait::async_trait;
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use wxlike_discovery::crypto::MemoryNonceStore;
use wxlike_discovery::qr::{
    render_png, CameraPort, CameraSession, DeeplinkPolicy, QrCodec, QrScanner, QrTarget, ScanAction,
};
use wxlike_discovery::DiscoveryError;

const KEY: &[u8] = b"g07-test-signing-key";
const NOW: i64 = 1_700_000_000_000;

fn scanner(policy: DeeplinkPolicy) -> QrScanner<MemoryNonceStore> {
    QrScanner::new(QrCodec::new(KEY), KEY, policy, MemoryNonceStore::default())
}

fn strict_scanner() -> QrScanner<MemoryNonceStore> {
    scanner(DeeplinkPolicy::strict())
}

fn friend_wire(exp: Option<i64>) -> String {
    QrCodec::new(KEY)
        .encode(
            &QrTarget::Friend {
                user_id: "alice".into(),
            },
            "n-friend",
            exp,
        )
        .unwrap()
}

fn png_of(wire: &str) -> Vec<u8> {
    render_png(wire).unwrap()
}

#[test]
fn signed_friend_qr_roundtrips_through_real_png_decode() {
    let wire = friend_wire(Some(NOW + 60_000));
    let png = png_of(&wire);
    let result = strict_scanner().scan_image("bob", &png, NOW).unwrap();
    assert!(result.requires_confirmation, "扫码不得自动加好友");
    assert!(matches!(
        result.action,
        ScanAction::Signed(QrTarget::Friend { ref user_id }) if user_id == "alice"
    ));
    assert!(result.stats.elapsed_ms < 10_000);
}

#[test]
fn expired_and_tampered_codes_fail() {
    let expired = friend_wire(Some(NOW - 1));
    assert_eq!(
        strict_scanner()
            .scan_text("bob", &expired, NOW)
            .unwrap_err(),
        DiscoveryError::Expired
    );

    let wire = friend_wire(Some(NOW + 60_000));
    let bad_signature = format!("{wire}x");
    assert_eq!(
        strict_scanner()
            .scan_text("bob", &bad_signature, NOW)
            .unwrap_err(),
        DiscoveryError::InvalidArgument("signature")
    );

    let mut tampered: Vec<char> = wire.chars().collect();
    let payload_pos = tampered.iter().position(|c| *c == '?').unwrap();
    tampered[payload_pos + 4] = if tampered[payload_pos + 4] == 'A' {
        'B'
    } else {
        'A'
    };
    let tampered: String = tampered.into_iter().collect();
    assert_eq!(
        strict_scanner()
            .scan_text("bob", &tampered, NOW)
            .unwrap_err(),
        DiscoveryError::InvalidArgument("signature")
    );
}

#[test]
fn payment_qr_requires_confirmation_and_rejects_replay() {
    let codec = QrCodec::new(KEY);
    let wire = codec
        .encode(
            &QrTarget::Payment {
                intent_id: "pi_1".into(),
                amount_minor: 12_300,
                currency: "CNY".into(),
                payee_id: "shop_1".into(),
            },
            "pay-once",
            Some(NOW + 60_000),
        )
        .unwrap();
    let scan = strict_scanner();
    let first = scan.scan_text("bob", &wire, NOW).unwrap();
    assert!(first.requires_confirmation);
    let (token, intent, amount, currency, payee) = match first.action {
        ScanAction::Payment {
            intent_id,
            amount_minor,
            currency,
            payee_id,
            confirm_token,
            ..
        } => (confirm_token, intent_id, amount_minor, currency, payee_id),
        other => panic!("unexpected {other:?}"),
    };
    let approval = scan
        .approve_payment("bob", &intent, amount, &currency, &payee, &token, NOW)
        .unwrap();
    assert_eq!(approval.amount_minor, 12_300);

    assert_eq!(
        scan.scan_text("bob", &wire, NOW).unwrap_err(),
        DiscoveryError::Conflict("payment code replayed")
    );
}

#[test]
fn payment_confirmation_is_bound_to_actor_amount_and_payee() {
    let codec = QrCodec::new(KEY);
    let wire = codec
        .encode(
            &QrTarget::Payment {
                intent_id: "pi_2".into(),
                amount_minor: 5_000,
                currency: "CNY".into(),
                payee_id: "shop_2".into(),
            },
            "pay-bind",
            Some(NOW + 60_000),
        )
        .unwrap();
    let scan = strict_scanner();
    let result = scan.scan_text("bob", &wire, NOW).unwrap();
    let token = match result.action {
        ScanAction::Payment { confirm_token, .. } => confirm_token,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(
        scan.approve_payment("bob", "pi_2", 50_000, "CNY", "shop_2", &token, NOW)
            .unwrap_err(),
        DiscoveryError::Unauthorized
    );
    assert_eq!(
        scan.approve_payment("carol", "pi_2", 5_000, "CNY", "shop_2", &token, NOW)
            .unwrap_err(),
        DiscoveryError::Unauthorized
    );
    assert_eq!(
        scan.approve_payment("bob", "pi_2", 5_000, "CNY", "shop_3", &token, NOW)
            .unwrap_err(),
        DiscoveryError::Unauthorized
    );
}

#[test]
fn external_links_never_auto_execute_and_respect_host_allowlist() {
    let strict = strict_scanner();
    let result = strict
        .scan_text("bob", "https://evil.example/x", NOW)
        .unwrap();
    assert!(result.requires_confirmation);
    assert!(matches!(result.action, ScanAction::External { .. }));
    assert_eq!(
        strict
            .execute_external("https://evil.example/x")
            .unwrap_err(),
        DiscoveryError::BlockedExternal("host not allowed")
    );

    let open = scanner(DeeplinkPolicy::with_hosts(vec!["wxlike.com".into()], false).unwrap());
    assert_eq!(
        open.execute_external("https://a.wxlike.com/path").unwrap(),
        "https://a.wxlike.com/path"
    );
    for blocked in [
        "https://evilwxlike.com/path",
        "https://wxlike.com@evil.example/path",
        "http://wxlike.com/path",
        "javascript://alert(1)",
        "javascript:alert(1)",
        "file:///etc/passwd",
    ] {
        assert!(
            open.execute_external(blocked).is_err(),
            "{blocked} should be blocked from execution"
        );
    }
    for unsafe_scheme in [
        "http://wxlike.com/path",
        "javascript://alert(1)",
        "javascript:alert(1)",
        "file:///etc/passwd",
    ] {
        assert!(
            open.scan_text("bob", unsafe_scheme, NOW).is_err(),
            "{unsafe_scheme} should not scan as a safe action"
        );
    }
    let lookalike = open
        .scan_text("bob", "https://evilwxlike.com/path", NOW)
        .unwrap();
    assert!(lookalike.requires_confirmation);
}

#[test]
fn weak_light_rotated_and_large_images_stay_within_budget() {
    let wire = friend_wire(Some(NOW + 60_000));
    let png = png_of(&wire);
    let source = image::load_from_memory(&png).unwrap().to_rgb8();

    let dark = RgbImage::from_fn(source.width(), source.height(), |x, y| {
        let pixel = source.get_pixel(x, y);
        Rgb([pixel[0] / 3, pixel[1] / 3, pixel[2] / 3])
    });
    let rotated = image::imageops::rotate90(&dark);
    let mut bytes = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(rotated)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    let scan = strict_scanner()
        .scan_image("bob", bytes.get_ref(), NOW)
        .unwrap();
    assert!(scan.stats.elapsed_ms < 10_000);

    let mut canvas = RgbImage::from_pixel(1_600, 1_600, Rgb([255, 255, 255]));
    image::imageops::overlay(&mut canvas, &source, 480, 480);
    let mut bytes = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(canvas)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    let scan = strict_scanner()
        .scan_image("bob", bytes.get_ref(), NOW)
        .unwrap();
    assert!(scan.stats.elapsed_ms < 10_000);
}

#[test]
fn empty_image_reports_not_found() {
    let blank = RgbImage::from_pixel(400, 400, Rgb([255, 255, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(blank)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    assert_eq!(
        strict_scanner()
            .scan_image("bob", bytes.get_ref(), NOW)
            .unwrap_err(),
        DiscoveryError::NotFound
    );
    assert_eq!(
        strict_scanner()
            .scan_image("bob", b"not-an-image", NOW)
            .unwrap_err(),
        DiscoveryError::InvalidArgument("image format")
    );
}

#[test]
fn text_codes_require_explicit_policy() {
    assert_eq!(
        strict_scanner()
            .scan_text("bob", "hello world", NOW)
            .unwrap_err(),
        DiscoveryError::InvalidArgument("unsupported scheme")
    );
    let policy = DeeplinkPolicy::with_hosts(Vec::new(), true).unwrap();
    let result = scanner(policy)
        .scan_text("bob", "hello world", NOW)
        .unwrap();
    assert!(!result.requires_confirmation);
    assert!(matches!(result.action, ScanAction::Text { .. }));
}

struct FakeCamera {
    calls: Mutex<Vec<String>>,
    frame: Vec<u8>,
    fail_capture: bool,
}

#[async_trait]
impl CameraPort for FakeCamera {
    async fn start(&self) -> Result<CameraSession, DiscoveryError> {
        self.calls.lock().unwrap().push("start".into());
        Ok(CameraSession {
            id: "cam1".into(),
            width: 640,
            height: 640,
        })
    }

    async fn set_flashlight(&self, enabled: bool) -> Result<(), DiscoveryError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("flashlight:{enabled}"));
        Ok(())
    }

    async fn capture_frame(&self) -> Result<Vec<u8>, DiscoveryError> {
        self.calls.lock().unwrap().push("capture".into());
        if self.fail_capture {
            return Err(DiscoveryError::DependencyUnavailable);
        }
        Ok(self.frame.clone())
    }

    async fn stop(&self) -> Result<(), DiscoveryError> {
        self.calls.lock().unwrap().push("stop".into());
        Ok(())
    }
}

#[tokio::test]
async fn camera_scan_actually_toggles_flashlight_and_stops() {
    let camera = FakeCamera {
        calls: Mutex::new(Vec::new()),
        frame: png_of(&friend_wire(Some(NOW + 60_000))),
        fail_capture: false,
    };
    let result = strict_scanner()
        .scan_camera(&camera, "bob", true, NOW)
        .await
        .unwrap();
    assert!(matches!(
        result.action,
        ScanAction::Signed(QrTarget::Friend { ref user_id }) if user_id == "alice"
    ));
    assert_eq!(
        *camera.calls.lock().unwrap(),
        vec!["start", "flashlight:true", "capture", "stop"]
    );
}

#[tokio::test]
async fn camera_failure_still_releases_camera() {
    let camera = FakeCamera {
        calls: Mutex::new(Vec::new()),
        frame: Vec::new(),
        fail_capture: true,
    };
    assert_eq!(
        strict_scanner()
            .scan_camera(&camera, "bob", false, NOW)
            .await
            .unwrap_err(),
        DiscoveryError::DependencyUnavailable
    );
    assert_eq!(
        *camera.calls.lock().unwrap(),
        vec!["start", "flashlight:false", "capture", "stop"]
    );
}
