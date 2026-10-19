//! 真实声卡采集与播放（`audio` feature）。
//!
//! 只在有音频设备的环境里可用，所以整个模块跟着 feature 走；纯算术在
//! [`crate::dsp`] 里，CI 无设备也能回归。
//!
//! 设计上只做一件事：**把声卡的任意格式变成 48 kHz 单声道 20 ms 帧**（采集），
//! 以及**把 48 kHz 单声道 PCM 写回任意格式的声卡**（播放）。编解码由
//! [`crate::capture::OpusCodec`] 负责，两者不互相知道。
//!
//! cpal 的流对象不是 `Sync`，而且**必须一直活着**才有声音——所以 `MicStream`
//! / `SpeakerSink` 都自己持有 `Stream`，drop 即停。

use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::dsp::{self, FrameChunker, Resampler, FRAME_SAMPLES, RATE};

/// 麦克风采集流：把 48 kHz 单声道 20 ms 帧推到 `tx`。
pub struct MicStream {
    _stream: cpal::Stream,
    rate: u32,
    channels: usize,
}

impl MicStream {
    /// 打开默认输入设备。`tx` 满（引擎没跟上）时丢帧——通话里丢旧帧比积压好。
    pub fn start(tx: Sender<Vec<i16>>) -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("没有可用的麦克风（默认输入设备）")?;
        let supported = device
            .default_input_config()
            .context("读取麦克风默认格式失败")?;
        let rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        let mut resampler = Resampler::new(rate, RATE);
        let mut chunker = FrameChunker::new();
        let err_fn = |e| eprintln!("[audio] 采集流出错: {e}");

        let stream = match format {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    let mono = dsp::interleaved_to_mono(data, channels);
                    let pcm = resampler.process(&mono);
                    for frame in chunker.push(&pcm) {
                        let _ = tx.send(frame);
                    }
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => {
                let mut resampler = Resampler::new(rate, RATE);
                let mut chunker = FrameChunker::new();
                device.build_input_stream(
                    &config,
                    move |data: &[i16], _| {
                        let mono = if channels <= 1 {
                            data.to_vec()
                        } else {
                            data.chunks_exact(channels)
                                .map(|f| {
                                    (f.iter().map(|s| i32::from(*s)).sum::<i32>() / channels as i32)
                                        as i16
                                })
                                .collect()
                        };
                        let pcm = resampler.process(&mono);
                        for frame in chunker.push(&pcm) {
                            let _ = tx.send(frame);
                        }
                    },
                    err_fn,
                    None,
                )
            }
            other => anyhow::bail!("不支持的麦克风采样格式: {other:?}"),
        }
        .context("打开麦克风流失败")?;
        stream.play().context("启动麦克风流失败")?;
        Ok(Self {
            _stream: stream,
            rate,
            channels,
        })
    }

    /// 实际使用的设备采样率（诊断用）。
    pub fn device_rate(&self) -> u32 {
        self.rate
    }

    pub fn device_channels(&self) -> usize {
        self.channels
    }
}

/// 扬声器播放：`push` 进 48 kHz 单声道 PCM，内部转成设备格式。
pub struct SpeakerSink {
    _stream: cpal::Stream,
    queue: Arc<Mutex<VecDeque<i16>>>,
    resampler: Resampler,
    rate: u32,
    channels: usize,
}

/// 播放队列上限：约 400 ms。超过就丢最旧的——音频永远"晚到不如丢掉"，
/// 越积越晚的队列最后会变成几秒钟的延迟。
const QUEUE_CAP: usize = (RATE as usize / 1000) * 400;

impl SpeakerSink {
    pub fn new() -> anyhow::Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("没有可用的扬声器（默认输出设备）")?;
        let supported = device
            .default_output_config()
            .context("读取扬声器默认格式失败")?;
        let rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        let queue: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));
        let q = queue.clone();
        let err_fn = |e| eprintln!("[audio] 播放流出错: {e}");

        let stream = match format {
            cpal::SampleFormat::F32 => {
                let mut out = Vec::<f32>::new();
                device.build_output_stream(
                    &config,
                    move |data: &mut [f32], _| {
                        let need = data.len();
                        out.clear();
                        let mut q = q.lock().unwrap();
                        while out.len() < need {
                            match q.pop_front() {
                                Some(s) => out.push(f32::from(s) / f32::from(i16::MAX)),
                                None => out.push(0.0), // 欠载补静音，不要卡住
                            }
                        }
                        data.copy_from_slice(&out);
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let mut out = Vec::<i16>::new();
                device.build_output_stream(
                    &config,
                    move |data: &mut [i16], _| {
                        let need = data.len();
                        out.clear();
                        let mut q = q.lock().unwrap();
                        while out.len() < need {
                            out.push(q.pop_front().unwrap_or(0));
                        }
                        data.copy_from_slice(&out);
                    },
                    err_fn,
                    None,
                )
            }
            other => anyhow::bail!("不支持的扬声器采样格式: {other:?}"),
        }
        .context("打开扬声器流失败")?;
        stream.play().context("启动扬声器流失败")?;

        Ok(Self {
            _stream: stream,
            queue,
            resampler: Resampler::new(RATE, rate),
            rate,
            channels,
        })
    }

    /// 推入一帧 48 kHz 单声道 PCM（任意长度）。内部重采样到设备采样率，
    /// 再按声道数展开成交错。
    pub fn push(&mut self, pcm: &[i16]) {
        let dev = self.resampler.process(pcm);
        let inter = dsp::mono_to_interleaved(&dev, self.channels);
        let mut q = self.queue.lock().unwrap();
        for v in inter {
            let s = (v.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16;
            if q.len() >= QUEUE_CAP {
                q.pop_front();
            }
            q.push_back(s);
        }
    }

    /// 队列里还剩多少样本（诊断用：长期增长说明时钟漂移）。
    pub fn queued(&self) -> usize {
        self.queue.lock().map(|q| q.len()).unwrap_or(0)
    }

    pub fn device_rate(&self) -> u32 {
        self.rate
    }

    pub fn device_channels(&self) -> usize {
        self.channels
    }

    /// 一帧 20 ms 的样本数，方便调用方按帧喂。
    pub fn frame_samples() -> usize {
        FRAME_SAMPLES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机自测：开麦克风与扬声器，采 1 秒直接播回去（本地回环）。
    ///
    /// 默认 `#[ignore]`：CI 与无头环境没有声卡，硬跑会失败。真机上执行：
    /// `cargo test -p wxlike-rtc --features audio -- --ignored --nocapture`
    /// 听到自己的声音 = 采集、重采样、分帧、播放四段都通了。
    #[test]
    #[ignore]
    fn mic_to_speaker_loopback() {
        let (tx, rx) = std::sync::mpsc::channel();
        let _mic = MicStream::start(tx).expect("打不开麦克风");
        let mut spk = SpeakerSink::new().expect("打不开扬声器");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut frames = 0usize;
        while std::time::Instant::now() < deadline {
            if let Ok(pcm) = rx.recv_timeout(std::time::Duration::from_millis(100)) {
                assert_eq!(pcm.len(), FRAME_SAMPLES, "每帧必须是 20ms");
                spk.push(&pcm);
                frames += 1;
            }
        }
        // 1 秒约 50 帧，留足余量给设备启动延迟
        assert!(frames >= 20, "1 秒内只收到 {frames} 帧，采集可能没起来");
        eprintln!("回环 {frames} 帧，队列剩余 {}", spk.queued());
    }
}
