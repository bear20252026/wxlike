//! 审核管线契约：变形绕过、分级、两种模式、多模态合并。

use std::sync::Arc;

use wxlike_moderation::{
    MediaJudge, MediaVerdict, Mode, Moderator, Outcome, Pipeline, Rule, Severity,
};

fn pipeline(mode: Mode) -> Pipeline {
    Pipeline::new(mode, Moderator::with_builtin_rules())
}

struct AlwaysReview;
impl MediaJudge for AlwaysReview {
    fn judge(&self, _mime: &str, _bytes: &[u8]) -> MediaVerdict {
        MediaVerdict::Review
    }
}

struct AlwaysBlock;
impl MediaJudge for AlwaysBlock {
    fn judge(&self, _mime: &str, _bytes: &[u8]) -> MediaVerdict {
        MediaVerdict::Block
    }
}

#[test]
fn clean_text_publishes_in_both_modes() {
    for mode in [Mode::PreReview, Mode::PostReview] {
        let d = pipeline(mode).judge_text("今晚一起吃饭吗？");
        assert_eq!(d.outcome, Outcome::Publish);
        assert!(d.labels.is_empty());
        assert_eq!(d.severity, None);
    }
}

#[test]
fn hard_violation_is_rejected_even_in_post_review() {
    // 先发后审也不放硬违规——"先发"只对软违规成立
    let d = pipeline(Mode::PostReview).judge_text("这里有暴恐内容");
    assert_eq!(d.outcome, Outcome::Reject);
    assert_eq!(d.severity, Some(Severity::Block));
    assert_eq!(d.labels, vec!["violence".to_string()]);
}

#[test]
fn spaced_and_zero_width_evasion_is_caught() {
    let p = pipeline(Mode::PreReview);
    for text in ["傻 逼", "傻\u{200b}逼", "傻-逼", "傻*逼"] {
        let d = p.judge_text(text);
        assert_eq!(d.outcome, Outcome::Hold, "{text}");
        assert_eq!(d.labels, vec!["abuse".to_string()], "{text}");
    }
}

#[test]
fn fullwidth_and_leet_evasion_is_caught() {
    let p = pipeline(Mode::PreReview);
    assert_eq!(p.judge_text("这里有ＰＯＲＮ").outcome, Outcome::Reject);
    assert_eq!(p.judge_text("p0rn video").outcome, Outcome::Reject);
    // 全角中文 + 分隔符
    assert_eq!(p.judge_text("裸 聊").outcome, Outcome::Reject);
}

#[test]
fn soft_violation_holds_in_pre_review_and_flags_in_post_review() {
    let pre = pipeline(Mode::PreReview).judge_text("加我微信 带你刷单");
    assert_eq!(pre.outcome, Outcome::Hold);
    assert_eq!(pre.severity, Some(Severity::Review));

    let post = pipeline(Mode::PostReview).judge_text("加我微信 带你刷单");
    assert_eq!(post.outcome, Outcome::PublishFlagged);
    assert!(post.is_publishable());
}

#[test]
fn mode_switch_applies_to_later_judgements() {
    let mut p = pipeline(Mode::PreReview);
    assert_eq!(p.judge_text("傻逼").outcome, Outcome::Hold);
    p.set_mode(Mode::PostReview);
    assert_eq!(p.judge_text("傻逼").outcome, Outcome::PublishFlagged);
}

#[test]
fn media_hook_downgrades_clean_text_to_review() {
    let p = pipeline(Mode::PreReview).with_media(Arc::new(AlwaysReview));
    let d = p.judge("正常文字", Some(("image/png", b"fake")));
    assert_eq!(d.outcome, Outcome::Hold);
    assert!(d.labels.contains(&"media_review".to_string()));
}

#[test]
fn media_block_overrides_text_and_rejects() {
    let p = pipeline(Mode::PostReview).with_media(Arc::new(AlwaysBlock));
    let d = p.judge("正常文字", Some(("image/jpeg", b"fake")));
    assert_eq!(d.outcome, Outcome::Reject);
    assert_eq!(d.severity, Some(Severity::Block));
}

#[test]
fn unscanned_media_is_not_assumed_clean() {
    // 没有多模态能力时不假装干净：带媒体 → 至少进复核
    let p = pipeline(Mode::PreReview);
    let d = p.judge("正常文字", Some(("video/mp4", b"fake")));
    assert_eq!(d.outcome, Outcome::Hold);
    assert!(d.labels.contains(&"media_unscanned".to_string()));
}

#[test]
fn labels_are_stable_and_deduplicated() {
    let p = Pipeline::new(
        Mode::PreReview,
        Moderator::new(vec![
            Rule::new("bad", "b", Severity::Review),
            Rule::new("ba", "a", Severity::Review),
        ]),
    );
    let d1 = p.judge_text("bad bad bad");
    let d2 = p.judge_text("bad");
    assert_eq!(d1.labels, d2.labels);
    assert_eq!(d1.labels, vec!["a".to_string(), "b".to_string()]);
}
