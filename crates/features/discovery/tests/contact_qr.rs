use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use image::{Rgba, RgbaImage};
use wxlike_discovery::clock::ManualClock;
use wxlike_discovery::contact_qr::{
    contrast_ratio, CardStyle, ContactQrService, FriendRequestPort, MediaPort,
};
use wxlike_discovery::qr::{decode_strings, render_png};
use wxlike_discovery::DiscoveryError;

const NOW: i64 = 1_700_000_000_000;

fn service() -> (Arc<ManualClock>, ContactQrService<Arc<ManualClock>>) {
    let clock = Arc::new(ManualClock::new(NOW));
    let service = ContactQrService::new(clock.clone(), b"g07-card-key");
    (clock, service)
}

fn logo() -> RgbaImage {
    RgbaImage::from_fn(64, 64, |x, y| {
        if (x + y) % 2 == 0 {
            Rgba([20, 20, 20, 255])
        } else {
            Rgba([240, 240, 240, 255])
        }
    })
}

#[test]
fn four_styles_decode_with_and_without_logo() {
    let (_clock, service) = service();
    let card = service.create("alice", 60_000).unwrap();
    let mark = logo();
    for style in CardStyle::ALL {
        assert!(contrast_ratio(style.colors().0, style.colors().1) >= 4.5);
        for with_logo in [false, true] {
            let png = service
                .render(&card, style, with_logo.then_some(&mark))
                .unwrap();
            let texts = decode_strings(&png, 4_096).unwrap();
            assert_eq!(texts.len(), 1, "{} logo={with_logo}", style.label());
            let profile = service.resolve(&texts[0]).unwrap();
            assert_eq!(profile.user_id, "alice");
            assert_eq!(profile.card_id, card.card_id);
        }
    }
}

#[test]
fn saved_png_reopens_and_scans() {
    let (_clock, service) = service();
    let card = service.create("alice", 60_000).unwrap();
    let png = service.render(&card, CardStyle::WarmSun, None).unwrap();
    let path = std::env::temp_dir().join(format!("g07-card-{}.png", std::process::id()));
    std::fs::write(&path, &png).unwrap();
    let reopened = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let texts = decode_strings(&reopened, 4_096).unwrap();
    assert_eq!(service.resolve(&texts[0]).unwrap().user_id, "alice");
}

#[test]
fn rotation_and_revocation_invalidate_old_codes() {
    let (_clock, service) = service();
    let old = service.create("alice", 60_000).unwrap();
    let new = service.rotate("alice", 60_000).unwrap();
    assert_eq!(
        service.resolve(&old.wire).unwrap_err(),
        DiscoveryError::Expired
    );
    assert_eq!(service.resolve(&new.wire).unwrap().user_id, "alice");
    service.revoke_all("alice").unwrap();
    assert_eq!(
        service.resolve(&new.wire).unwrap_err(),
        DiscoveryError::Expired
    );
    assert_eq!(
        service.revoke_all("alice").unwrap_err(),
        DiscoveryError::NotFound
    );
}

/// 名片 wire 里 `p=` 段的明文 JSON。不透明性的断言都对着它说话。
fn payload_text(wire: &str) -> String {
    use base64::Engine as _;
    let payload = wire.split("p=").nth(1).unwrap().split('&').next().unwrap();
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .unwrap();
    String::from_utf8(json).unwrap()
}

/// 载荷里除了有效期，每个字段都应当是**不透明十六进制令牌**（允许 `card_`/`n_` 前缀，
/// 但前缀后面必须还是十六进制——不然 `n_` 就成了夹带明文的许可证）。
/// 这条比"扫一遍有没有某几个数字"有意义得多：它能挡住"字段里开始带明文身份"。
fn assert_opaque_token(field: &str, value: &str) {
    let body = value
        .strip_prefix("card_")
        .or_else(|| value.strip_prefix("n_"))
        .unwrap_or(value);
    assert!(
        !body.is_empty() && body.chars().all(|c| c.is_ascii_hexdigit()),
        "{field} 不是不透明十六进制令牌：{value}"
    );
}

#[test]
fn card_payload_is_opaque_and_tamper_evident() {
    let (_clock, service) = service();
    let card = service.create("alice", 60_000).unwrap();
    let text = payload_text(&card.wire);
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["c", "e", "k", "n"]);
    for (field, v) in value.as_object().unwrap() {
        if field != "e" {
            let s = v
                .as_str()
                .unwrap_or_else(|| panic!("{field} 应为不透明字符串，实际 {v}"));
            assert_opaque_token(field, s);
        }
    }
    // 探针只能放"只有真泄漏才会出现"的串。这里原来遍历的是
    // `["alice", "phone", "138"]`：`138` 会在随机出来的十六进制 id 里自己凑出来
    // （c/k/n 三段约 90 个三位窗口，每个命中率 1/4096 ⇒ 整条约 2% 无端变红），
    // 而这三个数字本身什么也证明不了。一条会随机红的**安全断言**是双向的坏：
    // 大家很快就开始把它当噪声重跑，下次真泄漏也没人看。
    // 真该防的是"主体可识别信息被带进载荷"，那就用一条长得像手机号的主体 id 来验
    // ——随机十六进制不会恰好等于它，这才是可判定的。
    assert!(!text.contains("alice"), "名片载荷里出现了主体 id：{text}");
    let phone_card = service.create("13800001111", 60_000).unwrap();
    let phone_text = payload_text(&phone_card.wire);
    assert!(
        !phone_text.contains("13800001111"),
        "载荷里带出了手机号样的主体 id：{phone_text}"
    );
    assert!(
        !phone_text.contains("phone"),
        "载荷里出现了字段名 phone：{phone_text}"
    );
    let tampered = format!("{}x", card.wire);
    assert_eq!(
        service.resolve(&tampered).unwrap_err(),
        DiscoveryError::InvalidArgument("card signature")
    );
}

#[test]
fn expired_card_fails() {
    let (clock, service) = service();
    let card = service.create("alice", 60_000).unwrap();
    clock.advance(60_001);
    assert_eq!(
        service.resolve(&card.wire).unwrap_err(),
        DiscoveryError::Expired
    );
}

struct FakeFriendRequests {
    calls: Mutex<Vec<(String, String, String)>>,
}

#[async_trait]
impl FriendRequestPort for FakeFriendRequests {
    async fn request_friend(
        &self,
        actor: &str,
        target_user_id: &str,
        card_id: &str,
    ) -> Result<String, DiscoveryError> {
        self.calls.lock().unwrap().push((
            actor.to_owned(),
            target_user_id.to_owned(),
            card_id.to_owned(),
        ));
        Ok("req_1".into())
    }
}

struct FakeMedia {
    saved: Mutex<Vec<usize>>,
    shared: Mutex<Vec<usize>>,
}

#[async_trait]
impl MediaPort for FakeMedia {
    async fn save_image(&self, bytes: &[u8], mime: &str) -> Result<String, DiscoveryError> {
        assert_eq!(mime, "image/png");
        self.saved.lock().unwrap().push(bytes.len());
        Ok("album://1".into())
    }

    async fn share_image(&self, bytes: &[u8], mime: &str) -> Result<(), DiscoveryError> {
        assert_eq!(mime, "image/png");
        self.shared.lock().unwrap().push(bytes.len());
        Ok(())
    }
}

#[tokio::test]
async fn friend_request_and_media_are_explicit_platform_calls() {
    let (_clock, service) = service();
    let card = service.create("alice", 60_000).unwrap();
    let profile = service.resolve(&card.wire).unwrap();
    let requests = FakeFriendRequests {
        calls: Mutex::new(Vec::new()),
    };
    assert_eq!(
        service
            .request_friend("bob", &requests, &profile)
            .await
            .unwrap(),
        "req_1"
    );
    assert_eq!(
        *requests.calls.lock().unwrap(),
        vec![("bob".into(), "alice".into(), card.card_id.clone())]
    );
    assert_eq!(
        service
            .request_friend("alice", &requests, &profile)
            .await
            .unwrap_err(),
        DiscoveryError::InvalidArgument("self friend request")
    );

    let png = service.render(&card, CardStyle::InkGreen, None).unwrap();
    let media = FakeMedia {
        saved: Mutex::new(Vec::new()),
        shared: Mutex::new(Vec::new()),
    };
    assert_eq!(
        service.save_to_album(&media, &png).await.unwrap(),
        "album://1"
    );
    service.share(&media, &png).await.unwrap();
    assert_eq!(media.saved.lock().unwrap().len(), 1);
    assert_eq!(media.shared.lock().unwrap().len(), 1);
}

#[test]
fn standard_qr_renderer_is_decodable_by_the_scanner() {
    let png = render_png("https://wxlike.com/invite").unwrap();
    assert_eq!(decode_strings(&png, 4_096).unwrap().len(), 1);
}
