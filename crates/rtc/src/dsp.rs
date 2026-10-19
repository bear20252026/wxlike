//! 纯 DSP 小件：重采样与分帧。
//!
//! 单独成模块是为了**能在 CI 里测**——它们才是音频链路上最容易错的部分
//! （采样率不匹配、跨缓冲的相位断裂、声道交错搞反），而真正碰声卡的代码
//! （`audio_io`）在无头环境里跑不起来。把易错的算术留在这里，无设备也能回归。
//!
//! 重采样用线性插值，不是高质量方案（语音够用、无依赖）。要上音乐级音质
//! 再换 sinc/polyphase，接口不变。

/// 通话统一采样率：Opus 只吃 48 kHz。
pub const RATE: u32 = 48_000;
/// 一帧 20 ms @ 48 kHz 的采样数。
pub const FRAME_SAMPLES: usize = 960;

/// 线性插值重采样器。**有状态**：跨调用保持相位与上一个样本，
/// 否则每个缓冲边界都会产生一次相位跳变（听感上是每 10ms 一声"咔"）。
pub struct Resampler {
    /// 每输出一个样本，在输入轴上前进多少。
    step: f64,
    /// 相对于"上一块末尾"的读指针，范围 `[0, 1)`。
    pos: f64,
    /// 上一块的最后一样本，用于跨缓冲插值。
    prev: i16,
    started: bool,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Self {
            step: from as f64 / to as f64,
            pos: 0.0,
            prev: 0,
            started: false,
        }
    }

    /// 同采样率时是恒等变换（省掉插值误差）。
    pub fn identity(&self) -> bool {
        (self.step - 1.0).abs() < f64::EPSILON
    }

    /// 处理一块输入，返回重采样后的输出。
    pub fn process(&mut self, input: &[i16]) -> Vec<i16> {
        if input.is_empty() {
            return Vec::new();
        }
        if !self.started {
            // 首块没有"上一个样本"，用首样本补齐，避免开头一个从 0 起跳的斜坡
            self.prev = input[0];
            self.started = true;
        }
        if self.identity() {
            self.prev = *input.last().unwrap();
            return input.to_vec();
        }

        // 虚拟轴：0 = prev，1..=n = input[0..n]
        let n = input.len();
        let at = |i: usize| -> i16 {
            if i == 0 {
                self.prev
            } else {
                input[i - 1]
            }
        };
        let mut out = Vec::with_capacity((n as f64 / self.step).ceil() as usize + 1);
        let mut pos = self.pos;
        while pos < n as f64 {
            let i = pos.floor() as usize;
            let frac = pos - i as f64;
            let a = f64::from(at(i));
            let b = f64::from(at(i + 1));
            out.push((a + (b - a) * frac).round() as i16);
            pos += self.step;
        }
        self.prev = input[n - 1];
        // 把读指针折算到下一块的坐标系（下一块的第 0 位是 input[n-1]）
        self.pos = pos - n as f64;
        out
    }
}

/// 把任意长度的样本流切成固定 20 ms 帧。
pub struct FrameChunker {
    buf: Vec<i16>,
}

impl FrameChunker {
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(FRAME_SAMPLES * 2),
        }
    }

    /// 追加样本，返回本次凑满的所有整帧（余数留在内部）。
    pub fn push(&mut self, pcm: &[i16]) -> Vec<Vec<i16>> {
        self.buf.extend_from_slice(pcm);
        let mut frames = Vec::new();
        while self.buf.len() >= FRAME_SAMPLES {
            frames.push(self.buf.drain(..FRAME_SAMPLES).collect());
        }
        frames
    }

    /// 当前攒了多少样本（诊断用）。
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}

impl Default for FrameChunker {
    fn default() -> Self {
        Self::new()
    }
}

/// 交错多声道 f32 → 单声道 i16（取平均）。
///
/// 声卡给的常是立体声，直接取左声道会丢掉右声道的说话声（很多人用单耳麦）。
pub fn interleaved_to_mono(samples: &[f32], channels: usize) -> Vec<i16> {
    if channels <= 1 {
        return samples
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
            .collect();
    }
    samples
        .chunks_exact(channels)
        .map(|frame| {
            let sum: f32 = frame.iter().sum();
            let avg = (sum / channels as f32).clamp(-1.0, 1.0);
            (avg * f32::from(i16::MAX)) as i16
        })
        .collect()
}

/// 单声道 i16 → 交错多声道 f32（写回声卡）。
pub fn mono_to_interleaved(pcm: &[i16], channels: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(pcm.len() * channels.max(1));
    for s in pcm {
        let v = f32::from(*s) / f32::from(i16::MAX);
        for _ in 0..channels.max(1) {
            out.push(v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_rate_is_identity() {
        let mut r = Resampler::new(48_000, 48_000);
        let input: Vec<i16> = (0..480).map(|i| (i * 37 % 1000) as i16).collect();
        assert_eq!(r.process(&input), input);
    }

    #[test]
    fn upsample_length_matches_ratio() {
        let mut r = Resampler::new(44_100, 48_000);
        let input: Vec<i16> = (0..4410).map(|i| ((i % 100) * 100) as i16).collect();
        let out = r.process(&input);
        let expect = 4410.0 * 48_000.0 / 44_100.0;
        assert!(
            (out.len() as f64 - expect).abs() <= 2.0,
            "got {} want ~{expect}",
            out.len()
        );
    }

    #[test]
    fn downsample_length_matches_ratio() {
        let mut r = Resampler::new(48_000, 44_100);
        let input: Vec<i16> = vec![1234; 4800];
        let out = r.process(&input);
        let expect = 4800.0 * 44_100.0 / 48_000.0;
        assert!(
            (out.len() as f64 - expect).abs() <= 2.0,
            "got {} want ~{expect}",
            out.len()
        );
    }

    #[test]
    fn dc_signal_survives_resampling() {
        let mut r = Resampler::new(44_100, 48_000);
        let out = r.process(&vec![1000i16; 4410]);
        assert!(out.iter().all(|s| (*s - 1000).abs() <= 1), "DC 不该变形");
    }

    #[test]
    fn chunk_boundaries_do_not_drop_samples() {
        // 同样的信号，分两次喂和一次喂，输出总长度必须一致（相位连续）
        let input: Vec<i16> = (0..4800).map(|i| (i % 200) as i16).collect();
        let mut whole = Resampler::new(44_100, 48_000);
        let one = whole.process(&input).len();

        let mut split = Resampler::new(44_100, 48_000);
        let mut two = split.process(&input[..1234]).len();
        two += split.process(&input[1234..]).len();
        assert!(
            (one as i64 - two as i64).abs() <= 1,
            "分块喂入丢了/多了样本: {one} vs {two}"
        );
    }

    #[test]
    fn chunker_emits_only_full_frames() {
        let mut c = FrameChunker::new();
        assert!(c.push(&vec![1i16; 2400]).len() == 2, "2400 样本 = 2 帧");
        assert_eq!(c.pending(), 2400 - 2 * FRAME_SAMPLES);
        let frames = c.push(&vec![1i16; FRAME_SAMPLES - c.pending()]);
        assert_eq!(frames.len(), 1);
        assert_eq!(c.pending(), 0);
        assert!(frames[0].len() == FRAME_SAMPLES);
    }

    #[test]
    fn stereo_is_averaged_not_left_only() {
        // 左声道静音、右声道满幅：取左声道会得到静音，取平均应是一半
        let stereo = [0.0f32, 1.0, 0.0, 1.0];
        let mono = interleaved_to_mono(&stereo, 2);
        assert_eq!(mono.len(), 2);
        let half = i16::MAX / 2;
        assert!((mono[0] - half).abs() <= 2, "got {}", mono[0]);
    }

    #[test]
    fn mono_to_stereo_duplicates() {
        let out = mono_to_interleaved(&[1000, -1000], 2);
        assert_eq!(out.len(), 4);
        assert!((out[0] - out[1]).abs() < f32::EPSILON);
    }
}
