//! Speech-only conversion. Never run this DSP on the original audio executor.
use super::{OriginalFrame, PcmFrame, resample::Resampler};
use std::time::{Duration, Instant};

pub struct SpeechTap {
    resampler: Option<Resampler>,
    format: Option<(u32, u16)>,
    previous: Option<Instant>,
    input_samples: u64,
    output_samples: u64,
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
            input_samples: 0,
            output_samples: 0,
            mono: Vec::new(),
            converted: Vec::new(),
        }
    }

    fn needs_reset(&self, original: &OriginalFrame) -> bool {
        let channels = usize::from(original.channels.max(1));
        let duration = Duration::from_secs_f64(
            original.samples.len() as f64 / channels as f64 / f64::from(original.sample_rate),
        );
        let discontinuity = self.previous.is_some_and(|previous| {
            original.captured_at.saturating_duration_since(previous)
                > duration + Duration::from_millis(50)
        });
        self.format != Some((original.sample_rate, original.channels)) || discontinuity
    }

    /// Preserve delayed speech before a device format or capture gap resets the
    /// filter. The caller delivers this frame before converting the new input.
    pub fn finish_before(&mut self, original: &OriginalFrame) -> Option<PcmFrame> {
        if self.needs_reset(original) {
            self.finish()
        } else {
            None
        }
    }

    /// Emit the filter's delayed samples at their original capture boundary.
    /// Repeated calls are empty; no artificial silence extends the source.
    pub fn finish(&mut self) -> Option<PcmFrame> {
        let captured_at = self.previous.take()?;
        self.converted.clear();
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.finish(&mut self.converted);
        }
        self.format = None;
        self.input_samples = 0;
        self.output_samples = 0;
        (!self.converted.is_empty()).then(|| self.frame(captured_at))
    }

    pub fn convert(&mut self, original: &OriginalFrame) -> PcmFrame {
        let channels = usize::from(original.channels.max(1));
        if self.needs_reset(original) {
            self.resampler = Some(Resampler::new(original.sample_rate, 16_000));
            self.format = Some((original.sample_rate, original.channels));
            self.input_samples = 0;
            self.output_samples = 0;
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
        self.input_samples = self.input_samples.saturating_add(self.mono.len() as u64);
        self.output_samples = self
            .output_samples
            .saturating_add(self.converted.len() as u64);
        let expected =
            (u128::from(self.input_samples) * 16_000).div_ceil(u128::from(original.sample_rate));
        let delayed = expected.saturating_sub(u128::from(self.output_samples));
        let delay = Duration::from_secs_f64(delayed as f64 / 16_000.0);
        self.frame(
            original
                .captured_at
                .checked_sub(delay)
                .unwrap_or(original.captured_at),
        )
    }

    fn frame(&self, captured_at: Instant) -> PcmFrame {
        PcmFrame {
            samples: self
                .converted
                .iter()
                .map(|sample| (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
                .collect(),
            sample_rate: 16_000,
            captured_at,
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

    #[test]
    fn eof_preserves_exact_speech_duration_and_capture_clock() {
        for sample_rate in [16_000, 44_100, 48_000, 96_000] {
            let origin = Instant::now();
            let mut speech = SpeechTap::new();
            let mut samples = 0;
            let mut previous = origin;
            for index in 1..=3 {
                let frame = speech.convert(&OriginalFrame {
                    samples: vec![0.25; (sample_rate / 100) as usize * 2].into(),
                    channels: 2,
                    sample_rate,
                    captured_at: origin + Duration::from_millis(index * 10),
                });
                samples += frame.samples.len();
                assert!(frame.captured_at >= previous);
                assert_eq!(
                    frame.captured_at.duration_since(origin),
                    Duration::from_secs_f64(samples as f64 / 16_000.0),
                );
                previous = frame.captured_at;
            }
            if let Some(tail) = speech.finish() {
                assert_eq!(tail.captured_at, origin + Duration::from_millis(30));
                assert!(tail.captured_at >= previous);
                samples += tail.samples.len();
            }
            assert_eq!(samples, 480, "source rate {sample_rate}");
            assert!(speech.finish().is_none());
        }
    }

    #[test]
    fn format_changes_and_capture_gaps_preserve_the_previous_tail() {
        for (sample_rate, end_ms) in [(44_100, 20), (48_000, 100)] {
            let origin = Instant::now();
            let mut speech = SpeechTap::new();
            let first = speech.convert(&OriginalFrame {
                samples: vec![0.25; 480].into(),
                channels: 1,
                sample_rate: 48_000,
                captured_at: origin + Duration::from_millis(10),
            });
            let second = OriginalFrame {
                samples: vec![0.5; (sample_rate / 100) as usize].into(),
                channels: 1,
                sample_rate,
                captured_at: origin + Duration::from_millis(end_ms),
            };
            let tail = speech.finish_before(&second).unwrap();
            assert_eq!(first.samples.len() + tail.samples.len(), 160);
            assert_eq!(tail.captured_at, origin + Duration::from_millis(10));
            let converted = speech.convert(&second);
            let tail = speech.finish().unwrap();
            assert_eq!(converted.samples.len() + tail.samples.len(), 160);
            assert_eq!(tail.captured_at, second.captured_at);
        }
    }
}
