#![cfg(target_os = "linux")]
//! Explicitly invoked end-to-end test. Never captures or plays physical devices.
use anyhow::{Context, Result, ensure};
use babel_audio::{
    audio::{self, AudioOptions, AudioStats, PlaybackCommand},
    config::AppConfig,
    engine::Controller,
};
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{process::Command, sync::mpsc};
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a live PipeWire/PulseAudio session; creates/removes Babel virtual devices"]
async fn synthetic_original_audio_crosses_controller_and_virtual_devices() -> Result<()> {
    ensure!(
        !audio::devices()
            .await?
            .iter()
            .any(|d| d.id.starts_with("babel_")),
        "Run this only when Babel devices are absent; it must not affect active sessions"
    );
    let directory = tempfile::tempdir()?;
    let mut cfg = AppConfig::default();
    cfg.microphone.enabled = false;
    cfg.speaker.enabled = false;
    cfg.transcription.enabled = false;
    cfg.recording.enabled = true;
    cfg.recording.microphone = false;
    cfg.recording.directory = directory.path().join("recordings").to_str().unwrap().into();
    cfg.speaker.capture_device = "babel_speaker.monitor".into();
    cfg.speaker.playback_device = "babel_mic_bus".into();
    let controller = Controller::new(cfg, directory.path().join("test.toml")).unwrap();
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let mut capture = None;
    let mut playback = None;
    let mut external_client = None;
    audio::install_virtual_devices().await?;
    // Keep every fallible assertion inside Result so cleanup also runs when the
    // input process or tone check fails partway through the test.
    let result: Result<()> = async {
        // Real external stream ownership activates the selected virtual speaker.
        // Babel's synthetic helpers are deliberately excluded by the activity gate.
        external_client = Some(
            Command::new("pacat")
                .args([
                    "--playback",
                    "--raw",
                    "--format=s16le",
                    "--channels=1",
                    "--rate=16000",
                    "--device=babel_speaker",
                    "--client-name=Babel external regression",
                ])
                .stdin(Stdio::from(std::fs::File::open("/dev/zero")?))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()?,
        );
        controller
            .start_named(Some("Teste integração / sessão nomeada".into()))
            .await?;
        let (capture_tx, mut capture_rx) = mpsc::channel(100);
        let options = AudioOptions {
            sample_rate: 24000,
            frame_ms: 20,
            latency_ms: 30,
            queue_ms: 200,
        };
        let capture_cancel = cancel.clone();
        capture = Some(tokio::spawn(async move {
            audio::capture(
                "babel_microphone",
                options,
                capture_tx,
                capture_cancel,
                Arc::new(AudioStats::default()),
            )
            .await
        }));
        let (play_tx, play_rx) = mpsc::channel(100);
        let play_cancel = cancel.clone();
        playback = Some(tokio::spawn(async move {
            audio::playback(
                "babel_speaker",
                AudioOptions {
                    sample_rate: 16000,
                    ..options
                },
                play_rx,
                play_cancel,
                Arc::new(AudioStats::default()),
            )
            .await
        }));
        tokio::time::sleep(Duration::from_millis(200)).await;
        for frame in 0..100 {
            let samples = (0..320)
                .map(|i| {
                    (((frame * 320 + i) as f64 * std::f64::consts::TAU * 440.0 / 16000.0).sin()
                        * 6000.0) as i16
                })
                .collect();
            play_tx
                .send(PlaybackCommand::Audio {
                    samples,
                    generation: 0,
                })
                .await?;
        }
        let received = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(frame) = capture_rx.recv().await {
                if frame
                    .samples
                    .iter()
                    .any(|sample| sample.unsigned_abs() > 1000)
                {
                    return true;
                }
            }
            false
        })
        .await
        .context("Timed out waiting for synthetic audio through the controller")?;
        let status = controller.status().await;
        ensure!(
            status.session_name.as_deref() == Some("Teste integração / sessão nomeada"),
            "Session name did not reach engine status"
        );
        ensure!(
            status
                .session_id
                .as_deref()
                .is_some_and(|id| id.ends_with("teste-integração-sessão-nomeada")),
            "Session identifier did not retain a safe name"
        );
        ensure!(
            received,
            "No synthetic tone crossed the original-audio route"
        );
        ensure!(
            status.speaker.captured_frames > 0,
            "Controller did not capture audio"
        );
        ensure!(
            status.speaker.translated_samples == 0,
            "Original routing unexpectedly produced translated audio"
        );
        Ok(())
    }
    .await;
    // Do not return early from cleanup: always stop both helper streams and
    // remove the newly created modules even if another cleanup step fails.
    let stopped = controller.shutdown().await;
    cancel.cancel();
    let external_stopped = match external_client {
        Some(mut child) => child.kill().await,
        None => Ok(()),
    };
    let captured = match capture {
        Some(task) => task
            .await
            .context("Capture helper panicked")
            .and_then(|r| r),
        None => Ok(()),
    };
    let played = match playback {
        Some(task) => task
            .await
            .context("Playback helper panicked")
            .and_then(|r| r),
        None => Ok(()),
    };
    let removed = audio::uninstall_virtual_devices().await;
    result?;
    stopped?;
    captured?;
    played?;
    external_stopped?;
    removed?;
    let recordings = std::fs::read_dir(directory.path().join("recordings"))?
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure!(recordings.len() == 1, "Expected one original recording");
    let mut wav = hound::WavReader::open(recordings[0].path())?;
    ensure!(
        wav.samples::<i16>()
            .any(|sample| sample.is_ok_and(|sample| sample.unsigned_abs() > 1000)),
        "The original recording contains no synthetic tone"
    );
    let status = controller.status().await;
    ensure!(!status.running, "Controller remained running after stop");
    ensure!(
        status.session_name.as_deref() == Some("Teste integração / sessão nomeada"),
        "Session name was lost when stopping"
    );
    ensure!(
        status.last_error.is_none(),
        "Controller reported an error during normal stop: {status:?}"
    );
    ensure!(
        !audio::devices()
            .await?
            .iter()
            .any(|d| d.id.starts_with("babel_")),
        "Virtual device cleanup left Babel endpoints"
    );
    Ok(())
}
