//! G02-05 隐私策略：可见性、陌生人消息、已读回执、通知。
//!
//! 全是**纯函数**：输入是关系状态与偏好，输出是判定，不查库、不发 IO。
//! 这样每条策略都能单测，"为什么这个人看不到我的手机号"也能一眼追到规则。
//!
//! 三处口径必须和产品一致，写死在这里：
//!
//! * **拉黑优先于一切**：被拉黑的人连"资料是否可见"都拿不到（`Hidden`）；
//! * **陌生人只看到有限资料**：昵称/头像可见，手机号、账号 id 等可搜索字段不可见；
//! * **通知的硬关与静音是两件事**：`general.dnd=on` 不投递；免打扰时段只是不响。

use std::collections::BTreeMap;

/// 资料可见性。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    /// 全部字段可见（本人、好友）。
    Visible,
    /// 只有昵称/头像/签名（陌生人）。
    Limited,
    /// 什么都不给（被拉黑）。
    Hidden,
}

/// 观察者与目标的关系状态。由调用方从关系域取，不在这里猜。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewer {
    pub is_self: bool,
    pub is_friend: bool,
    /// 任一方向被拉黑。
    pub blocked: bool,
}

/// 可搜索维度。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKey {
    Phone,
    AccountId,
}

/// 隐私偏好（缺省值即产品默认行为）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrivacyPrefs {
    pub discover_by_phone: bool,
    pub discover_by_id: bool,
    pub allow_stranger_msg: bool,
    pub read_receipts: bool,
}

impl Default for PrivacyPrefs {
    fn default() -> Self {
        Self {
            discover_by_phone: false,
            discover_by_id: true,
            allow_stranger_msg: true,
            read_receipts: true,
        }
    }
}

/// 资料可见性判定。
pub fn profile_visibility(viewer: Viewer, prefs: PrivacyPrefs) -> Visibility {
    if viewer.blocked {
        return Visibility::Hidden;
    }
    if viewer.is_self || viewer.is_friend {
        return Visibility::Visible;
    }
    // 陌生人：即使开了"允许陌生人消息"，资料也只是有限可见。
    let _ = prefs;
    Visibility::Limited
}

/// 能不能被这个维度搜到（`is_self` 恒 true）。
pub fn searchable_by(prefs: PrivacyPrefs, key: SearchKey, is_self: bool) -> bool {
    if is_self {
        return true;
    }
    match key {
        SearchKey::Phone => prefs.discover_by_phone,
        SearchKey::AccountId => prefs.discover_by_id,
    }
}

/// 陌生人能否直接发消息（不能的话走"消息请求箱"，不是拒收）。
pub fn may_send_stranger(viewer: Viewer, prefs: PrivacyPrefs) -> bool {
    if viewer.blocked {
        return false;
    }
    if viewer.is_friend || viewer.is_self {
        return true;
    }
    prefs.allow_stranger_msg
}

/// 对方能否看到我的已读状态。
pub fn may_read_receipts(viewer: Viewer, prefs: PrivacyPrefs) -> bool {
    if viewer.blocked {
        return false;
    }
    if viewer.is_self {
        return true;
    }
    viewer.is_friend && prefs.read_receipts
}

/// 通知类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyKind {
    DirectMessage,
    GroupMessage,
    /// @我：即使关了群通知也要提醒，否则会漏事。
    Mention,
    Call,
}

/// 通知判定。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotifyDecision {
    pub deliver: bool,
    pub sound: bool,
    pub preview: bool,
}

impl NotifyDecision {
    const OFF: Self = Self {
        deliver: false,
        sound: false,
        preview: false,
    };
}

/// 偏好读取的最小接口（`wxlike_identity::preferences::ServerPrefs` 可直接适配）。
pub trait PrefSource {
    fn flag(&self, key: &str, default: bool) -> bool;
}

/// `BTreeMap<String, String>` 上的实现（值口径：`on`/`true` 为真）。
pub struct MapPrefs<'a>(pub &'a BTreeMap<String, String>);

impl PrefSource for MapPrefs<'_> {
    fn flag(&self, key: &str, default: bool) -> bool {
        match self.0.get(key).map(|v| v.as_str()) {
            Some("on") | Some("true") => true,
            Some("off") | Some("false") => false,
            _ => default,
        }
    }
}

fn enabled(prefs: &dyn PrefSource, kind: NotifyKind) -> bool {
    match kind {
        NotifyKind::DirectMessage => prefs.flag("notify.dm", true),
        NotifyKind::GroupMessage => prefs.flag("notify.group", true),
        NotifyKind::Mention => true,
        NotifyKind::Call => true,
    }
}

/// 通知判定：硬关 → 不投递；免打扰 → 投递但不响；预览单独开关。
pub fn notification(
    prefs: &dyn PrefSource,
    kind: NotifyKind,
    in_quiet_hours: bool,
) -> NotifyDecision {
    if prefs.flag("general.dnd", false) {
        return NotifyDecision::OFF;
    }
    if !enabled(prefs, kind) {
        return NotifyDecision::OFF;
    }
    NotifyDecision {
        deliver: true,
        sound: !in_quiet_hours,
        preview: prefs.flag("notify.preview", true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRANGER: Viewer = Viewer {
        is_self: false,
        is_friend: false,
        blocked: false,
    };
    const FRIEND: Viewer = Viewer {
        is_self: false,
        is_friend: true,
        blocked: false,
    };
    const BLOCKED: Viewer = Viewer {
        is_self: false,
        is_friend: false,
        blocked: true,
    };

    fn prefs_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn block_hides_everything() {
        let prefs = PrivacyPrefs::default();
        assert_eq!(profile_visibility(BLOCKED, prefs), Visibility::Hidden);
        assert!(!may_send_stranger(BLOCKED, prefs));
        assert!(!may_read_receipts(BLOCKED, prefs));
    }

    #[test]
    fn stranger_sees_limited_profile() {
        let prefs = PrivacyPrefs::default();
        assert_eq!(profile_visibility(STRANGER, prefs), Visibility::Limited);
        assert_eq!(profile_visibility(FRIEND, prefs), Visibility::Visible);
        assert_eq!(
            profile_visibility(
                Viewer {
                    is_self: true,
                    ..STRANGER
                },
                prefs
            ),
            Visibility::Visible
        );
    }

    #[test]
    fn searchable_dimensions_follow_prefs() {
        let prefs = PrivacyPrefs::default();
        assert!(
            !searchable_by(prefs, SearchKey::Phone, false),
            "手机号默认不可搜"
        );
        assert!(searchable_by(prefs, SearchKey::AccountId, false));
        assert!(
            searchable_by(prefs, SearchKey::Phone, true),
            "自己总能搜到自己"
        );
        let open = PrivacyPrefs {
            discover_by_phone: true,
            ..prefs
        };
        assert!(searchable_by(open, SearchKey::Phone, false));
    }

    #[test]
    fn stranger_message_follows_switch() {
        let open = PrivacyPrefs::default();
        assert!(may_send_stranger(STRANGER, open));
        let closed = PrivacyPrefs {
            allow_stranger_msg: false,
            ..open
        };
        assert!(!may_send_stranger(STRANGER, closed));
        assert!(may_send_stranger(FRIEND, closed), "好友不受陌生人开关影响");
    }

    #[test]
    fn read_receipts_are_friends_only() {
        let prefs = PrivacyPrefs::default();
        assert!(may_read_receipts(FRIEND, prefs));
        assert!(!may_read_receipts(STRANGER, prefs));
        let off = PrivacyPrefs {
            read_receipts: false,
            ..prefs
        };
        assert!(!may_read_receipts(FRIEND, off));
    }

    #[test]
    fn notifications_respect_hard_off_quiet_and_preview() {
        let map = prefs_map(&[("notify.dm", "off"), ("notify.preview", "off")]);
        let prefs = MapPrefs(&map);
        assert!(!notification(&prefs, NotifyKind::DirectMessage, false).deliver);
        assert!(
            notification(&prefs, NotifyKind::Mention, false).deliver,
            "@我 不能被普通开关关掉"
        );
        assert!(!notification(&prefs, NotifyKind::Mention, false).preview);

        let quiet = notification(&prefs, NotifyKind::Mention, true);
        assert!(quiet.deliver && !quiet.sound, "免打扰只是不响");

        let dnd = prefs_map(&[("general.dnd", "on")]);
        let hard = MapPrefs(&dnd);
        assert!(!notification(&hard, NotifyKind::Call, false).deliver);
    }

    #[test]
    fn defaults_deliver_with_sound_and_preview() {
        let empty = BTreeMap::new();
        let prefs = MapPrefs(&empty);
        let d = notification(&prefs, NotifyKind::GroupMessage, false);
        assert!(d.deliver && d.sound && d.preview);
    }
}
