//! SFU 出口码率分配（**参照 atm0s-media-server 重写**，非逐行翻译）。
//!
//! 来源：`refs/atm0s-media-server`（8xFF，MIT）
//! `packages/media_core/src/endpoint/internal/bitrate_allocator/egress.rs`。
//! 原实现挂在他们的 `LocalTrackId` / `TrackPriority` / `sans-io-runtime` 上，
//! 这里保留**算法本身**（按优先级权重比例分配、BWE 目标 6/5、无轨道时的兜底
//! 值），把类型换成我们自己的 `TrackId = u32` 与 `u64` 权重，去掉 `log` 与
//! `indexmap` 依赖（顺序用 `Vec` 保持插入序，与原实现一致）。
//!
//! 为什么需要它：我们的 `Room` 只有名册与管控，没有任何"观众能收多少码率"
//! 的策略。多方通话里 N 个观众各订阅 M 路视频，出口带宽必须按优先级分配，
//! 否则弱网观众会把整屋拖垮。这套分配器是 atm0s 在生产里用过的形状。
//!
//! 语义要点（与原实现一致）：
//! * 只在"有变化"时产出（`set_*` 置 dirty，`on_tick` 结算）；
//! * 可用带宽 = `min(估算带宽, 上限)`；
//! * 各轨按权重比例分，权重为 0 视为不分；
//! * 有轨道时 BWE 目标 = 当前 × 6/5（封顶上限），无轨道时用固定兜底值。

use std::collections::VecDeque;

/// 视频轨道标识（我们的编号，与原实现的 `LocalTrackId` 无关）。
pub type TrackId = u32;

/// 未收到估算值时的默认出口码率（原实现同名常量）。
pub const DEFAULT_BITRATE_BPS: u64 = 800_000;
/// 无轨道时的 BWE 当前值。
pub const NO_TRACK_BWE_CURRENT: u64 = 100_000;
/// 无轨道时的 BWE 目标值。
pub const NO_TRACK_BWE_DESIRED: u64 = 300_000;
/// 单个观众默认出口上限（2.5 Mbps，与原实现测试用的上限一致）。
pub const DEFAULT_MAX_EGRESS_BPS: u64 = 2_500_000;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Action {
    SetBitrate(u64),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Output {
    Track(TrackId, Action),
    BweConfig(u64, u64),
}

#[derive(Clone, Debug)]
pub struct EgressBitrateAllocator {
    max_egress_bitrate: u64,
    changed: bool,
    egress_bitrate: u64,
    /// 插入序 = 产出序（原实现用 `IndexMap`，这里用 `Vec` 保持同样语义）。
    tracks: Vec<(TrackId, u64)>,
    queue: VecDeque<Output>,
}

impl EgressBitrateAllocator {
    pub fn new(max_egress_bitrate: u64) -> Self {
        Self {
            max_egress_bitrate,
            changed: false,
            egress_bitrate: DEFAULT_BITRATE_BPS,
            tracks: Vec::new(),
            queue: VecDeque::new(),
        }
    }

    pub fn on_tick(&mut self) {
        self.process();
    }

    pub fn set_egress_estimate(&mut self, bitrate: u64) {
        self.egress_bitrate = bitrate;
        self.changed = true;
    }

    /// 新增/更新一条视频轨的优先级权重。已存在则原位更新（保持插入序）。
    pub fn set_video_track(&mut self, track: TrackId, priority: u64) {
        match self.tracks.iter_mut().find(|(id, _)| *id == track) {
            Some(slot) => slot.1 = priority,
            None => self.tracks.push((track, priority)),
        }
        self.changed = true;
    }

    pub fn del_video_track(&mut self, track: TrackId) {
        self.tracks.retain(|(id, _)| *id != track);
        self.changed = true;
    }

    pub fn pop_output(&mut self) -> Option<Output> {
        self.queue.pop_front()
    }

    fn process(&mut self) {
        if !self.changed {
            return;
        }
        self.changed = false;
        let use_bitrate = self.egress_bitrate.min(self.max_egress_bitrate);
        let sum: u64 = self.tracks.iter().map(|(_, p)| *p).sum();

        for (track, priority) in self.tracks.iter() {
            // 原实现：use_bitrate * priority / sum（整数比例，允许余数丢弃）。
            // 权重和为 0 时 checked_div 返回 None → 不分码率（与原实现一致）。
            if let Some(bitrate) = use_bitrate.saturating_mul(*priority).checked_div(sum) {
                self.queue
                    .push_back(Output::Track(*track, Action::SetBitrate(bitrate)));
            }
        }

        if !self.tracks.is_empty() {
            let current = use_bitrate;
            let desired = (use_bitrate * 6 / 5).min(self.max_egress_bitrate);
            self.queue.push_back(Output::BweConfig(current, desired));
        } else {
            self.queue.push_back(Output::BweConfig(
                NO_TRACK_BWE_CURRENT,
                NO_TRACK_BWE_DESIRED,
            ));
        }
    }
}
