//! Native output regression using isolated virtual devices and synthetic PCM.
use super::*;
use tokio::process::Command;

async fn pactl(args: &[String]) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new("pactl").args(args).kill_on_drop(true).output(),
    )
    .await??;
    ensure!(
        output.status.success(),
        "Test audio module operation failed"
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[tokio::test]
#[ignore = "requires PipeWire/PulseAudio; uses only two isolated test sinks and one remapped source, without physical audio or cloud calls"]
async fn translated_pcm_reaches_the_call_microphone_and_speaker_output() -> Result<()> {
    let before = audio::devices().await?;
    let owner = format!("{}_{}", std::process::id(), rand::random::<u64>());
    let mic = format!("babeltesttranslate_{owner}_mic");
    let speaker = format!("babeltesttranslate_{owner}_speaker");
    let source = format!("babeltesttranslate_{owner}_source");
    let mut modules = Vec::new();
    let result: Result<()> = async {
        for sink in [&mic, &speaker] {
            let id = pactl(&[
                "load-module".into(),
                "module-null-sink".into(),
                format!("sink_name={sink}"),
                format!("sink_properties=babel.test={owner}"),
                "rate=48000".into(),
                "channels=2".into(),
                "channel_map=front-left,front-right".into(),
            ])
            .await?;
            modules.push(id);
        }
        modules.push(
            pactl(&[
                "load-module".into(),
                "module-remap-source".into(),
                format!("source_name={source}"),
                format!("master={mic}.monitor"),
                format!("source_properties=babel.test={owner}"),
                "channels=2".into(),
                "channel_map=front-left,front-right".into(),
                "master_channel_map=front-left,front-right".into(),
            ])
            .await?,
        );
        let mut cfg = AppConfig::default();
        cfg.microphone.playback_device = mic.clone();
        cfg.speaker.playback_device = speaker.clone();
        let (_selection, usage) = watch::channel(audio::activity::EndpointUse {
            microphone: true,
            speaker: true,
            speaker_selected: true,
            ..Default::default()
        });
        for (origin, destination, observer) in [
            (TranscriptOrigin::Microphone, mic.clone(), source.clone()),
            (
                TranscriptOrigin::Speaker,
                speaker.clone(),
                format!("{speaker}.monitor"),
            ),
        ] {
            let stop = CancellationToken::new();
            let _guard = stop.clone().drop_guard();
            let (frames, mut received) = mpsc::channel(300);
            let captured = tokio_util::task::AbortOnDropHandle::new(tokio::spawn({
                let stop = stop.clone();
                async move {
                    audio::capture(
                        &observer,
                        AudioOptions {
                            sample_rate: 48_000,
                            channels: 2,
                            frame_ms: 20,
                            latency_ms: 30,
                            queue_ms: 200,
                        },
                        frames,
                        stop,
                        Arc::default(),
                    )
                    .await
                }
            }));
            tokio::time::sleep(Duration::from_millis(300)).await;
            let samples = (0..OUTPUT_RATE)
                .map(|index| {
                    (12_000.0
                        * (std::f64::consts::TAU * 440.0 * f64::from(index)
                            / f64::from(OUTPUT_RATE))
                        .sin()) as i16
                })
                .collect();
            let (_device, playback) = watch::channel(destination);
            let metrics = Arc::new(RouteMetrics::default());
            tokio::time::timeout(
                Duration::from_secs(8),
                play_segment(
                    &cfg,
                    &cfg.microphone,
                    origin,
                    &metrics,
                    &playback,
                    None,
                    usage.clone(),
                    vec![ProviderEvent::Audio {
                        samples,
                        sample_rate: OUTPUT_RATE,
                    }],
                    false,
                ),
            )
            .await??;
            tokio::time::sleep(Duration::from_millis(200)).await;
            stop.cancel();
            tokio::time::timeout(Duration::from_secs(3), captured).await???;
            let mut peak_rms = [0.0_f32; 2];
            while let Ok(frame) = received.try_recv() {
                for (channel, peak) in peak_rms.iter_mut().enumerate() {
                    let samples: Vec<_> = frame.samples.iter().skip(channel).step_by(2).collect();
                    let rms = (samples
                        .iter()
                        .map(|sample| **sample * **sample)
                        .sum::<f32>()
                        / samples.len() as f32)
                        .sqrt();
                    *peak = peak.max(rms);
                }
            }
            ensure!(
                peak_rms.iter().all(|rms| *rms > 0.03),
                "Translated PCM did not reach both channels of {origin:?}: {peak_rms:?}"
            );
            let status = metrics.snapshot();
            ensure!(
                status.translated_samples == u64::from(OUTPUT_RATE)
                    && status.dropped_frames == 0
                    && status.device_error.is_none(),
                "Translated playback did not complete cleanly"
            );
        }
        Ok(())
    }
    .await;
    // Remove only modules that still carry this test's unique ownership token.
    for id in modules.iter().rev() {
        let current = pactl(&["list".into(), "short".into(), "modules".into()]).await?;
        let row = current
            .lines()
            .find(|row| row.split('\t').next() == Some(id));
        if let Some(row) = row {
            ensure!(
                row.contains(&format!("babel.test={owner}")),
                "Test module ownership changed"
            );
            pactl(&["unload-module".into(), id.clone()]).await?;
        }
    }
    let after = audio::devices().await?;
    for device in before
        .iter()
        .filter(|device| device.id.starts_with("babel_"))
    {
        ensure!(
            after.iter().any(|current| current.id == device.id),
            "Existing Babel device changed"
        );
    }
    result
}
