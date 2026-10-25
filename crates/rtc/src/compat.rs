//! G09-01 唯一 RTC 引擎门禁与客户端兼容矩阵。
//!
//! 全平台只有一套房间事实：`str0m` 媒体栈（[`crate::media`]）加本仓库的
//! SFU。UI 组不得各自接不同的 RTC 服务；LiveKit 仅作为候选比较，换引擎
//! 必须走全局 ADR。本模块把这个约束变成可执行的门禁：
//!
//! * [`approved_engine`] —— 配置里出现第二个引擎就直接失败；
//! * [`support`] —— 每类客户端 × 每项能力的明确结论（原生 / 需原生桥 / 阻塞）；
//! * [`require`] —— UI 接线前的强制检查，`Blocked`/`NeedsBridge` 不许被当成可用；
//! * [`budget`] / [`power_budget`] —— 带宽与耗电预算，按质量档位给出上限。
//!
//! 这里刻意不把"浏览器能跑"当成"iOS 也能跑"：iOS 音频路由、后台来电、
//! 移动端屏幕共享都需要 G10/G12 的原生桥，矩阵里写清楚，UI 才不会误报。
//! 真实四端设备实测属于交付清单里的待验项（见
//! `crates/rtc/docs/engine-decision.md`）。

use serde::Serialize;

/// 唯一获批的引擎名。配置项只接受这一个值。
pub const ENGINE: &str = "str0m";

/// 已知的 RTC 引擎。`LiveKit` 只在 ADR 比较里存在，不是可选项。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Str0m,
    LiveKit,
}

impl Engine {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "str0m" => Some(Self::Str0m),
            "livekit" => Some(Self::LiveKit),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Str0m => ENGINE,
            Self::LiveKit => "livekit",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EngineError {
    #[error("unknown RTC engine `{0}`")]
    Unknown(String),
    #[error("`{0}` is a comparison candidate only; switching engines needs a global ADR")]
    NeedsGlobalAdr(&'static str),
}

/// 所有客户端必须经过的门禁。`None` 表示使用默认引擎。
pub fn approved_engine(configured: Option<&str>) -> Result<Engine, EngineError> {
    let raw = configured.unwrap_or(ENGINE);
    let engine = Engine::parse(raw).ok_or_else(|| EngineError::Unknown(raw.to_string()))?;
    match engine {
        Engine::Str0m => Ok(engine),
        Engine::LiveKit => Err(EngineError::NeedsGlobalAdr(engine.name())),
    }
}

/// 需要接线的客户端类别。验收四类 = 桌面浏览器 / Android / iOS / Flutter。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientClass {
    DesktopBrowser,
    AndroidNative,
    IosNative,
    FlutterMobile,
    FlutterDesktop,
    CliHarness,
}

impl ClientClass {
    /// 四类真实验收端（两两语音/视频）。
    pub const ACCEPTANCE: [Self; 4] = [
        Self::DesktopBrowser,
        Self::AndroidNative,
        Self::IosNative,
        Self::FlutterMobile,
    ];

    /// 矩阵覆盖的全部端（含无设备的 CI 探针）。
    pub const ALL: [Self; 6] = [
        Self::DesktopBrowser,
        Self::AndroidNative,
        Self::IosNative,
        Self::FlutterMobile,
        Self::FlutterDesktop,
        Self::CliHarness,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::DesktopBrowser => "desktop_browser",
            Self::AndroidNative => "android_native",
            Self::IosNative => "ios_native",
            Self::FlutterMobile => "flutter_mobile",
            Self::FlutterDesktop => "flutter_desktop",
            Self::CliHarness => "cli_harness",
        }
    }
}

/// 需要逐项给结论的能力。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Voice,
    Video,
    ScreenShare,
    /// 听筒/扬声器/蓝牙路由（iOS 的 AVAudioSession 不能由 Rust 直接管）。
    AudioRoute,
    TurnRelay,
    E2ee,
    /// 来电时 App 在后台也能响铃。
    BackgroundIncoming,
    LiveWatch,
    LivePublish,
}

impl Capability {
    pub const ALL: [Self; 9] = [
        Self::Voice,
        Self::Video,
        Self::ScreenShare,
        Self::AudioRoute,
        Self::TurnRelay,
        Self::E2ee,
        Self::BackgroundIncoming,
        Self::LiveWatch,
        Self::LivePublish,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::Video => "video",
            Self::ScreenShare => "screen_share",
            Self::AudioRoute => "audio_route",
            Self::TurnRelay => "turn_relay",
            Self::E2ee => "e2ee",
            Self::BackgroundIncoming => "background_incoming",
            Self::LiveWatch => "live_watch",
            Self::LivePublish => "live_publish",
        }
    }
}

/// 兼容矩阵的单元格。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// 本仓库直接实现，不需要别的组。
    Native,
    /// 能力存在，但必须由指定组的原生桥提供。
    Bridge(&'static str),
    /// 该端不可行；`reason` 是上报阻塞的理由，不是"以后再说"。
    Blocked(&'static str),
}

impl Support {
    /// 只有原生实现才算"现在就可用"；桥接能力在桥合入前不算。
    pub fn ready(self) -> bool {
        matches!(self, Self::Native)
    }

    pub fn usable(self) -> bool {
        !matches!(self, Self::Blocked(_))
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Bridge(_) => "bridge",
            Self::Blocked(_) => "blocked",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompatError {
    #[error("{} cannot use {}: {}", client, capability, reason)]
    Blocked {
        client: &'static str,
        capability: &'static str,
        reason: &'static str,
    },
    #[error("{} needs the {} bridge for {}", client, group, capability)]
    NeedsBridge {
        client: &'static str,
        capability: &'static str,
        group: &'static str,
    },
}

/// 单格结论。规则集中在一处，测试与文档都从这里取。
pub fn support(client: ClientClass, capability: Capability) -> Support {
    use Capability as C;
    use ClientClass as K;
    match (client, capability) {
        // CI 探针没有声卡/摄像头：协议层可测，真实采集不可行。
        (K::CliHarness, C::Voice) => Support::Bridge("G16 headless harness (no sound card)"),
        (K::CliHarness, C::Video | C::ScreenShare) => {
            Support::Blocked("headless CI has no camera or display capture")
        }
        (K::CliHarness, C::AudioRoute) => Support::Blocked("no audio route in CI"),
        (K::CliHarness, C::BackgroundIncoming) => Support::Blocked("no mobile lifecycle in CI"),
        (K::IosNative, C::BackgroundIncoming) => Support::Bridge("G12 CallKit/PushKit"),
        (K::IosNative, C::AudioRoute) => Support::Bridge("G12 AVAudioSession"),
        (K::AndroidNative, C::BackgroundIncoming) => Support::Bridge("G10 FGS + FCM"),
        (K::AndroidNative, C::AudioRoute) => Support::Bridge("G10 AudioManager"),
        (K::FlutterMobile, C::BackgroundIncoming) => Support::Bridge("G12 iOS / G10 Android"),
        (K::FlutterMobile | K::FlutterDesktop, C::ScreenShare) => {
            Support::Bridge("G12 screen-capture plugin")
        }
        (K::FlutterMobile, C::AudioRoute) => Support::Bridge("G12 audio-session plugin"),
        _ => Support::Native,
    }
}

/// UI 接线前的强制检查：阻塞要上报，桥接要等桥，不许静默降级成图标。
pub fn require(client: ClientClass, capability: Capability) -> Result<Support, CompatError> {
    match support(client, capability) {
        Support::Blocked(reason) => Err(CompatError::Blocked {
            client: client.name(),
            capability: capability.name(),
            reason,
        }),
        Support::Bridge(group) => Err(CompatError::NeedsBridge {
            client: client.name(),
            capability: capability.name(),
            group,
        }),
        native => Ok(native),
    }
}

/// 质量档位。数字是**发送预算**，不是画质承诺。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    AudioOnly,
    Video360p,
    Video720p,
    Screen1080p,
}

impl Quality {
    pub fn label(self) -> &'static str {
        match self {
            Self::AudioOnly => "audio_only",
            Self::Video360p => "video_360p",
            Self::Video720p => "video_720p",
            Self::Screen1080p => "screen_1080p",
        }
    }
}

/// 一路通话的上行预算（kbps）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct MediaBudget {
    pub audio_kbps: u16,
    pub video_kbps: u16,
    pub total_kbps: u16,
}

/// 音频恒为 Opus 单声道 40 kbps。
pub const AUDIO_KBPS: u16 = 40;
pub const VIDEO_360P_KBPS: u16 = 600;
pub const VIDEO_720P_KBPS: u16 = 1_500;
pub const SCREEN_1080P_KBPS: u16 = 3_000;

/// 产品预算：视频按档位；屏幕共享给更高码率。
pub fn budget(quality: Quality) -> MediaBudget {
    let video = match quality {
        Quality::AudioOnly => 0,
        Quality::Video360p => VIDEO_360P_KBPS,
        Quality::Video720p => VIDEO_720P_KBPS,
        Quality::Screen1080p => SCREEN_1080P_KBPS,
    };
    MediaBudget {
        audio_kbps: AUDIO_KBPS,
        video_kbps: video,
        total_kbps: AUDIO_KBPS + video,
    }
}

/// 耗电预算（mAh/小时）。按 4000 mAh 手机估算，只用于产品验收，
/// 不是芯片功耗模型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PowerBudget {
    pub client: ClientClass,
    pub quality: Quality,
    pub mah_per_hour: u16,
}

/// 按端类型的基础功耗（屏幕亮起之外的部分）。
const POWER_BASE_MOBILE_MAH: u16 = 120;
const POWER_BASE_DESKTOP_MAH: u16 = 60;
/// 屏幕与编解码带来的额外功耗。
const POWER_SCREEN_AUDIO_MAH: u16 = 20;
const POWER_SCREEN_360P_MAH: u16 = 90;
const POWER_SCREEN_720P_MAH: u16 = 220;
const POWER_SCREEN_1080P_MAH: u16 = 380;

pub fn power_budget(client: ClientClass, quality: Quality) -> PowerBudget {
    let base = match client {
        ClientClass::AndroidNative | ClientClass::IosNative | ClientClass::FlutterMobile => {
            POWER_BASE_MOBILE_MAH
        }
        ClientClass::DesktopBrowser | ClientClass::FlutterDesktop => POWER_BASE_DESKTOP_MAH,
        ClientClass::CliHarness => 0,
    };
    let screen = match quality {
        Quality::AudioOnly => POWER_SCREEN_AUDIO_MAH,
        Quality::Video360p => POWER_SCREEN_360P_MAH,
        Quality::Video720p => POWER_SCREEN_720P_MAH,
        Quality::Screen1080p => POWER_SCREEN_1080P_MAH,
    };
    PowerBudget {
        client,
        quality,
        mah_per_hour: base + screen,
    }
}

/// 文档/诊断用矩阵行。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MatrixRow {
    pub client: ClientClass,
    pub capability: Capability,
    pub support: Support,
}

impl Serialize for Support {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        let mut st = s.serialize_struct("Support", 2)?;
        st.serialize_field("kind", self.label())?;
        match self {
            Support::Native => st.serialize_field("detail", "")?,
            Support::Bridge(g) | Support::Blocked(g) => st.serialize_field("detail", g)?,
        }
        st.end()
    }
}

/// 全矩阵（6 端 × 9 能力）。
pub fn matrix() -> Vec<MatrixRow> {
    ClientClass::ALL
        .into_iter()
        .flat_map(|client| {
            Capability::ALL
                .into_iter()
                .map(move |capability| MatrixRow {
                    client,
                    capability,
                    support: support(client, capability),
                })
        })
        .collect()
}

/// 四类验收端的两两组合（6 对，顺序无关）。
pub fn acceptance_pairs() -> Vec<(ClientClass, ClientClass)> {
    let mut pairs = Vec::new();
    for (i, a) in ClientClass::ACCEPTANCE.iter().enumerate() {
        for b in &ClientClass::ACCEPTANCE[i + 1..] {
            pairs.push((*a, *b));
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_str0m_is_approved() {
        assert_eq!(approved_engine(None), Ok(Engine::Str0m));
        assert_eq!(approved_engine(Some("STR0M")), Ok(Engine::Str0m));
        assert_eq!(
            approved_engine(Some("livekit")),
            Err(EngineError::NeedsGlobalAdr("livekit"))
        );
        assert!(matches!(
            approved_engine(Some("janus")),
            Err(EngineError::Unknown(_))
        ));
    }

    #[test]
    fn every_acceptance_client_has_voice_and_video() {
        for client in ClientClass::ACCEPTANCE {
            for cap in [Capability::Voice, Capability::Video, Capability::TurnRelay] {
                let s = support(client, cap);
                assert!(s.usable(), "{client:?} {cap:?} = {s:?}");
            }
        }
        assert_eq!(acceptance_pairs().len(), 6);
    }

    #[test]
    fn blocked_cells_are_not_silently_usable() {
        assert!(matches!(
            require(ClientClass::CliHarness, Capability::Video),
            Err(CompatError::Blocked { .. })
        ));
        assert!(matches!(
            require(ClientClass::IosNative, Capability::BackgroundIncoming),
            Err(CompatError::NeedsBridge {
                group: "G12 CallKit/PushKit",
                ..
            })
        ));
        assert_eq!(
            require(ClientClass::DesktopBrowser, Capability::Voice),
            Ok(Support::Native)
        );
    }

    #[test]
    fn matrix_covers_every_cell_once() {
        let rows = matrix();
        assert_eq!(rows.len(), ClientClass::ALL.len() * Capability::ALL.len());
        for row in rows {
            assert_eq!(row.support, support(row.client, row.capability));
        }
    }

    #[test]
    fn budgets_grow_with_quality_and_stay_sane() {
        let tiers = [
            Quality::AudioOnly,
            Quality::Video360p,
            Quality::Video720p,
            Quality::Screen1080p,
        ];
        let mut last = 0;
        for q in tiers {
            let b = budget(q);
            assert!(b.total_kbps >= last);
            assert_eq!(b.total_kbps, b.audio_kbps + b.video_kbps);
            last = b.total_kbps;
        }
        assert_eq!(budget(Quality::AudioOnly).video_kbps, 0);
        assert!(budget(Quality::Screen1080p).total_kbps <= 4_000);
    }

    #[test]
    fn mobile_power_budget_exceeds_desktop() {
        let mobile = power_budget(ClientClass::IosNative, Quality::Video720p);
        let desktop = power_budget(ClientClass::DesktopBrowser, Quality::Video720p);
        assert!(mobile.mah_per_hour > desktop.mah_per_hour);
        assert_eq!(
            power_budget(ClientClass::CliHarness, Quality::AudioOnly).mah_per_hour,
            20
        );
    }
}
