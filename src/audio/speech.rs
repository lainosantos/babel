//! Speech-only conversion. Never run this DSP on the original audio executor.
use super::{OriginalFrame, PcmFrame, resample::Resampler};
use std::time::{Duration, Instant};

pub struct SpeechTap {
    resampler: Option<Resampler>,
    format: Option<(u32, u16)>,
    previous: Option<Instant>,
    mono: Vec<f32>,
    converted: Vec<f32>,
}

impl Default for SpeechTap {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeechTap {
    pub fn new() -> Self {
        Self {
            resampler: None,
            format: None,
            previous: None,
            mono: Vec::new(),
            converted: Vec::new(),
        }
    }

    pub fn convert(&mut self, original: &OriginalFrame) -> PcmFrame {
        let channels = usize::from(original.channels.max(1));
        let format = (original.sample_rate, original.channels);
        let duration = Duration::from_secs_f64(
            original.samples.len() as f64 / channels as f64 / f64::from(original.sample_rate),
        );
        let discontinuity = self.previous.is_some_and(|previous| {
            original.captured_at.saturating_duration_since(previous)
                > duration + Duration::from_millis(50)
        });
        if self.format != Some(format) || discontinuity {
            self.resampler = Some(Resampler::new(original.sample_rate, 16_000));
            self.format = Some(format);
        }
        self.previous = Some(original.captured_at);
        self.mono.clear();
        self.mono.extend(
            original
                .samples
                .chunks_exact(channels)
                .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32),
        );
        self.converted.clear();
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.process(&self.mono, &mut self.converted);
        }
        PcmFrame {
            samples: self
                .converted
                .iter()
                .map(|sample| (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
                .collect(),
            sample_rate: 16_000,
            captured_at: original.captured_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn speech_conversion_downmixes_without_modifying_original_channels() {
        let original = OriginalFrame {
            samples: [0.75, -0.25].repeat(480).into(),
            channels: 2,
            sample_rate: 48_000,
            captured_at: Instant::now(),
        };
        let before = original.samples.clone();
        let speech = SpeechTap::new().convert(&original);
        assert_eq!(original.samples, before);
        assert_eq!(speech.sample_rate, 16_000);
        assert!((140..=160).contains(&speech.samples.len()));
        assert!(
            speech.samples[20..]
                .iter()
                .all(|&sample| (sample - 8192).abs() <= 1)
        );
    }
}
