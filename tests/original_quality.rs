#![cfg(target_os = "linux")]
//! Opt-in hardware-free Pulse integration: only uniquely owned null sinks.
//! No physical device is opened and existing/default devices are never changed.
use anyhow::{Context, Result, ensure};
use babel_audio::audio::{
    self, AudioOptions, AudioStats, OriginalFrame, PlaybackCommand,
    passthrough::{self, RouteDevices},
};
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::Duration,
};
use tokio::{
    process::Command,
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

async fn pactl(args: &[String]) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new("pactl").args(args).kill_on_drop(true).output(),
    )
    .await
    .context("fixture pactl timed out")??;
    ensure!(
        output.status.success(),
        "fixture pactl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

async fn existing_devices() -> Result<(BTreeSet<String>, String, String)> {
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

async fn cleanup(modules: &[(u32, String)], owner: &str) -> Result<()> {
    let mut failures = Vec::new();
    for (id, name) in modules.iter().rev() {
        let removed: Result<()> = async {
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
            let args: BTreeSet<_> = fields
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .collect();
            ensure!(
                args.contains(format!("sink_name={name}").as_str())
                    && args.contains(format!("sink_properties=babel.test={owner}").as_str()),
                "module ownership changed; refusing cleanup"
            );
            pactl(&["unload-module".into(), id_text]).await?;
            Ok(())
        }
        .await;
        if let Err(error) = removed {
            failures.push(format!("{error:#}"));
        }
    }
    ensure!(
        failures.is_empty(),
        "fixture cleanup failed: {}",
        failures.join("; ")
    );
    Ok(())
}

// Co-prime periods identify every stereo frame. Values intentionally contain
// detail below one PCM16 step; converting to i16 cannot satisfy bit equality.
fn samples(index: usize) -> [f32; 2] {
    [
        0.123_456_78 + (index % 29) as f32 * 0.000_310_123,
        -0.234_567_9 - (index % 31) as f32 * 0.000_290_321,
    ]
}

async fn verify(mut observed: mpsc::Receiver<OriginalFrame>) -> Result<usize> {
    const PERIOD: usize = 29 * 31;
    let phases: HashMap<_, _> = (0..PERIOD)
        .map(|index| {
            let pair = samples(index);
            ((pair[0].to_bits(), pair[1].to_bits()), index)
        })
        .collect();
    let mut previous = None;
    let mut consecutive = 0;
    let mut best = 0;
    tokio::time::timeout(Duration::from_secs(8), async {
        while let Some(frame) = observed.recv().await {
            ensure!(
                frame.sample_rate == 48_000 && frame.channels == 2,
                "original format changed"
            );
            for pair in frame.samples.as_chunks::<2>().0 {
                let phase = phases.get(&(pair[0].to_bits(), pair[1].to_bits())).copied();
                consecutive = match (previous, phase) {
                    (Some(last), Some(next)) if next == (last + 1) % PERIOD => consecutive + 1,
                    (_, Some(_)) => 1,
                    _ => 0,
                };
                previous = phase;
                best = best.max(consecutive);
                if consecutive >= 4_800 {
                    return Ok(best);
                }
            }
        }
        Err(anyhow::anyhow!(
            "observer closed; longest exact stereo sequence: {best}"
        ))
    })
    .await
    .with_context(|| {
        format!("no 100 ms bit-exact stereo interval; longest exact sequence: {best}")
    })?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires live PulseAudio/PipeWire; creates/removes only two uniquely owned synthetic float stereo null sinks"]
async fn original_stereo_float_samples_survive_the_live_routing_path() -> Result<()> {
    let before = existing_devices().await?;
    let owner = format!("{}_{}", std::process::id(), rand::random::<u64>());
    let names = [
        format!("babeltestquality_{owner}_source"),
        format!("babeltestquality_{owner}_destination"),
    ];
    let mut modules = Vec::new();
    let mut workers: Vec<JoinHandle<Result<()>>> = Vec::new();
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let options = AudioOptions {
        sample_rate: 48_000,
        channels: 2,
        frame_ms: 10,
        latency_ms: 40,
        queue_ms: 80,
    };
    let result: Result<()> = async {
        for name in &names {
            let id = pactl(&["load-module".into(), "module-null-sink".into(), format!("sink_name={name}"), format!("sink_properties=babel.test={owner}"), "rate=48000".into(), "channels=2".into(), "channel_map=front-left,front-right".into(), "format=float32le".into()]).await?.parse()?;
            modules.push((id, name.clone()));
            pactl(&["set-sink-volume".into(), name.clone(), "100%".into()]).await?;
            pactl(&["set-source-volume".into(), format!("{name}.monitor"), "100%".into()]).await?;
        }
        let (_capture, capture) = watch::channel(format!("{}.monitor", names[0]));
        let (_playback, playback) = watch::channel(names[1].clone());
        let route_cancel = cancel.clone();
        workers.push(tokio::spawn(async move {
            passthrough::run_route(RouteDevices { capture, playback }, options, route_cancel, Arc::new(AudioStats::default())).await
        }));
        let observer_cancel = cancel.clone();
        let observed_device = format!("{}.monitor", names[1]);
        let (captured, observed) = mpsc::channel(32);
        workers.push(tokio::spawn(async move {
            audio::capture(&observed_device, options, captured, observer_cancel, Arc::new(AudioStats::default())).await
        }));
        let producer_cancel = cancel.clone();
        let source = names[0].clone();
        let (send, incoming) = mpsc::channel(8);
        workers.push(tokio::spawn(async move {
            audio::playback(&source, options, incoming, producer_cancel, Arc::new(AudioStats::default())).await
        }));
        let feeder_cancel = cancel.clone();
        workers.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(10));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            for block in 0..600 {
                tokio::select! { biased; _ = feeder_cancel.cancelled() => return Ok(()), _ = tick.tick() => {} }
                let samples = (block * 480..(block + 1) * 480).flat_map(samples).collect();
                tokio::select! {
                    biased;
                    _ = feeder_cancel.cancelled() => return Ok(()),
                    sent = send.send(PlaybackCommand::Original { samples, generation: 0 }) => { sent.context("synthetic producer closed")?; }
                }
            }
            Ok(())
        }));
        let count = verify(observed).await?;
        println!("Verified {count} consecutive bit-exact stereo f32 frames at 48 kHz through live Babel routing");
        Ok(())
    }.await;
    cancel.cancel();
    let mut errors = Vec::new();
    for mut worker in workers {
        match tokio::time::timeout(Duration::from_secs(4), &mut worker).await {
            Ok(Ok(Ok(()))) => (),
            Ok(Ok(Err(error))) => errors.push(format!("{error:#}")),
            Ok(Err(error)) => errors.push(error.to_string()),
            Err(_) => {
                worker.abort();
                errors.push("fixture worker did not stop".into());
            }
        }
    }
    let removed = cleanup(&modules, &owner).await;
    if let Err(error) = &removed {
        eprintln!("Owned fixture cleanup failed: {error:#}");
    }
    result?;
    removed?;
    ensure!(
        errors.is_empty(),
        "fixture workers failed: {}",
        errors.join("; ")
    );
    ensure!(
        existing_devices().await? == before,
        "existing Babel/default devices changed"
    );
    ensure!(
        !audio::devices()
            .await?
            .iter()
            .any(|device| device.id.contains(&owner)),
        "fixture devices remained after cleanup"
    );
    Ok(())
}
