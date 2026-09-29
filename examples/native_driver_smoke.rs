//! Opt-in, synthetic loopback validation for installed Babel drivers.
//! Never uses default devices and never writes audio or opens an AI provider.
#![forbid(unsafe_code)]

const RATE: u32 = 48_000;
const FREQUENCIES: [[f64; 2]; 2] = [[997.0, 1601.0], [2203.0, 2801.0]];

fn amplitude(samples: &[[f32; 2]], channel: usize, frequency: f64) -> f64 {
    let step = std::f64::consts::TAU * frequency / f64::from(RATE);
    let (mut re, mut im) = (0.0, 0.0);
    for (index, sample) in samples.iter().enumerate() {
        let phase = step * index as f64;
        re += f64::from(sample[channel]) * phase.cos();
        im += f64::from(sample[channel]) * phase.sin();
    }
    2.0 * re.hypot(im) / samples.len().max(1) as f64
}

fn analyze(samples: &[[f32; 2]], cable: usize) -> anyhow::Result<serde_json::Value> {
    anyhow::ensure!(
        samples.len() >= RATE as usize,
        "less than one second captured"
    );
    let samples = &samples[samples.len() - RATE as usize..];
    let mut channels = Vec::new();
    for (channel, expected) in FREQUENCIES[cable].into_iter().enumerate() {
        let signal = amplitude(samples, channel, expected);
        let unwanted = FREQUENCIES
            .into_iter()
            .flatten()
            .filter(|frequency| *frequency != expected)
            .map(|frequency| amplitude(samples, channel, frequency))
            .fold(0.0_f64, f64::max);
        anyhow::ensure!(
            signal > 0.02,
            "cable {cable}, channel {channel}: expected tone missing"
        );
        anyhow::ensure!(
            unwanted < signal * 0.06,
            "cable {cable}, channel {channel}: channel swap or crosstalk"
        );
        channels.push(serde_json::json!({
            "channel": channel + 1,
            "frequency_hz": expected,
            "amplitude": signal,
            "strongest_unwanted_amplitude": unwanted,
        }));
    }
    Ok(serde_json::json!({"channels": channels}))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() -> anyhow::Result<()> {
    // Keep the portable analyzer compiled and tested on Linux too.
    let _ = analyze(&[], 0);
    anyhow::bail!(
        "This opt-in driver smoke runs only on macOS or Windows; use cargo test --example native_driver_smoke for its portable checks"
    )
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() -> anyhow::Result<()> {
    native::run()
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use super::*;
    use anyhow::{Context, Result, bail, ensure};
    use clap::Parser;
    use cpal::{
        FromSample, Sample, SampleFormat, SizedSample, StreamConfig,
        traits::{DeviceTrait, HostTrait, StreamTrait},
    };
    use crossbeam_queue::ArrayQueue;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        time::Duration,
    };

    #[derive(Parser)]
    #[command(
        about = "Synthetic 48 kHz stereo smoke for installed Babel drivers. Close Babel and other audio clients first. Copies explicit IDs from `babel devices`; never selects defaults."
    )]
    struct Args {
        #[arg(long)]
        microphone_render: String,
        #[arg(long)]
        microphone_capture: String,
        #[arg(long)]
        speaker_render: String,
        #[arg(long)]
        speaker_capture: String,
        /// Explicit acknowledgement that all four IDs are virtual endpoints.
        #[arg(long, required = true)]
        confirm_virtual_devices: bool,
    }

    struct Capture {
        frames: Arc<ArrayQueue<[f32; 2]>>,
        dropped: Arc<AtomicU64>,
        failed: Arc<AtomicBool>,
        stream: cpal::Stream,
    }

    fn device(id: &str, input: bool) -> Result<cpal::Device> {
        let prefix = if input { "input:" } else { "output:" };
        let native = id
            .strip_prefix(prefix)
            .context("ID has the wrong direction")?;
        let native: cpal::DeviceId = native
            .parse()
            .context("Use a persistent ID from babel devices")?;
        let host = cpal::default_host();
        ensure!(native.host() == host.id(), "ID belongs to a different OS");
        let device = host
            .device_by_id(&native)
            .context("Explicit endpoint is unavailable")?;
        let description = device.description()?;
        #[cfg(target_os = "macos")]
        ensure!(
            matches!(
                native.id(),
                "org.babel.audio.microphone.v1" | "org.babel.audio.speaker.v1"
            ),
            "Smoke requires an exact Babel virtual-device UID; refusing another device"
        );
        #[cfg(target_os = "windows")]
        ensure!(
            description.driver() == Some("Babel Audio v1"),
            "Smoke requires Babel driver interface metadata; a renamed physical device is not accepted"
        );
        ensure!(
            description.name().starts_with("Babel "),
            "Smoke requires an installed Babel endpoint with its original name; refusing to open a physical device"
        );
        Ok(device)
    }

    fn configuration(device: &cpal::Device, input: bool) -> Result<(StreamConfig, SampleFormat)> {
        let ranges: Vec<_> = if input {
            device.supported_input_configs()?.collect()
        } else {
            device.supported_output_configs()?.collect()
        };
        let range = ranges
            .into_iter()
            .find(|range| {
                range.channels() == 2
                    && range.min_sample_rate() <= RATE
                    && range.max_sample_rate() >= RATE
                    && matches!(range.sample_format(), SampleFormat::F32 | SampleFormat::I16)
            })
            .context("Endpoint does not expose stereo 48000 Hz PCM16/f32")?;
        let format = range.sample_format();
        Ok((range.with_sample_rate(RATE).config(), format))
    }

    fn capture_as<T: SizedSample>(
        device: &cpal::Device,
        config: &StreamConfig,
        frames: Arc<ArrayQueue<[f32; 2]>>,
        dropped: Arc<AtomicU64>,
        failed: Arc<AtomicBool>,
    ) -> Result<cpal::Stream>
    where
        f32: FromSample<T>,
    {
        Ok(device.build_input_stream(
            *config,
            move |data: &[T], _| {
                for frame in data.as_chunks::<2>().0 {
                    if frames
                        .push([f32::from_sample(frame[0]), f32::from_sample(frame[1])])
                        .is_err()
                    {
                        dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            },
            move |_| {
                failed.store(true, Ordering::Relaxed);
            },
            None,
        )?)
    }

    fn capture(device: &cpal::Device) -> Result<Capture> {
        let (config, format) = configuration(device, true)?;
        // Four seconds is a fixed upper bound. No callback allocates or waits.
        let frames = Arc::new(ArrayQueue::new(RATE as usize * 4));
        let dropped = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = match format {
            SampleFormat::F32 => capture_as::<f32>(
                device,
                &config,
                frames.clone(),
                dropped.clone(),
                failed.clone(),
            )?,
            SampleFormat::I16 => capture_as::<i16>(
                device,
                &config,
                frames.clone(),
                dropped.clone(),
                failed.clone(),
            )?,
            _ => unreachable!(),
        };
        Ok(Capture {
            frames,
            dropped,
            failed,
            stream,
        })
    }

    fn render_as<T: SizedSample + FromSample<f32>>(
        device: &cpal::Device,
        config: &StreamConfig,
        cable: usize,
        failed: Arc<AtomicBool>,
    ) -> Result<cpal::Stream> {
        // Generate one period outside the callback; every frequency is integral.
        let wave: Vec<[f32; 2]> = (0..RATE)
            .map(|frame| {
                FREQUENCIES[cable].map(|frequency| {
                    (0.08
                        * (std::f64::consts::TAU * frequency * f64::from(frame) / f64::from(RATE))
                            .sin()) as f32
                })
            })
            .collect();
        let mut position = 0;
        Ok(device.build_output_stream(
            *config,
            move |data: &mut [T], _| {
                for frame in data.as_chunks_mut::<2>().0 {
                    frame[0] = T::from_sample(wave[position][0]);
                    frame[1] = T::from_sample(wave[position][1]);
                    position = (position + 1) % wave.len();
                }
            },
            move |_| {
                failed.store(true, Ordering::Relaxed);
            },
            None,
        )?)
    }

    pub fn run() -> Result<()> {
        let args = Args::parse();
        ensure!(
            args.confirm_virtual_devices,
            "Explicit virtual-device acknowledgement required"
        );
        ensure!(
            args.microphone_render != args.speaker_render
                && args.microphone_capture != args.speaker_capture,
            "Choose two independent virtual cables"
        );
        let input = [
            device(&args.microphone_capture, true)?,
            device(&args.speaker_capture, true)?,
        ];
        let output = [
            device(&args.microphone_render, false)?,
            device(&args.speaker_render, false)?,
        ];
        let captures = [capture(&input[0])?, capture(&input[1])?];
        let failed = Arc::new(AtomicBool::new(false));
        let mut renders = Vec::new();
        for (index, device) in output.iter().enumerate() {
            let (config, format) = configuration(device, false)?;
            renders.push(match format {
                SampleFormat::F32 => render_as::<f32>(device, &config, index, failed.clone())?,
                SampleFormat::I16 => render_as::<i16>(device, &config, index, failed.clone())?,
                _ => bail!("Unsupported sample format"),
            });
        }
        for capture in &captures {
            capture.stream.play()?;
        }
        for render in &renders {
            render.play()?;
        }
        std::thread::sleep(Duration::from_secs(3));
        for render in renders {
            drop(render);
        }
        let mut report = Vec::new();
        for (index, capture) in captures.into_iter().enumerate() {
            drop(capture.stream);
            ensure!(
                !capture.failed.load(Ordering::Relaxed),
                "Capture callback failed"
            );
            ensure!(
                capture.dropped.load(Ordering::Relaxed) == 0,
                "Capture queue overflow"
            );
            let mut samples = Vec::new();
            while let Some(frame) = capture.frames.pop() {
                samples.push(frame);
            }
            let analysis = analyze(&samples, index)?;
            report.push(serde_json::json!({"cable": index, "captured_frames": samples.len(), "analysis": analysis}));
        }
        ensure!(!failed.load(Ordering::Relaxed), "Render callback failed");
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": "passed", "sample_rate": RATE, "seconds": 3, "cables": report,
                "scope": "Synthetic stereo transport and cable isolation only; no physical audio, AI, latency or long-duration certification."
            }))?
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tone(cable: usize) -> Vec<[f32; 2]> {
        (0..RATE)
            .map(|i| {
                FREQUENCIES[cable].map(|f| {
                    (0.08 * (std::f64::consts::TAU * f * f64::from(i) / f64::from(RATE)).sin())
                        as f32
                })
            })
            .collect()
    }
    #[test]
    fn verifies_both_cables_and_rejects_silence_crossed_cables_and_channels() {
        for cable in 0..2 {
            let valid = tone(cable);
            assert!(analyze(&valid, cable).is_ok());
            assert!(analyze(&valid, 1 - cable).is_err());
            let swapped: Vec<_> = valid.iter().map(|frame| [frame[1], frame[0]]).collect();
            assert!(analyze(&swapped, cable).is_err());
            let other = tone(1 - cable);
            let mixed: Vec<_> = valid
                .iter()
                .zip(other)
                .map(|(a, b)| [a[0] + b[0], a[1] + b[1]])
                .collect();
            assert!(analyze(&mixed, cable).is_err());
        }
        assert!(analyze(&vec![[0.0; 2]; RATE as usize], 0).is_err());
        assert!(analyze(&[], 0).is_err());
    }
}
