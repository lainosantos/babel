//! Session-owned translation drains retained originals independently of capture.
use super::*;
use std::{
    collections::HashMap,
    sync::{OnceLock, Weak},
};

type OutputGates = StdMutex<HashMap<String, Weak<Mutex<()>>>>;
static OUTPUTS: OnceLock<OutputGates> = OnceLock::new();

fn output_gate(device: &str) -> Arc<Mutex<()>> {
    let mut gates = OUTPUTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    gates.retain(|_, gate| gate.strong_count() > 0);
    if let Some(gate) = gates.get(device).and_then(Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(Mutex::new(()));
    gates.insert(device.to_owned(), Arc::downgrade(&gate));
    gate
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    cfg: AppConfig,
    route: RouteConfig,
    origin: TranscriptOrigin,
    retained: Arc<retained::RetainedSession>,
    metrics: Arc<RouteMetrics>,
    playback: watch::Receiver<String>,
    mirror_metrics: Option<Arc<RouteMetrics>>,
    mut usage: watch::Receiver<audio::activity::EndpointUse>,
) -> Result<()> {
    let mut reader = retained.reader(origin);
    let Some(first) = reader.next().await? else {
        return Ok(());
    };
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let (_microphone, mic_device) = watch::channel(cfg.microphone.playback_device.clone());
    let initial = usage.borrow_and_update().clone();
    let selected_epoch = playback_selection(&initial, origin)?;
    let profile = cfg.profile(&route.provider).clone();
    let provider =
        provider::create_configured_provider(&route.provider, &profile, &cfg.providers.local)?;
    let settings = SessionConfig {
        model: profile.model.clone(),
        api_key_env: profile.api_key_env.clone(),
        voice: if route.provider == "local" {
            route.resolved_voice.clone()
        } else {
            String::new()
        },
        source_language: route.source_language.clone(),
        target_language: route.target_language.clone(),
        prompt: route.prompt.clone(),
        vad_silence_ms: cfg.audio.quality.vad_silence_ms(),
        connect_timeout_secs: profile.connect_timeout_secs,
        max_reconnect_attempts: profile.max_reconnect_attempts,
        input_transcription: false,
        output_transcription: false,
    };
    let (input, received) = mpsc::channel(8);
    let (events, mut output) = mpsc::channel(16);
    let (play, played) =
        mpsc::channel((cfg.audio.playback_queue_ms / OUTPUT_FRAME_MS).max(1) as usize);
    let mirror_output = origin == TranscriptOrigin::Speaker
        && cfg.audio.microphone_source == crate::config::MicrophoneSource::SpeakerOutput;
    let (mirror_send, mirror_received) = mpsc::channel(8);
    let mirror_send = mirror_output.then_some(mirror_send);
    let playback_stats = Arc::new(AudioStats {
        translated_playback: true,
        playback_mirror: (!mirror_output)
            .then(|| metrics.audio.playback_mirror.clone())
            .flatten(),
        ..Default::default()
    });
    let mirror_stats = Arc::new(AudioStats {
        translated_playback: true,
        ..Default::default()
    });
    if mirror_output && let Some(metrics) = &mirror_metrics {
        *metrics
            .translated_audio
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(mirror_stats.clone());
    }
    *metrics
        .translated_audio
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(playback_stats.clone());
    let feed = async {
        input
            .send(first.samples)
            .await
            .context("Translator closed before consuming original audio")?;
        while let Some(frame) = reader.next().await? {
            input
                .send(frame.samples)
                .await
                .context("Translator closed before consuming original audio")?;
        }
        drop(input);
        Ok::<_, anyhow::Error>(())
    };
    let collect = async {
        while let Some(event) = output.recv().await {
            match event {
                ProviderEvent::Connected => metrics.state("running"),
                ProviderEvent::Audio {
                    mut samples,
                    sample_rate,
                } => {
                    ensure!(
                        sample_rate == OUTPUT_RATE,
                        "Translator returned unsupported audio sample rate"
                    );
                    ensure!(
                        samples.len() <= OUTPUT_RATE as usize,
                        "Translator audio chunk exceeds one second"
                    );
                    apply_gain(&mut samples, route.gain);
                    metrics
                        .output_level
                        .store(rms(&samples).to_bits(), Ordering::Relaxed);
                    metrics
                        .translated_samples
                        .fetch_add(samples.len() as u64, Ordering::Relaxed);
                    let generation = playback_stats.playback_generation.load(Ordering::Acquire);
                    for samples in samples.chunks(OUTPUT_FRAME_SAMPLES) {
                        // Backpressure belongs exclusively to translation. Source
                        // capture continues into RAM/encrypted retention meanwhile.
                        play.send(PlaybackCommand::Audio {
                            samples: samples.to_vec(),
                            generation,
                        })
                        .await
                        .context("Translated playback closed before completion")?;
                        if let Some(mirror) = &mirror_send {
                            mirror
                                .send(PlaybackCommand::Audio {
                                    samples: samples.to_vec(),
                                    generation: 0,
                                })
                                .await
                                .context("Translated microphone mirror closed before completion")?;
                        }
                    }
                }
                ProviderEvent::Warning { message } => metrics.report_processing_error(&message),
                ProviderEvent::Interrupted | ProviderEvent::Reconnecting { .. } => {
                    bail!(
                        "Translation was interrupted; original audio remains retained for recovery"
                    );
                }
                _ => {}
            }
        }
        drop(play);
        drop(mirror_send);
        Ok::<_, anyhow::Error>(())
    };
    let destination = playback.borrow().clone();
    // Gate by logical route, not the initial physical device: live switching
    // must not let an older session bypass a newer session's output ownership.
    let microphone_gate = (origin == TranscriptOrigin::Microphone || mirror_output)
        .then(|| output_gate("microphone"));
    let speaker_gate = (origin == TranscriptOrigin::Speaker).then(|| output_gate("speaker"));
    let options = AudioOptions {
        sample_rate: OUTPUT_RATE,
        channels: 1,
        frame_ms: OUTPUT_FRAME_MS,
        latency_ms: cfg.audio.device_latency_ms,
        queue_ms: cfg.audio.playback_queue_ms,
    };
    let playback_task = async {
        // Sessions may infer concurrently, but two translated streams targeting
        // the same endpoint must not talk over each other at a session boundary.
        // Always acquire microphone before speaker when one translation uses
        // both destinations; another session cannot deadlock or overlap them.
        let _microphone = match microphone_gate {
            Some(gate) => Some(gate.lock_owned().await),
            None => None,
        };
        let _speaker = match speaker_gate {
            Some(gate) => Some(gate.lock_owned().await),
            None => None,
        };
        let primary = playback_worker(
            destination,
            options,
            played,
            playback,
            cancel.child_token(),
            playback_stats.clone(),
        );
        let mirror = async {
            if !mirror_output {
                return Ok(());
            }
            selected_mirror(
                mirror_received,
                usage.clone(),
                mirror_stats.clone(),
                |received, stopped| {
                    playback_worker(
                        cfg.microphone.playback_device.clone(),
                        options,
                        received,
                        mic_device.clone(),
                        stopped,
                        mirror_stats.clone(),
                    )
                },
            )
            .await
        };
        tokio::try_join!(primary, mirror)?;
        Ok::<_, anyhow::Error>(())
    };
    let completion = async {
        tokio::try_join!(
            provider.run(settings, received, events, cancel.clone()),
            feed,
            collect,
            playback_task
        )?;
        Ok::<_, anyhow::Error>(())
    };
    complete_selected(
        usage.clone(),
        origin,
        selected_epoch,
        &cancel,
        [&playback_stats, &mirror_stats],
        completion,
    )
    .await?;
    ensure!(
        playback_stats.dropped_frames.load(Ordering::Acquire) == 0,
        "Translated playback was interrupted; original audio remains retained for recovery"
    );
    ensure!(
        mirror_stats.dropped_frames.load(Ordering::Acquire) == 0,
        "Translated microphone mirror was interrupted; original audio remains retained for recovery"
    );
    Ok(())
}

async fn complete_selected<F>(
    usage: watch::Receiver<audio::activity::EndpointUse>,
    origin: TranscriptOrigin,
    selected_epoch: u64,
    cancel: &CancellationToken,
    stats: [&AudioStats; 2],
    completion: F,
) -> Result<()>
where
    F: Future<Output = Result<()>>,
{
    tokio::select! {
        biased;
        error = revoked_selection(usage, origin, selected_epoch) => {
            for stats in stats { stats.playback_generation.fetch_add(1, Ordering::AcqRel); }
            cancel.cancel();
            Err(error)
        }
        result = completion => result,
    }
}

fn playback_selection(
    usage: &audio::activity::EndpointUse,
    origin: TranscriptOrigin,
) -> Result<u64> {
    let (active, epoch, error) = match origin {
        TranscriptOrigin::Microphone => (
            usage.microphone,
            usage.microphone_epoch,
            &usage.microphone_error,
        ),
        TranscriptOrigin::Speaker => (
            usage.speaker_selected,
            usage.speaker_selection_epoch,
            &usage.speaker_error,
        ),
    };
    ensure!(
        usage.error.is_none() && error.is_none(),
        "Translation device usage could not be verified; original audio remains retained"
    );
    ensure!(
        active,
        "Translation device is no longer selected or in use; original audio remains retained"
    );
    Ok(epoch)
}

/// Capture Stop and speaker inactivity do not revoke an unchanged output
/// selection. Deselection or an intervening epoch fences late provider results.
async fn revoked_selection(
    mut usage: watch::Receiver<audio::activity::EndpointUse>,
    origin: TranscriptOrigin,
    selected_epoch: u64,
) -> anyhow::Error {
    loop {
        if usage.changed().await.is_err() {
            return anyhow!("Translation device usage monitor closed before completion");
        }
        match playback_selection(&usage.borrow_and_update(), origin) {
            Err(error) => return error,
            Ok(epoch) if epoch != selected_epoch => {
                return anyhow!(
                    "Translation device selection changed before accepted playback completed; original audio remains retained"
                );
            }
            Ok(_) => {}
        }
    }
}

async fn playback_worker(
    destination: String,
    options: AudioOptions,
    received: mpsc::Receiver<PlaybackCommand>,
    playback: watch::Receiver<String>,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
) -> Result<()> {
    let _cancel_on_drop = cancel.clone().drop_guard();
    tokio_util::task::AbortOnDropHandle::new(crate::execution::audio_handle()?.spawn(async move {
        audio::switching::playback(&destination, options, received, playback, cancel, stats).await
    }))
    .await
    .context("Translated playback worker interrupted")?
}

fn microphone_selected(usage: &audio::activity::EndpointUse) -> bool {
    usage.microphone && usage.error.is_none() && usage.microphone_error.is_none()
}

/// Unselected microphones consume no device resources and receive no audio.
/// Once selected playback accepts audio, source EOF drains its complete tail.
async fn selected_mirror<F, Fut>(
    mut received: mpsc::Receiver<PlaybackCommand>,
    mut usage: watch::Receiver<audio::activity::EndpointUse>,
    stats: Arc<AudioStats>,
    mut play: F,
) -> Result<()>
where
    F: FnMut(mpsc::Receiver<PlaybackCommand>, CancellationToken) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let (first, epoch) = loop {
        let Some(command) = received.recv().await else {
            return Ok(());
        };
        let current = usage.borrow_and_update();
        ensure!(
            current.error.is_none() && current.microphone_error.is_none(),
            "Translated microphone usage could not be verified"
        );
        if microphone_selected(&current) {
            break (command, current.microphone_epoch);
        }
    };
    let cancel = CancellationToken::new();
    let _guard = cancel.clone().drop_guard();
    let (sender, input) = mpsc::channel(2);
    let mut sender = Some(sender);
    let playback = play(input, cancel.clone());
    tokio::pin!(playback);
    let mut pending = Some(first);
    loop {
        tokio::select! {
            biased;
            changed = usage.changed() => {
                changed.context("Translated microphone usage monitor closed")?;
                let current = usage.borrow_and_update().clone();
                if !microphone_selected(&current) || current.microphone_epoch != epoch {
                    cancel.cancel();
                    let _ = tokio::time::timeout(Duration::from_secs(2), &mut playback).await;
                    bail!("Translated microphone selection ended before accepted playback completed");
                }
            }
            result = &mut playback => {
                result?;
                ensure!(sender.is_none(), "Translated microphone playback ended before EOF");
                return Ok(());
            }
            permit = async { sender.as_ref().unwrap().clone().reserve_owned().await }, if pending.is_some() => {
                let permit = permit.context("Translated microphone playback queue closed")?;
                let mut command = pending.take().unwrap();
                if let PlaybackCommand::Audio { generation, .. } = &mut command {
                    *generation = stats.playback_generation.load(Ordering::Acquire);
                }
                permit.send(command);
            }
            command = received.recv(), if sender.is_some() && pending.is_none() => {
                match command {
                    Some(command) => pending = Some(command),
                    None => { sender.take(); }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn capture_stop_preserves_selected_translation_completion() {
        let (selection, usage) = watch::channel(audio::activity::EndpointUse {
            microphone: true,
            speaker: true,
            speaker_selected: true,
            ..Default::default()
        });
        let (finish, finished) = tokio::sync::oneshot::channel();
        let playback_cancel = CancellationToken::new();
        let stopped = playback_cancel.clone();
        let task = tokio::spawn(async move {
            let stats = AudioStats::default();
            complete_selected(
                usage,
                TranscriptOrigin::Speaker,
                0,
                &playback_cancel,
                [&stats, &stats],
                async {
                    finished
                        .await
                        .context("test output completion interrupted")?;
                    Ok(())
                },
            )
            .await
        });
        // Capture ownership closes independently; an unrelated microphone
        // change also cannot revoke the still-selected speaker's accepted tail.
        let capture = CancellationToken::new();
        capture.cancel();
        selection.send_modify(|usage| {
            usage.microphone = false;
            usage.microphone_epoch += 1;
            // Natural app EOF pauses capture while Babel remains the chosen
            // output. Already accepted translated speech still finishes.
            usage.speaker = false;
            usage.speaker_epoch += 1;
        });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        assert!(!stopped.is_cancelled());
        finish.send(()).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn primary_selection_revocation_fences_late_output_for_both_directions() {
        for origin in [TranscriptOrigin::Microphone, TranscriptOrigin::Speaker] {
            for coalesced_reselection in [false, true] {
                let (selection, usage) = watch::channel(audio::activity::EndpointUse {
                    microphone: true,
                    speaker: true,
                    speaker_selected: true,
                    ..Default::default()
                });
                let stats = AudioStats::default();
                let cancel = CancellationToken::new();
                let output = Arc::new(AtomicBool::new(false));
                let wrote = output.clone();
                selection.send_modify(|usage| match origin {
                    TranscriptOrigin::Microphone => {
                        usage.microphone = coalesced_reselection;
                        usage.microphone_epoch += 1;
                    }
                    TranscriptOrigin::Speaker => {
                        usage.speaker = coalesced_reselection;
                        usage.speaker_epoch += 1;
                        usage.speaker_selected = coalesced_reselection;
                        usage.speaker_selection_epoch += 1;
                    }
                });
                let result =
                    complete_selected(usage, origin, 0, &cancel, [&stats, &stats], async move {
                        wrote.store(true, Ordering::Release);
                        Ok(())
                    })
                    .await;
                assert!(result.is_err());
                assert!(cancel.is_cancelled());
                assert!(
                    !output.load(Ordering::Acquire),
                    "Revocation must win over a ready late provider result"
                );
                assert!(stats.playback_generation.load(Ordering::Acquire) > 0);
            }
        }
    }

    #[tokio::test]
    async fn selected_microphone_drains_all_translation_after_speaker_capture_ends() {
        let (_usage, usage) = watch::channel(audio::activity::EndpointUse {
            microphone: true,
            speaker: false,
            ..Default::default()
        });
        let stats = Arc::new(AudioStats::default());
        stats.playback_generation.store(7, Ordering::Release);
        let (sender, received) = mpsc::channel(4);
        let (eof, observed) = tokio::sync::oneshot::channel();
        let (finish, finished) = tokio::sync::oneshot::channel();
        let mut signals = Some((eof, finished));
        let task = tokio::spawn(selected_mirror(
            received,
            usage,
            stats,
            move |mut input, _| {
                let (eof, finished) = signals.take().expect("one selected playback worker");
                async move {
                    let mut played = Vec::new();
                    while let Some(command) = input.recv().await {
                        let PlaybackCommand::Audio {
                            samples,
                            generation,
                        } = command
                        else {
                            panic!("audio expected");
                        };
                        assert_eq!(generation, 7);
                        played.extend(samples);
                    }
                    eof.send(played).unwrap();
                    finished.await.context("test playback drain interrupted")?;
                    Ok(())
                }
            },
        ));
        for value in [1, 2, 3] {
            sender
                .send(PlaybackCommand::Audio {
                    samples: vec![value; 17],
                    generation: 0,
                })
                .await
                .unwrap();
        }
        drop(sender);
        assert_eq!(
            observed.await.unwrap(),
            [vec![1; 17], vec![2; 17], vec![3; 17]].concat()
        );
        assert!(
            !task.is_finished(),
            "Input EOF must wait for the complete device tail"
        );
        finish.send(()).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unused_microphone_never_opens_playback() {
        let (_usage, usage) = watch::channel(audio::activity::EndpointUse::default());
        let (sender, received) = mpsc::channel(1);
        sender
            .send(PlaybackCommand::Audio {
                samples: vec![1; 17],
                generation: 0,
            })
            .await
            .unwrap();
        drop(sender);
        selected_mirror(received, usage, Arc::default(), |_, _| async {
            panic!("An unused virtual microphone must not open playback");
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn selection_revocation_during_mirrored_eof_drain_is_not_success() {
        let (usage, observed_usage) = watch::channel(audio::activity::EndpointUse {
            microphone: true,
            ..Default::default()
        });
        let (sender, received) = mpsc::channel(1);
        let (eof, observed) = tokio::sync::oneshot::channel();
        let mut eof = Some(eof);
        let task = tokio::spawn(selected_mirror(
            received,
            observed_usage,
            Arc::default(),
            move |mut input, cancel| {
                let eof = eof.take().unwrap();
                async move {
                    while input.recv().await.is_some() {}
                    eof.send(()).unwrap();
                    cancel.cancelled().await;
                    Ok(())
                }
            },
        ));
        sender
            .send(PlaybackCommand::Audio {
                samples: vec![1; 17],
                generation: 0,
            })
            .await
            .unwrap();
        drop(sender);
        observed.await.unwrap();
        usage.send_modify(|usage| {
            usage.microphone = false;
            usage.microphone_epoch += 1;
        });
        assert!(
            task.await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("before accepted playback completed")
        );
    }
}
