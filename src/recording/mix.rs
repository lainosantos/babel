//! Recording-only level balance. No model, microphone gate, automatic gain,
//! routing mutation or source-audio mutation occurs in this mixer.
use super::RecordingMixConfig;
use std::collections::VecDeque;

const SAMPLE_RATE: f64 = 16_000.0;
const LOOKAHEAD: usize = 80; // Five milliseconds, inside the recorder only.
const PEAK_CEILING: f64 = 0.98;

fn amplitude(db: f32) -> f64 {
    10.0_f64.powf(f64::from(db) / 20.0)
}

fn smoothing(milliseconds: f64) -> f64 {
    1.0 - (-1.0 / (SAMPLE_RATE * milliseconds / 1000.0)).exp()
}

struct MicrophonePriority {
    enabled: bool,
    threshold_power: f64,
    attenuation: f64,
    power: f64,
    active: bool,
    hold: usize,
    gain: f64,
    detector: f64,
    attack: f64,
    release: f64,
}

impl MicrophonePriority {
    fn next_gain(&mut self, microphone: f64) -> f64 {
        if !self.enabled {
            return 1.0;
        }
        // Measure original microphone energy before its optional manual gain.
        // Hysteresis/hold suppress chatter; no captured microphone sample is
        // gated and quiet/noisy passages are never normalized toward a target.
        self.power += self.detector * (microphone * microphone - self.power);
        if self.power < 1e-18 {
            self.power = 0.0;
        }
        if !self.active && self.power >= self.threshold_power {
            self.active = true;
        }
        if self.active {
            if self.power >= self.threshold_power * 0.25 {
                self.hold = 2400; // 150 ms below the -6 dB release threshold.
            } else if self.hold > 0 {
                self.hold -= 1;
            } else {
                self.active = false;
            }
        }
        let target = if self.active { self.attenuation } else { 1.0 };
        let coefficient = if target < self.gain {
            self.attack
        } else {
            self.release
        };
        self.gain += coefficient * (target - self.gain);
        if (target - self.gain).abs() < 1e-9 {
            self.gain = target;
        }
        self.gain
    }
}

pub(super) struct RecordingMixer {
    active: [bool; 2],
    divisor: i32,
    gains: [f64; 2],
    transparent: bool,
    priority: MicrophonePriority,
    limit: bool,
    limiter_gain: f64,
    limiter_release: f64,
    lookahead: VecDeque<f64>,
}

impl RecordingMixer {
    /// The recorder validates configuration before any file is created.
    pub(super) fn new(config: &RecordingMixConfig, microphone: bool, speaker: bool) -> Self {
        let priority =
            config.microphone_priority && microphone && speaker && config.ducking_db > 0.0;
        let gains = [
            if microphone {
                amplitude(config.microphone_gain_db)
            } else {
                0.0
            },
            if speaker {
                amplitude(config.speaker_gain_db)
            } else {
                0.0
            },
        ];
        Self {
            active: [microphone, speaker],
            divisor: if microphone && speaker { 2 } else { 1 },
            gains,
            transparent: !priority
                && (!microphone || config.microphone_gain_db == 0.0)
                && (!speaker || config.speaker_gain_db == 0.0),
            priority: MicrophonePriority {
                enabled: priority,
                threshold_power: amplitude(config.microphone_threshold_db).powi(2),
                attenuation: amplitude(-config.ducking_db),
                power: 0.0,
                active: false,
                hold: 0,
                gain: 1.0,
                detector: smoothing(10.0),
                attack: smoothing(15.0),
                release: smoothing(300.0),
            },
            limit: gains.iter().any(|gain| *gain > 1.0),
            limiter_gain: 1.0,
            limiter_release: smoothing(100.0),
            lookahead: VecDeque::with_capacity(LOOKAHEAD + 1),
        }
    }

    pub(super) fn push(&mut self, mut sources: [i32; 2]) -> Option<i16> {
        for (sample, active) in sources.iter_mut().zip(self.active) {
            if !active {
                *sample = 0;
            }
        }
        if self.transparent {
            // Preserve the old signed integer division exactly, including odd
            // sums and full-scale negative PCM. No DSP delay exists in this mode.
            let sample = (sources[0] + sources[1]) / self.divisor;
            return Some(sample.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16);
        }
        let microphone = f64::from(sources[0]) / 32768.0;
        let speaker = f64::from(sources[1]) / 32768.0;
        let output_gain = self.priority.next_gain(microphone);
        let mixed = (microphone * self.gains[0] + speaker * self.gains[1] * output_gain)
            / f64::from(self.divisor);
        if !self.limit {
            return Some(quantize(mixed));
        }
        self.lookahead.push_back(mixed);
        (self.lookahead.len() > LOOKAHEAD).then(|| self.render_oldest())
    }

    pub(super) fn finish_sample(&mut self) -> Option<i16> {
        (!self.lookahead.is_empty()).then(|| self.render_oldest())
    }

    fn render_oldest(&mut self) -> i16 {
        // Each future over-limit peak imposes a linear gain ceiling that reaches
        // its required attenuation exactly when that sample is output. A newly
        // entering peak starts at unity, so attack ramps cannot jump on a later
        // block boundary. Both sources share the same limiter after their mix.
        let mut allowed_gain = 1.0_f64;
        for (distance, sample) in self.lookahead.iter().enumerate() {
            let peak = sample.abs();
            if peak > PEAK_CEILING {
                let required = PEAK_CEILING / peak;
                let allowed = required + (1.0 - required) * distance as f64 / LOOKAHEAD as f64;
                allowed_gain = allowed_gain.min(allowed);
            }
        }
        self.limiter_gain = (self.limiter_gain + self.limiter_release * (1.0 - self.limiter_gain))
            .min(allowed_gain);
        quantize(self.lookahead.pop_front().unwrap() * self.limiter_gain)
    }
}

fn quantize(sample: f64) -> i16 {
    (sample * 32768.0)
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn render(config: &RecordingMixConfig, sources: &[[i32; 2]]) -> Vec<i16> {
        let mut mixer = RecordingMixer::new(config, true, true);
        let mut output: Vec<_> = sources
            .iter()
            .filter_map(|samples| mixer.push(*samples))
            .collect();
        assert!(mixer.lookahead.len() <= LOOKAHEAD);
        while let Some(sample) = mixer.finish_sample() {
            output.push(sample);
        }
        assert_eq!(output.len(), sources.len());
        output
    }

    fn component(samples: &[i16], frequency: f64) -> f64 {
        let mut real = 0.0;
        let mut imaginary = 0.0;
        for (index, &sample) in samples.iter().enumerate() {
            let angle = TAU * frequency * index as f64 / SAMPLE_RATE;
            real += f64::from(sample) * angle.cos();
            imaginary += f64::from(sample) * angle.sin();
        }
        2.0 * real.hypot(imaginary) / samples.len() as f64
    }

    #[test]
    fn quiet_microphone_becomes_audible_against_loud_incoming_music() {
        let sources: Vec<_> = (0..16_000)
            .map(|index| {
                [
                    (260.0 * (TAU * 300.0 * index as f64 / SAMPLE_RATE).sin()).round() as i32,
                    (5000.0 * (TAU * 900.0 * index as f64 / SAMPLE_RATE).sin()).round() as i32,
                ]
            })
            .collect();
        let config = RecordingMixConfig {
            microphone_gain_db: 12.0,
            ..Default::default()
        };
        let mut unbalanced = config.clone();
        unbalanced.microphone_priority = false;
        let before = render(&unbalanced, &sources);
        let after = render(&config, &sources);
        // Measure each independent source frequency after the attack settles.
        // A blanket volume increase cannot pass this relative-intelligibility test.
        let before_ratio = component(&before[8000..], 300.0) / component(&before[8000..], 900.0);
        let after_ratio = component(&after[8000..], 300.0) / component(&after[8000..], 900.0);
        let improvement = 20.0 * (after_ratio / before_ratio).log10();
        assert!((11.8..12.2).contains(&improvement), "{improvement} dB");
        assert!(
            (component(&after[8000..], 300.0) / component(&before[8000..], 300.0) - 1.0).abs()
                < 0.01
        );
    }

    #[test]
    fn silence_and_low_microphone_noise_do_not_pump_incoming_audio() {
        for noise in [0, 30] {
            let sources: Vec<_> = (0..16_000)
                .map(|index| [if index % 2 == 0 { noise } else { -noise }, 3000])
                .collect();
            let result = render(&RecordingMixConfig::default(), &sources);
            for (input, output) in sources.iter().zip(result) {
                assert_eq!(i32::from(output), (input[0] + input[1]) / 2);
            }
        }
        let boosted = RecordingMixConfig {
            microphone_gain_db: 24.0,
            speaker_gain_db: 24.0,
            ..Default::default()
        };
        assert!(
            render(&boosted, &[[0; 2]; 300])
                .iter()
                .all(|sample| *sample == 0)
        );
    }

    #[test]
    fn priority_attack_hold_and_release_are_smooth_and_never_gate_microphone() {
        let sources: Vec<_> = (0..48_000)
            .map(|index| {
                [
                    if (4000..20_000).contains(&index) {
                        2000
                    } else {
                        0
                    },
                    10_000,
                ]
            })
            .collect();
        let result = render(&RecordingMixConfig::default(), &sources);
        let output_gains: Vec<_> = sources
            .iter()
            .zip(&result)
            .map(|(input, output)| (f64::from(*output) * 2.0 - f64::from(input[0])) / 10_000.0)
            .collect();
        assert!(output_gains[..4000].iter().all(|gain| *gain == 1.0));
        assert!(output_gains[19_000] < 0.26);
        assert!(output_gains[21_500] < 0.26, "hold protects short pauses");
        assert!(
            output_gains[47_999] > 0.99,
            "incoming audio recovers after speech"
        );
        assert!(
            output_gains
                .windows(2)
                .all(|pair| (pair[1] - pair[0]).abs() < 0.004)
        );
        let microphone_only: Vec<_> = sources.iter().map(|frame| [frame[0], 0]).collect();
        let preserved = render(&RecordingMixConfig::default(), &microphone_only);
        assert!(
            microphone_only
                .iter()
                .zip(preserved)
                .all(|(input, output)| i32::from(output) == input[0] / 2)
        );
    }

    #[test]
    fn boosted_peaks_are_limited_with_lookahead_and_smooth_recovery() {
        let config = RecordingMixConfig {
            microphone_gain_db: 24.0,
            microphone_priority: false,
            ..Default::default()
        };
        let mut mixer = RecordingMixer::new(&config, true, false);
        let mut sources = vec![1000; 6000];
        sources[2000] = 30_000;
        sources[4000] = -30_000;
        let mut output: Vec<_> = sources
            .iter()
            .filter_map(|sample| mixer.push([*sample, 0]))
            .collect();
        assert_eq!(output.len(), sources.len() - LOOKAHEAD);
        while let Some(sample) = mixer.finish_sample() {
            output.push(sample);
        }
        assert_eq!(output.len(), sources.len());
        assert!(
            output
                .iter()
                .all(|sample| f64::from(*sample).abs() <= (PEAK_CEILING * 32768.0).ceil())
        );
        assert!(output[2000] > 30_000 && output[4000] < -30_000);
        let gains: Vec<_> = sources
            .iter()
            .zip(output)
            .map(|(source, output)| f64::from(output) / (f64::from(*source) * amplitude(24.0)))
            .collect();
        assert!(
            gains
                .windows(2)
                .all(|pair| (pair[1] - pair[0]).abs() < 0.013)
        );
        assert!(
            gains[1999] < gains[1900],
            "attenuation begins before the loud peak"
        );
        assert!(gains[5999] > gains[4100], "gain releases after the peak");
    }

    #[test]
    fn transparent_mode_preserves_integer_mixing_and_source_selection() {
        let sources = [[32767, 32767], [-32768, -32768], [1, 0], [-1, 0], [200, -3]];
        let mut config = RecordingMixConfig::transparent();
        for active in [[true, true], [true, false], [false, true]] {
            let mut mixer = RecordingMixer::new(&config, active[0], active[1]);
            for frame in sources {
                let sum = frame
                    .iter()
                    .zip(active)
                    .map(|(sample, active)| if active { *sample } else { 0 })
                    .sum::<i32>();
                assert_eq!(
                    mixer.push(frame),
                    Some((sum / if active == [true, true] { 2 } else { 1 }) as i16)
                );
            }
            assert!(mixer.finish_sample().is_none());
        }
        // An unselected microphone cannot duck the speaker or force a limiter.
        config.microphone_gain_db = 24.0;
        config.microphone_priority = true;
        let mut mixer = RecordingMixer::new(&config, false, true);
        assert_eq!(mixer.push([32767, -1]), Some(-1));
    }
}
