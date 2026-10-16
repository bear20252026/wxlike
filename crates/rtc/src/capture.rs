//! Real capture/playback and codecs (feature-gated).
//!
//! These paths need hardware that CI and headless environments do not have,
//! so they live behind the `audio` (cpal + Opus) and `camera` (nokhwa)
//! features and are exercised on real machines — the demo commands in the
//! README (`rtc-audio-loopback`) drive them end to end.
//!
//! Payloads are opaque bytes at the media layer, so the codec here only has
//! to agree with itself: Opus 48 kHz mono, 20 ms frames (960 samples).

/// Opus frame duration in samples at 48 kHz (20 ms).
pub const FRAME_SAMPLES: usize = 960;
/// Opus frame duration in bytes of PCM input (i16 mono).
pub const FRAME_BYTES: usize = FRAME_SAMPLES * 2;

/// Encode/decode one Opus frame. A thin, testable wrapper: the encoder and
/// decoder are stateful and must be created as a pair per call leg.
pub struct OpusCodec {
    #[cfg(feature = "audio")]
    encoder: opus::Encoder,
    #[cfg(feature = "audio")]
    decoder: opus::Decoder,
    #[cfg(not(feature = "audio"))]
    _unused: (),
}

#[cfg(feature = "audio")]
impl OpusCodec {
    pub fn new() -> anyhow::Result<Self> {
        let encoder = opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Voip)?;
        let decoder = opus::Decoder::new(48_000, opus::Channels::Mono)?;
        Ok(Self { encoder, decoder })
    }

    /// PCM i16 mono frame -> Opus bytes.
    pub fn encode(&mut self, pcm: &[i16]) -> anyhow::Result<Vec<u8>> {
        let mut out = vec![0u8; 1024];
        let n = self.encoder.encode(pcm, &mut out)?;
        out.truncate(n);
        Ok(out)
    }

    /// Opus bytes -> PCM i16 mono frame.
    pub fn decode(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<i16>> {
        let mut pcm = vec![0i16; FRAME_SAMPLES];
        let n = self.decoder.decode(bytes, &mut pcm, false)?;
        pcm.truncate(n);
        Ok(pcm)
    }
}

#[cfg(feature = "audio")]
impl Default for OpusCodec {
    fn default() -> Self {
        Self::new().expect("opus codec init")
    }
}

#[cfg(not(feature = "audio"))]
impl OpusCodec {
    pub fn new() -> anyhow::Result<Self> {
        anyhow::bail!("built without the `audio` feature: enable it to use codecs")
    }
}

/// Lists audio output/input devices (diagnostics for the demo commands).
#[cfg(feature = "audio")]
pub fn audio_devices() -> Vec<String> {
    use cpal::traits::{DeviceTrait as _, HostTrait as _};
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Some(dev) = host.default_output_device() {
        names.push(format!("output: {}", dev.name().unwrap_or_default()));
    }
    if let Some(dev) = host.default_input_device() {
        names.push(format!("input: {}", dev.name().unwrap_or_default()));
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "audio")]
    #[test]
    fn opus_round_trip_preserves_energy() {
        let mut codec = OpusCodec::new().unwrap();
        // a 440 Hz-ish sine at reasonable amplitude
        let pcm: Vec<i16> = (0..FRAME_SAMPLES)
            .map(|i| (f32::sin(i as f32 * 0.058) * 8000.0) as i16)
            .collect();
        let bytes = codec.encode(&pcm).unwrap();
        assert!(
            bytes.len() < FRAME_BYTES,
            "opus compresses: {}",
            bytes.len()
        );
        let back = codec.decode(&bytes).unwrap();
        eprintln!(
            "encoded {} bytes, decoded {} samples",
            bytes.len(),
            back.len()
        );
        assert_eq!(back.len(), FRAME_SAMPLES);
        // compare average amplitude per sample, robust to codec gain control
        let amp_in: f32 = pcm.iter().map(|s| f32::from(*s).abs()).sum::<f32>() / pcm.len() as f32;
        let amp_out: f32 =
            back.iter().map(|s| f32::from(*s).abs()).sum::<f32>() / back.len() as f32;
        assert!(
            (amp_in - amp_out).abs() / amp_in < 0.35,
            "decoded signal keeps its shape: {amp_in} vs {amp_out}"
        );
    }

    #[cfg(not(feature = "audio"))]
    #[test]
    fn codec_reports_missing_feature() {
        assert!(OpusCodec::new().is_err());
    }
}
