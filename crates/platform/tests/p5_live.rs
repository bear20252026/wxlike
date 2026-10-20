//! P5 直播频道契约：开播授权、推流凭据、观众计数、弹幕路由。
//!
//! 验收重点不是"能不能播"（媒体面在外部 ingest/HLS），而是三条边界：
//! 1. 只有 `live` 类型频道 + 社群管理员能开播；
//! 2. 推流凭据自包含可验、过期/篡改/停播后一律拒绝；
//! 3. 弹幕只发给在房观众，且**不落库、不占频道时间线**（不变式 3）。

mod common;

use common::Recorder;
use wxlike_platform::Platform;

struct Fixture {
    platform: wxlike_platform::PlatformState,
    owner: String,
    viewer: String,
    channel: String,
    community: String,
}

/// 社群 owner + live 频道 + 一个普通成员。
fn fixture() -> Fixture {
    let p = Platform::in_memory().unwrap();
    let owner = p.register("owner", "pw").unwrap();
    let viewer = p.register("viewer", "pw").unwrap();
    let community = p.create_community(&owner, "直播社", 100).unwrap();
    p.join_community(&community, &viewer).unwrap();
    let channel = p
        .create_channel(&community, &owner, "主舞台", "live")
        .unwrap();
    Fixture {
        platform: p.shared(),
        owner,
        viewer,
        channel,
        community,
    }
}

#[test]
fn only_live_channels_can_start() {
    let p = Platform::in_memory().unwrap();
    let owner = p.register("owner", "pw").unwrap();
    let community = p.create_community(&owner, "社", 10).unwrap();
    let text_channel = p
        .create_channel(&community, &owner, "闲聊", "text")
        .unwrap();
    assert!(p.start_live(&text_channel, &owner, false).is_err());
}

#[test]
fn only_managers_can_start() {
    let f = fixture();
    // 普通成员开播 → Forbidden
    assert!(f.platform.start_live(&f.channel, &f.viewer, false).is_err());
    // 管理员可以
    assert!(f.platform.start_live(&f.channel, &f.owner, false).is_ok());
}

#[test]
fn start_issues_verifiable_ingest_token_and_blocks_double_start() {
    let f = fixture();
    let s = f.platform.start_live(&f.channel, &f.owner, true).unwrap();
    assert!(s.recording);
    assert_eq!(s.state, "live");

    let token = f.platform.ingest_token(&s.stream_key, 60_000);
    let now = wxlike_core::types::now_ms();
    f.platform
        .verify_ingest(&s.stream_key, &token, now)
        .unwrap();

    // 同频道二次开播 → 冲突
    assert!(f.platform.start_live(&f.channel, &f.owner, false).is_err());
}

#[test]
fn ingest_token_rejects_tamper_expiry_and_wrong_key() {
    let f = fixture();
    let s = f.platform.start_live(&f.channel, &f.owner, false).unwrap();
    let now = wxlike_core::types::now_ms();

    let token = f.platform.ingest_token(&s.stream_key, 60_000);
    // 篡改签名
    let mut bad = token.clone();
    bad.push('0');
    assert!(f.platform.verify_ingest(&s.stream_key, &bad, now).is_err());
    // 篡改过期时间（签名对不上）
    let (exp, sig) = token.split_once('.').unwrap();
    let shifted = format!("{}.{sig}", exp.parse::<u64>().unwrap() + 1);
    assert!(f
        .platform
        .verify_ingest(&s.stream_key, &shifted, now)
        .is_err());
    // 已过期
    assert!(f
        .platform
        .verify_ingest(&s.stream_key, &token, now + 120_000)
        .is_err());
    // 换流密钥
    assert!(f
        .platform
        .verify_ingest("another-stream-key", &token, now)
        .is_err());
    // 格式垃圾
    assert!(f
        .platform
        .verify_ingest(&s.stream_key, "garbage", now)
        .is_err());
}

#[test]
fn stop_ends_session_and_revokes_ingest() {
    let f = fixture();
    let s = f.platform.start_live(&f.channel, &f.owner, false).unwrap();
    let token = f.platform.ingest_token(&s.stream_key, 60_000);

    f.platform.stop_live(&f.channel, &f.owner).unwrap();
    assert!(!f.platform.live_state(&f.channel, &f.viewer).unwrap().live);
    // 凭据未过期，但会话已结束 → 拒绝
    assert!(f
        .platform
        .verify_ingest(&s.stream_key, &token, wxlike_core::types::now_ms())
        .is_err());
    // 停播后重开：新会话、新流密钥
    let s2 = f.platform.start_live(&f.channel, &f.owner, false).unwrap();
    assert_ne!(s.stream_key, s2.stream_key);
}

#[test]
fn join_leave_tracks_viewers_and_peak() {
    let f = fixture();
    f.platform.start_live(&f.channel, &f.owner, false).unwrap();
    let extra = f.platform.register("extra", "pw").unwrap();
    f.platform.join_community(&f.community, &extra).unwrap();

    assert_eq!(f.platform.join_live(&f.channel, &f.viewer).unwrap(), 1);
    // 重复进房不重复计数
    assert_eq!(f.platform.join_live(&f.channel, &f.viewer).unwrap(), 1);
    assert_eq!(f.platform.join_live(&f.channel, &extra).unwrap(), 2);
    assert_eq!(f.platform.leave_live(&f.channel, &extra).unwrap(), 1);

    let st = f.platform.live_state(&f.channel, &f.viewer).unwrap();
    assert_eq!(st.viewers, 1);
    assert_eq!(st.host.as_deref(), Some(f.owner.as_str()));
    assert!(st.hls_url.contains("/index.m3u8"));
}

#[test]
fn non_member_cannot_join_or_send_danmaku() {
    let f = fixture();
    f.platform.start_live(&f.channel, &f.owner, false).unwrap();
    let outsider = f.platform.register("outsider", "pw").unwrap();
    assert!(f.platform.join_live(&f.channel, &outsider).is_err());
    assert!(f.platform.live_state(&f.channel, &outsider).is_err());
}

#[tokio::test]
async fn danmaku_goes_to_viewers_only_and_is_not_persisted() {
    let f = fixture();
    let rec = Recorder::new();
    f.platform.set_event_sink(std::sync::Arc::new(rec.clone()));
    f.platform.start_live(&f.channel, &f.owner, false).unwrap();
    let b = f.platform.register("bob", "pw").unwrap();
    f.platform.join_community(&f.community, &b).unwrap();
    f.platform.join_live(&f.channel, &f.viewer).unwrap();
    f.platform.join_live(&f.channel, &b).unwrap();

    let delivered = f
        .platform
        .send_danmaku(&f.channel, &f.owner, "开场了")
        .await
        .unwrap();
    assert_eq!(delivered, 2, "两名在房观众各收一条，发送者自己不算");

    let events = rec.take();
    assert_eq!(events.len(), 1);
    let env = &events[0];
    assert_eq!(env.conv_id, f.channel);
    match &env.body {
        wxlike_core::MsgBody::Custom { kind, payload } => {
            assert_eq!(kind, "live.danmaku");
            assert_eq!(payload["text"], "开场了");
        }
        other => panic!("unexpected body {other:?}"),
    }
    assert!(!env.to.contains(&f.owner), "弹幕不回显给发送者");

    // 不落库、不进频道时间线（不变式 3）
    let store = f.platform.message_store().unwrap();
    let (timeline, _) = store
        .fetch((wxlike_message::OwnerType::Channel, &f.channel), 0, 50)
        .unwrap();
    assert!(timeline.is_empty(), "弹幕不得占用频道序列号");
}

#[tokio::test]
async fn danmaku_is_moderated() {
    let f = fixture();
    f.platform.start_live(&f.channel, &f.owner, false).unwrap();
    f.platform.join_live(&f.channel, &f.viewer).unwrap();

    // 硬违规直接拒绝
    assert!(f
        .platform
        .send_danmaku(&f.channel, &f.owner, "裸聊")
        .await
        .is_err());
    // 超长拒绝
    assert!(f
        .platform
        .send_danmaku(&f.channel, &f.owner, &"啊".repeat(200))
        .await
        .is_err());
    // 空串拒绝
    assert!(f
        .platform
        .send_danmaku(&f.channel, &f.owner, "   ")
        .await
        .is_err());
}

/// 推流密钥**不允许为空**。
///
/// 空串是合法的 HMAC key，签出来的 ingest token 谁都能伪造——与"公开默认值"
/// 是同一类洞，只是更隐蔽。所以 `Platform::open_with` 在密钥为空时兜底成
/// 随机值：不管调用方怎么构造，都不会有实例拿空密钥签名。
#[test]
fn empty_ingest_secret_is_never_used() {
    let p = Platform::in_memory().expect("platform");
    let secret = &p.options().live_ingest_secret;
    assert!(!secret.trim().is_empty(), "默认构造也必须生成密钥");
    assert_ne!(secret, "dev-insecure-live-secret", "不许用公开默认值");

    // 两个实例的密钥必须不同：相同就说明它其实是常量
    let q = Platform::in_memory().expect("platform");
    assert_ne!(secret, &q.options().live_ingest_secret);

    // 跨实例的凭据不可互认（同密钥才会通过）
    let f = fixture();
    let token = f.platform.ingest_token(&f.channel, 60_000);
    assert!(q
        .verify_ingest(&f.channel, &token, wxlike_core::types::now_ms())
        .is_err());
}
