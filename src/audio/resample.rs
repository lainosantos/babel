//! Streaming mono resampling for native devices. Coefficients are prepared on a
//! worker, never on the device callback. The low-pass cutoff prevents aliasing
//! when a 44.1/48 kHz device is captured as 16 kHz speech.

use std::{collections::VecDeque, f64::consts::PI};

const TAPS: usize = 64;
const PHASES: usize = 512;

pub(crate) struct Resampler {
    ratio: f64,
    position: f64,
    buffer: VecDeque<f32>,
    coefficients: Vec<[f32; TAPS]>,
    passthrough: bool,
}

/// Independent channel filters retain stereo separation during unavoidable
/// hardware-rate conversion. Equal-rate transport does not filter samples.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(crate) struct ChannelResampler {
    channels: usize,
    filters: Vec<Resampler>,
    input: Vec<f32>,
    outputs: Vec<Vec<f32>>,
    passthrough: bool,
}
#[cfg(any(target_os = "macos", target_os = "windows", test))]
impl ChannelResampler {
    pub(crate) fn new(input_rate: u32, output_rate: u32, channels: u16) -> Self {
        let channels = usize::from(channels);
        Self {
            channels,
            filters: (0..channels)
                .map(|_| Resampler::new(input_rate, output_rate))
                .collect(),
            input: Vec::new(),
            outputs: (0..channels).map(|_| Vec::new()).collect(),
            passthrough: input_rate == output_rate,
        }
    }
    pub(crate) fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if self.passthrough {
            output.extend_from_slice(input);
            return;
        }
        for channel in 0..self.channels {
            self.input.clear();
            self.input.extend(
                input
                    .chunks_exact(self.channels)
                    .map(|frame| frame[channel]),
            );
            self.outputs[channel].clear();
            self.filters[channel].process(&self.input, &mut self.outputs[channel]);
        }
        for frame in 0..self.outputs[0].len() {
            for channel in 0..self.channels {
                output.push(self.outputs[channel][frame]);
            }
        }
    }
    #[cfg(any(target_os = "macos", target_os = "windows", test))]
    pub(crate) fn reset(&mut self) {
        for filter in &mut self.filters {
            filter.reset();
        }
    }
}

impl Resampler {
    pub(crate) fn new(input_rate: u32, output_rate: u32) -> Self {
        if input_rate == output_rate {
            return Self {
                ratio: 1.0,
                position: 0.0,
                buffer: VecDeque::new(),
                coefficients: Vec::new(),
                passthrough: true,
            };
        }
        let cutoff = (f64::from(output_rate) / f64::from(input_rate)).min(1.0) * 0.90;
        let mut coefficients = Vec::with_capacity(PHASES);
        for phase in 0..PHASES {
            let fraction = phase as f64 / PHASES as f64;
            let mut kernel = [0.0; TAPS];
            let mut sum = 0.0;
            for (tap, coefficient) in kernel.iter_mut().enumerate() {
                let x = tap as f64 - (TAPS / 2 - 1) as f64 - fraction;
                let sinc = if x.abs() < 1e-8 {
                    cutoff
                } else {
                    (PI * cutoff * x).sin() / (PI * x)
                };
                let window = 0.42 - 0.5 * (2.0 * PI * tap as f64 / (TAPS - 1) as f64).cos()
                    + 0.08 * (4.0 * PI * tap as f64 / (TAPS - 1) as f64).cos();
                *coefficient = (sinc * window) as f32;
                sum += sinc * window;
            }
            for coefficient in &mut kernel {
                *coefficient /= sum as f32;
            }
            coefficients.push(kernel);
        }
        let mut buffer = VecDeque::with_capacity(8192);
        buffer.extend(std::iter::repeat_n(0.0, TAPS / 2));
        Self {
            ratio: f64::from(input_rate) / f64::from(output_rate),
            position: (TAPS / 2) as f64,
            buffer,
            coefficients,
            passthrough: input_rate == output_rate,
        }
    }

    pub(crate) fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if self.passthrough {
            output.extend_from_slice(input);
            return;
        }
        self.buffer.extend(input.iter().copied());
        while self.position.floor() as usize + TAPS / 2 < self.buffer.len() {
            let center = self.position.floor() as usize;
            let phase = ((self.position.fract() * PHASES as f64) as usize).min(PHASES - 1);
            let start = center - (TAPS / 2 - 1);
            let sample = self.coefficients[phase]
                .iter()
                .enumerate()
                .map(|(tap, coefficient)| self.buffer[start + tap] * coefficient)
                .sum();
            output.push(sample);
            self.position += self.ratio;
        }
        let discard = (self.position.floor() as usize)
            .saturating_sub(TAPS / 2)
            .min(self.buffer.len());
        self.buffer.drain(..discard);
        self.position -= discard as f64;
    }

    #[cfg(any(target_os = "macos", target_os = "windows", test))]
    pub(crate) fn reset(&mut self) {
        self.buffer.clear();
        self.buffer.extend(std::iter::repeat_n(0.0, TAPS / 2));
        self.position = (TAPS / 2) as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_rate_preserves_float_bits_and_does_not_construct_filters() {
        let input = [0.12345679, -0.9876543, f32::from_bits(1), -0.0];
        let mut resampler = Resampler::new(96_000, 96_000);
        assert!(resampler.coefficients.is_empty());
        let mut output = Vec::new();
        resampler.process(&input, &mut output);
        assert_eq!(
            input.map(f32::to_bits).as_slice(),
            output.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn hardware_rate_conversion_keeps_channels_separate() {
        let mut resampler = ChannelResampler::new(96_000, 48_000, 2);
        let input = [0.75, -0.25].repeat(960);
        let mut output = Vec::new();
        resampler.process(&input, &mut output);
        for frame in output.as_chunks::<2>().0.iter().skip(30) {
            assert!((frame[0] - 0.75).abs() < 0.00001);
            assert!((frame[1] + 0.25).abs() < 0.00001);
        }
        assert!(output.len() > 900);
        resampler.reset();
        output.clear();
        resampler.process(&[0.0; 960], &mut output);
        assert!(output.iter().all(|value| *value == 0.0));
    }

    fn convert_tone(frequency: f64) -> Vec<f32> {
        let input: Vec<f32> = (0..48_000)
            .map(|n| (2.0 * PI * frequency * n as f64 / 48_000.0).sin() as f32)
            .collect();
        let mut output = Vec::new();
        let mut resampler = Resampler::new(48_000, 16_000);
        for chunk in input.chunks(960) {
            resampler.process(chunk, &mut output);
        }
        output
    }

    #[test]
    fn downsampling_preserves_voice_and_rejects_aliases() {
        let voice = convert_tone(1000.0);
        let ultrasonic = convert_tone(12_000.0);
        let rms = |signal: &[f32]| {
            (signal[100..].iter().map(|x| f64::from(x * x)).sum::<f64>()
                / (signal.len() - 100) as f64)
                .sqrt()
        };
        assert!((15_980..16_001).contains(&voice.len()));
        assert!(rms(&voice) > 0.65);
        assert!(rms(&ultrasonic) < 0.005);
    }

    #[test]
    fn boundaries_do_not_change_resampling_and_reset_discards_history() {
        let input: Vec<f32> = (0..4800).map(|n| (n as f32 * 0.031).sin()).collect();
        let mut whole = Resampler::new(48_000, 24_000);
        let mut split = Resampler::new(48_000, 24_000);
        let mut a = Vec::new();
        let mut b = Vec::new();
        whole.process(&input, &mut a);
        for chunk in input.chunks(113) {
            split.process(chunk, &mut b);
        }
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(&b).all(|(a, b)| (a - b).abs() < 1e-6));
        split.reset();
        b.clear();
        split.process(&[0.0; 480], &mut b);
        assert!(b.iter().all(|sample| *sample == 0.0));
    }
}
