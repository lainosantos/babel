//! Opt-in Linux software-loopback timing through the production audio backend.
//! Sends synthetic tones only to an owned null sink; never opens physical devices
//! or changes the system defaults. The reported time includes monitor capture,
//! process scheduling and a 10 ms observation frame, not just playback latency.
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This isolated PulseAudio/PipeWire loopback probe runs on Linux only.");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    linux::run().await
}

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{Context, Result, ensure};
    use babel_audio::{
        audio::{self, AudioOptions, AudioStats, OriginalFrame, PlaybackCommand},
        execution,
    };
    use clap::Parser;
    use serde_json::json;
    use std::{
        collections::BTreeSet,
        sync::{Arc, atomic::Ordering},
        time::{Duration, Instant},
    };
    use tokio::{process::Command, sync::mpsc, task::JoinHandle};
    use tokio_util::sync::CancellationToken;

    #[derive(Parser)]
    #[command(about = "Measure production playback with isolated synthetic loopback audio")]
    struct Args {
        #[arg(long, default_value_t = 12, value_parser = clap::value_parser!(u32).range(2..=100))]
        trials: u32,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(5..=500))]
        device_latency_ms: u32,
        #[arg(long, default_value_t = 2_000, value_parser = clap::value_parser!(u32).range(20..=5_000))]
        queue_ms: u32,
    }

    async fn pactl(args: &[String]) -> Result<String> {
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            Command::new("pactl").args(args).kill_on_drop(true).output(),
        )
        .await
        .context("probe pactl timed out")??;
        ensure!(
            output.status.success(),
            "probe pactl failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    async fn persistent_devices() -> Result<(BTreeSet<String>, String, String)> {
        let devices = audio::devices()
            .await?
            .into_iter()
            .filter(|device| device.id.starts_with("babel_"))
            .map(|device| device.id)
            .collect();
        Ok((
            devices,
            pactl(&["get-default-sink".into()]).await?,
            pactl(&["get-default-source".into()]).await?,
        ))
    }

    async fn remove_owned_module(id: u32, name: &str, owner: &str) -> Result<()> {
        let modules = pactl(&["list".into(), "short".into(), "modules".into()]).await?;
        let id_text = id.to_string();
        let Some(row) = modules
            .lines()
            .find(|row| row.split('\t').next() == Some(id_text.as_str()))
        else {
            return Ok(());
        };
        let mut fields = row.split('\t');
        fields.next();
        ensure!(
            fields.next() == Some("module-null-sink"),
            "module ID was reused; refusing cleanup"
        );
        let arguments: BTreeSet<_> = fields
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        ensure!(
            arguments.contains(format!("sink_name={name}").as_str())
                && arguments.contains(format!("sink_properties=babel.test={owner}").as_str()),
            "module ownership changed; refusing cleanup"
        );
        pactl(&["unload-module".into(), id_text]).await?;
        Ok(())
    }

    async fn settle(receiver: &mut mpsc::Receiver<OriginalFrame>) -> Result<()> {
        // Continuously drain the observation queue between pulses. A silent
        // frame alone is insufficient: a previous pulse may still be in flight.
        let stop = tokio::time::sleep(Duration::from_millis(600));
        tokio::pin!(stop);
        let mut silent_frames = 0;
        loop {
            tokio::select! {
                _ = &mut stop => break,
                frame = receiver.recv() => {
                    let frame = frame.context("monitor closed while settling")?;
                    if frame.samples.iter().all(|sample| sample.abs() < 0.01) {
                        silent_frames += 1;
                    } else {
                        silent_frames = 0;
                    }
                }
            }
        }
        ensure!(
            silent_frames >= 5,
            "isolated monitor did not settle to silence"
        );
        while receiver.try_recv().is_ok() {}
        Ok(())
    }

    async fn trial(
        sender: &mpsc::Sender<PlaybackCommand>,
        receiver: &mut mpsc::Receiver<OriginalFrame>,
    ) -> Result<(f64, f64)> {
        let samples = (0..2_400)
            .map(|index| {
                (12_000.0 * (std::f64::consts::TAU * 800.0 * index as f64 / 24_000.0).sin()) as i16
            })
            .collect();
        // Reserve before timing so no generation/allocation/channel-capacity
        // wait is incorrectly attributed to downstream device playback.
        let permit = sender.reserve().await.context("playback worker closed")?;
        let started = Instant::now();
        permit.send(PlaybackCommand::Audio {
            samples,
            generation: 0,
        });
        tokio::time::timeout(Duration::from_secs(4), async {
            while let Some(frame) = receiver.recv().await {
                if let Some(onset) = frame.samples.iter().position(|sample| sample.abs() > 0.05) {
                    let observed_ms =
                        frame.captured_at.duration_since(started).as_secs_f64() * 1000.0;
                    // This removes only the known position inside the received
                    // frame. Monitor/Pulse/process latency remains included.
                    let remaining_ms = (frame.samples.len() - onset) as f64
                        / f64::from(frame.sample_rate)
                        / f64::from(frame.channels)
                        * 1000.0;
                    return Ok((observed_ms, (observed_ms - remaining_ms).max(0.0)));
                }
            }
            Err(anyhow::anyhow!(
                "monitor closed before synthetic pulse arrived"
            ))
        })
        .await
        .context("synthetic pulse did not reach isolated monitor")?
    }

    async fn join_worker(mut worker: JoinHandle<Result<()>>) -> Result<()> {
        match tokio::time::timeout(Duration::from_secs(4), &mut worker).await {
            Ok(result) => result?,
            Err(_) => {
                worker.abort();
                let _ = worker.await;
                anyhow::bail!("probe audio worker did not stop")
            }
        }
    }

    fn percentile(samples: &[f64], fraction: f64) -> f64 {
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let position = (sorted.len() - 1) as f64 * fraction;
        let lower = position.floor() as usize;
        let upper = position.ceil() as usize;
        sorted[lower] + (sorted[upper] - sorted[lower]) * (position - lower as f64)
    }

    pub async fn run() -> Result<()> {
        let args = Args::parse();
        let before = persistent_devices().await?;
        ensure!(
            !before.1.is_empty() && !before.2.is_empty(),
            "existing default devices are required before creating the probe sink"
        );
        let owner = format!("{}_{}", std::process::id(), rand::random::<u64>());
        let sink = format!("babelprobelatency_{owner}");
        let id: u32 = pactl(&[
            "load-module".into(),
            "module-null-sink".into(),
            format!("sink_name={sink}"),
            format!("sink_properties=babel.test={owner}"),
            "rate=24000".into(),
            "channels=1".into(),
            "channel_map=mono".into(),
            "format=float32le".into(),
        ])
        .await?
        .parse()?;
        let cancel = CancellationToken::new();
        let _cancel_on_drop = cancel.clone().drop_guard();
        let playback_stats = Arc::new(AudioStats::default());
        let capture_stats = Arc::new(AudioStats::default());
        let mut workers = Vec::new();
        let result: Result<serde_json::Value> = async {
            pactl(&["set-sink-volume".into(), sink.clone(), "100%".into()]).await?;
            pactl(&["set-source-volume".into(), format!("{sink}.monitor"), "100%".into()]).await?;
            let playback_options = AudioOptions {
                sample_rate: 24_000,
                channels: 1,
                frame_ms: 20,
                latency_ms: args.device_latency_ms,
                queue_ms: args.queue_ms,
            };
            let capture_options = AudioOptions {
                frame_ms: 10,
                latency_ms: 10,
                queue_ms: 80,
                ..playback_options
            };
            let (sender, incoming) = mpsc::channel((args.queue_ms / 20) as usize);
            let (captured, mut receiver) = mpsc::channel(128);
            let handle = execution::audio_handle()?;
            let destination = sink.clone();
            let playback_cancel = cancel.clone();
            let stats = playback_stats.clone();
            workers.push(handle.spawn(async move {
                audio::playback(&destination, playback_options, incoming, playback_cancel, stats).await
            }));
            let monitor = format!("{sink}.monitor");
            let capture_cancel = cancel.clone();
            let stats = capture_stats.clone();
            workers.push(handle.spawn(async move {
                audio::capture(&monitor, capture_options, captured, capture_cancel, stats).await
            }));
            let mut observations = Vec::new();
            let mut adjusted = Vec::new();
            for _ in 0..args.trials {
                settle(&mut receiver).await?;
                let (observed, onset) = trial(&sender, &mut receiver).await?;
                observations.push(observed);
                adjusted.push(onset);
            }
            ensure!(
                capture_stats.capture_lost_frames.load(Ordering::Relaxed) == 0,
                "observation capture dropped frames; timings are invalid"
            );
            Ok(json!({
                "probe": "production_playback_isolated_linux_loopback",
                "playback_sample_rate_hz": 24_000,
                "playback_frame_ms": 20,
                "device_latency_ms": args.device_latency_ms,
                "playback_queue_capacity_ms": args.queue_ms,
                "observation_frame_ms": 10,
                "observation_requested_latency_ms": 10,
                "pulse_duration_ms": 100,
                "trials": args.trials,
                "quantile_method": "linear interpolation over sorted observations",
                "enqueue_to_monitor_block_ms": {
                    "first": observations[0],
                    "median": percentile(&observations, 0.5),
                    "p95": percentile(&observations, 0.95),
                    "min": observations.iter().copied().fold(f64::INFINITY, f64::min),
                    "max": observations.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                    "all": observations
                },
                "frame_position_adjusted_onset_ms": adjusted,
                "limitations": "Software null-sink loopback includes monitor capture and scheduling; excludes provider, network, physical hardware and acoustic latency. Queue capacity is not a target fill level."
            }))
        }.await;
        cancel.cancel();
        let mut failures = Vec::new();
        for worker in workers {
            if let Err(error) = join_worker(worker).await {
                failures.push(format!("{error:#}"));
            }
        }
        let removed = remove_owned_module(id, &sink, &owner).await;
        // Always attempt owned cleanup even if the measurement failed.
        if let Err(error) = &removed {
            eprintln!("Owned probe cleanup failed: {error:#}");
        }
        let measured = result?;
        removed?;
        ensure!(
            failures.is_empty(),
            "probe workers failed: {}",
            failures.join("; ")
        );
        ensure!(
            persistent_devices().await? == before,
            "existing Babel/default devices changed during the probe"
        );
        ensure!(
            !audio::devices()
                .await?
                .iter()
                .any(|device| device.id.contains(&owner)),
            "owned probe endpoint remained after cleanup"
        );
        println!("{}", serde_json::to_string_pretty(&measured)?);
        Ok(())
    }
}
