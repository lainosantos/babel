//! Session processing is separate from device capture and original playback.
use super::*;
use futures_util::FutureExt;

pub(super) async fn run_route(
    name: &'static str,
    cfg: AppConfig,
    route: RouteConfig,
    metrics: Arc<RouteMetrics>,
    cancel: CancellationToken,
    io: RouteIo,
) -> Result<()> {
    let RouteIo {
        transcript: mut transcript_tx,
        audio: mut audio_tx,
        origin,
        mut capture_changes,
        mut playback_changes,
        history,
        session_origin,
    } = io;
    if capture_changes.borrow().is_empty() || playback_changes.borrow().is_empty() {
        metrics.state("unconfigured");
        if !wait_for_devices(&mut capture_changes, &mut playback_changes, &cancel).await {
            return Ok(());
        }
    }
    let translating = route.enabled;
    metrics.original_mode.store(!translating, Ordering::Relaxed);
    let mut transcribing = transcript_tx.is_some();
    let audio_handle = crate::execution::audio_handle()?;
    let processing_handle = crate::execution::processing_handle()?;
    let capture_device = capture_changes.borrow().clone();
    let playback_device = playback_changes.borrow().clone();
    let (capture_rate, capture_channels) = if translating {
        // The switching worker keeps retrying an unavailable physical device.
        // A speech-only fallback format must not turn disconnection into a fatal
        // session error. Original passthrough negotiates/retries independently.
        audio::original_format(&capture_device, &playback_device)
            .await
            .unwrap_or((INPUT_RATE, 1))
    } else {
        (48_000, 2)
    };
    let frame_ms = [10, 20, 40, 100]
        .into_iter()
        .find(|ms| (capture_rate * ms) % 1000 == 0)
        .unwrap_or(100);
    let recognition_cancel = cancel.child_token();
    let playback_rate = if translating {
        OUTPUT_RATE
    } else {
        capture_rate
    };
    let playback_queue_ms = if translating {
        cfg.audio.playback_queue_ms
    } else {
        80
    };
    let (captured_tx, device_rx) = mpsc::channel((80 / frame_ms).max(1) as usize);
    let (sidecar_tx, mut captured_rx) =
        mpsc::channel((cfg.audio.capture_queue_ms / frame_ms).clamp(1, 100) as usize);
    let (input_tx, input_rx) =
        mpsc::channel((cfg.audio.capture_queue_ms / frame_ms).max(1) as usize);
    let (events_tx, mut events_rx) = mpsc::channel(16);
    let (stt_input_tx, stt_input_rx) =
        mpsc::channel((cfg.audio.capture_queue_ms / frame_ms).max(1) as usize);
    let (stt_events_tx, mut stt_events_rx) = mpsc::channel(16);
    let (play_tx, play_rx) = mpsc::channel(
        (playback_queue_ms
            / if translating {
                OUTPUT_FRAME_MS
            } else {
                frame_ms
            })
        .max(1) as usize,
    );
    let mut audio_jobs = JoinSet::new();
    let mut processing_jobs = JoinSet::new();
    let capture_options = AudioOptions {
        sample_rate: capture_rate,
        channels: capture_channels,
        frame_ms,
        latency_ms: cfg.audio.device_latency_ms,
        queue_ms: 80.max(frame_ms),
    };
    let playback_options = AudioOptions {
        sample_rate: playback_rate,
        channels: if translating { 1 } else { capture_channels },
        frame_ms: if translating {
            OUTPUT_FRAME_MS
        } else {
            frame_ms
        },
        latency_ms: cfg.audio.device_latency_ms,
        queue_ms: playback_queue_ms,
    };
    if translating {
        let capture_cancel = cancel.clone();
        let capture_stats = metrics.audio.clone();
        audio_jobs.spawn_on(
            async move {
                audio::switching::capture(
                    &capture_device,
                    capture_options,
                    captured_tx,
                    capture_changes,
                    capture_cancel,
                    capture_stats,
                )
                .await
                .context("Capture")
            },
            &audio_handle,
        );
        let playback_cancel = cancel.clone();
        let playback_stats = metrics.audio.clone();
        audio_jobs.spawn_on(
            async move {
                audio::switching::playback(
                    &playback_device,
                    playback_options,
                    play_rx,
                    playback_changes,
                    playback_cancel,
                    playback_stats,
                )
                .await
                .context("Playback")
            },
            &audio_handle,
        );
        // The model necessarily owns translated output; capture remains on the audio executor.
        let forward_cancel = cancel.clone();
        let forward_stats = metrics.audio.clone();
        audio_jobs.spawn_on(
            async move {
                forward_processing(device_rx, sidecar_tx, forward_cancel, forward_stats).await
            },
            &audio_handle,
        );
    } else {
        drop(captured_tx);
        drop(device_rx);
        drop(play_rx);
        let bridge_cancel = cancel.clone();
        let bridge_stats = metrics.audio.clone();
        audio_jobs.spawn_on(
            async move {
                audio::passthrough::run_route_with_sidecar(
                    audio::passthrough::RouteDevices {
                        capture: capture_changes,
                        playback: playback_changes,
                    },
                    capture_options,
                    bridge_cancel,
                    bridge_stats,
                    sidecar_tx,
                )
                .await
            },
            &audio_handle,
        );
    }
    if translating {
        let provider_cancel = cancel.clone();
        let cloud = cfg.profile(&route.provider).clone();
        let native_voice = if route.provider == "local" {
            if route.voice.engine == "native" && !route.voice.voice_id.is_empty() {
                route.voice.voice_id.clone()
            } else {
                cfg.providers.local.piper_voice.clone()
            }
        } else if route.voice.engine == "native" && !route.voice.voice_id.is_empty() {
            route.voice.voice_id.clone()
        } else {
            cloud.voice.clone()
        };
        let session_config = SessionConfig {
            model: cloud.model.clone(),
            api_key_env: cloud.api_key_env.clone(),
            voice: native_voice,
            source_language: route.source_language.clone(),
            target_language: route.target_language.clone(),
            prompt: route.prompt.clone(),
            vad_silence_ms: cfg.audio.quality.vad_silence_ms(),
            connect_timeout_secs: cloud.connect_timeout_secs,
            max_reconnect_attempts: cloud.max_reconnect_attempts,
            input_transcription: false,
            output_transcription: translating && route.voice.engine != "native",
        };
        let provider_kind = route.provider.clone();
        let local_config = cfg.providers.local.clone();
        let synthesis = if translating && route.voice.engine != "native" {
            let voice_provider = cfg.profile(&route.voice.engine);
            Some(crate::voices::SynthesisConfig {
                provider: route.voice.engine.clone(),
                model: voice_provider.tts_model.clone(),
                api_key_env: voice_provider.api_key_env.clone(),
                voice_id: route.voice.voice_id.clone(),
                style: route.voice.style.clone(),
                language: route.target_language.clone(),
            })
        } else {
            None
        };
        let chunk_ms = route.voice.chunk_ms;
        let queue_ms = cfg.audio.playback_queue_ms;
        spawn_processor(
            &mut processing_jobs,
            &processing_handle,
            Processor::Translation,
            async move {
                let provider = provider::create_route_provider(
                    &provider_kind,
                    &cloud,
                    &local_config,
                    synthesis.is_none(),
                )?;
                if let Some(synthesis) = synthesis {
                    crate::revoice::run(
                        provider,
                        session_config,
                        synthesis,
                        chunk_ms,
                        queue_ms,
                        input_rx,
                        events_tx,
                        provider_cancel,
                    )
                    .await
                    .context("Translation + synthesis")
                } else {
                    provider
                        .run(session_config, input_rx, events_tx, provider_cancel)
                        .await
                        .context("Provider")
                }
            },
        );
    }
    if transcribing {
        let recognition = match origin {
            TranscriptOrigin::Microphone => cfg.transcription.microphone_recognition.clone(),
            TranscriptOrigin::Speaker => cfg.transcription.speaker_recognition.clone(),
        };
        let profiles = cfg.transcription.providers.clone();
        let stt_cancel = recognition_cancel.clone();
        spawn_processor(
            &mut processing_jobs,
            &processing_handle,
            Processor::Recognition,
            async move {
                let recognizer = provider::stt::create(&recognition, &profiles)?;
                let session_config = provider::stt::session_config(&recognition, &profiles)?;
                recognizer
                    .run(session_config, stt_input_rx, stt_events_tx, stt_cancel)
                    .await
                    .context("STT transcription")
            },
        );
    }
    if !translating {
        metrics.state(if transcribing {
            "connecting"
        } else {
            "passthrough"
        });
    }
    let mut connected = false;
    let mut stt_connected = false;
    let mut stt_offset_ms = None;
    let mut speech = audio::speech::SpeechTap::new();
    let mut copy_losses = metrics.audio.sidecar_dropped_frames.load(Ordering::Relaxed);
    let result: Result<()> = async {
        loop {
            if cancel.is_cancelled() { break Ok(()); }
            tokio::select! {
                _ = cancel.cancelled() => break Ok(()),
                completed = audio_jobs.join_next() => {
                    if cancel.is_cancelled() { break Ok(()); }
                    match completed {
                        Some(Ok(Err(error))) => break Err(error),
                        _ => bail!("An audio component ended unexpectedly"),
                    }
                }
                completed = processing_jobs.join_next(), if !processing_jobs.is_empty() => {
                    if cancel.is_cancelled() { break Ok(()); }
                    match completed {
                        Some(Ok((Processor::Recognition, result))) => {
                            transcribing = false; stt_connected = false;
                            recognition_cancel.cancel();
                            metrics.report_processing_error(&format!("Transcription interrupted: {}",
                                result.err().map_or_else(|| "the recognizer stopped".into(), |error| format!("{error:#}"))));
                            transcript_tx = None;
                            if !translating { metrics.state("passthrough"); }
                        }
                        Some(Ok((Processor::Translation, Err(error)))) => break Err(error),
                        Some(Ok((Processor::Translation, Ok(())))) => bail!("The translator ended unexpectedly"),
                        Some(Err(error)) => break Err(error.into()),
                        None => {}
                    }
                }
                event = events_rx.recv(), if translating => {
                    if cancel.is_cancelled() { break Ok(()); }
                    let Some(event) = event else {
                        break translation_result_after_eof(&mut processing_jobs, &cancel, &metrics).await;
                    };
                    match event {
                        ProviderEvent::Warning { message } => metrics.report_processing_error(&message),
                        ProviderEvent::Connected => { connected = true; metrics.state("running"); }
                        ProviderEvent::Reconnecting { .. } => {
                            connected = false;
                            metrics.state("reconnecting");
                            metrics.reconnects.fetch_add(1, Ordering::Relaxed);
                            interrupt(&metrics, &play_tx);
                        }
                        ProviderEvent::Interrupted => { interrupt(&metrics, &play_tx); }
                        ProviderEvent::Audio { mut samples, sample_rate } => {
                            ensure!(sample_rate == OUTPUT_RATE, "Provider returned an unsupported audio sample rate: {sample_rate}");
                            ensure!(samples.len() <= OUTPUT_RATE as usize, "Provider audio chunk exceeds 1 second");
                            apply_gain(&mut samples, route.gain);
                            metrics.output_level.store(rms(&samples).to_bits(), Ordering::Relaxed);
                            metrics.translated_samples.fetch_add(samples.len() as u64, Ordering::Relaxed);
                            let generation = metrics.audio.playback_generation.load(Ordering::Acquire);
                            for chunk in samples.chunks(OUTPUT_FRAME_SAMPLES) {
                                if play_tx.try_send(PlaybackCommand::Audio { samples: chunk.to_vec(), generation }).is_err() {
                                    metrics.audio.dropped_frames.fetch_add(1, Ordering::Relaxed);
                                    bail!("Playback queue is full. The stream stopped to prevent accumulating delay; increase playback_queue_ms or check device/model speed");
                                }
                            }
                        }
                        // STS may emit transcripts for its own synthesis protocol. Only
                        // the independently selected STT stream owns the saved original.
                        ProviderEvent::Transcript { .. } | ProviderEvent::TurnComplete => {}
                    }
                }
                event = stt_events_rx.recv(), if transcribing => {
                    if cancel.is_cancelled() { break Ok(()); }
                    let Some(event) = event else {
                        metrics.report_processing_error("The recognizer closed the transcription channel");
                        transcribing = false; stt_connected = false; transcript_tx = None;
                        recognition_cancel.cancel();
                        if !translating { metrics.state("passthrough"); }
                        continue;
                    };
                    match &event {
                        ProviderEvent::Connected => {
                            stt_connected = true;
                            if !translating { metrics.state("transcribing"); }
                        }
                        ProviderEvent::Reconnecting { .. } => {
                            stt_connected = false;
                            stt_offset_ms = None;
                            metrics.reconnects.fetch_add(1, Ordering::Relaxed);
                            if !translating { metrics.state("reconnecting"); }
                        }
                        _ => {}
                    }
                    if let Err(error) = record_recognition_event_at(event, &transcript_tx, &metrics, stt_offset_ms.unwrap_or(0)) {
                        metrics.report_processing_error(&format!("Transcription interrupted: {error:#}"));
                        transcript_tx = None; transcribing = false; stt_connected = false;
                        recognition_cancel.cancel();
                        if !translating { metrics.state("passthrough"); }
                    }
                }
                frame = captured_rx.recv() => {
                    if cancel.is_cancelled() { break Ok(()); }
                    let Some(original) = frame else { bail!("Audio capture ended"); };
                    // This whole branch runs only on the processing executor. Native-rate,
                    // full-resolution original audio has already been forwarded independently.
                    let losses = metrics.audio.sidecar_dropped_frames.load(Ordering::Relaxed);
                    if losses > copy_losses && (audio_tx.is_some() || transcribing) {
                        metrics.report_processing_error("Audio processing overloaded; the recording or transcript may contain gaps");
                    }
                    copy_losses = losses;
                    if !translating && !transcribing && audio_tx.is_none() && !history.enabled() {
                        continue;
                    }
                    let frame = speech.convert(&original);
                    let input_level = rms(&frame.samples);
                    metrics.input_level.store(input_level.to_bits(), Ordering::Relaxed);
                    if !translating { metrics.output_level.store(input_level.to_bits(), Ordering::Relaxed); }
                    history.push(match origin { TranscriptOrigin::Microphone => RecordingLane::Microphone, TranscriptOrigin::Speaker => RecordingLane::Speaker }, &frame.samples, frame.captured_at);
                    if let Some(sender) = &audio_tx
                        && sender.try_send(AudioRecord { lane: match origin { TranscriptOrigin::Microphone => RecordingLane::Microphone, TranscriptOrigin::Speaker => RecordingLane::Speaker }, samples: frame.samples.clone(), captured_at: frame.captured_at }).is_err() {
                            metrics.report_processing_error("Audio recording interrupted: destination unavailable or slow; the file may be incomplete");
                            audio_tx = None;
                    }
                    if translating || transcribing {
                        if frame.captured_at.elapsed() > Duration::from_millis(u64::from(cfg.audio.max_capture_age_ms)) {
                            metrics.audio.processing_dropped_frames.fetch_add(1, Ordering::Relaxed);
                        } else {
                            if transcribing && stt_connected && stt_offset_ms.is_none() {
                                let start = frame.captured_at.checked_sub(Duration::from_secs_f64(frame.samples.len() as f64 / f64::from(INPUT_RATE))).unwrap_or(frame.captured_at);
                                stt_offset_ms = Some(start.saturating_duration_since(session_origin).as_millis().min(u128::from(u64::MAX)) as u64);
                            }
                            fanout_original_audio(frame.samples,
                                (translating && connected).then_some(&input_tx),
                                (transcribing && stt_connected).then_some(&stt_input_tx), &metrics);
                        }
                    }
                }
            }
        }
    }.await;
    cancel.cancel();
    interrupt(&metrics, &play_tx);
    drop(input_tx);
    drop(stt_input_tx);
    drop(play_tx);
    drop(audio_tx);
    // Device shutdown cannot wait for a provider or a disk writer. Background
    // work is cancelled separately; stalled processing never owns audio leases.
    recognition_cancel.cancel();
    processing_jobs.abort_all();
    let mut cleanup_result = Ok(());
    let audio_shutdown = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(completed) = audio_jobs.join_next().await {
            let completed = completed
                .context("Transport task interrupted")
                .and_then(|r| r);
            if completed.is_err() && cleanup_result.is_ok() {
                cleanup_result = completed;
            }
        }
    })
    .await;
    if audio_shutdown.is_err() {
        audio_jobs.abort_all();
        cleanup_result = Err(anyhow!("Timed out while stopping audio devices"));
    }
    // Save finals already delivered before cancellation. Never wait for a model
    // to finish another turn or replay a result into a later device activation.
    while let Ok(event) = stt_events_rx.try_recv() {
        if let Err(error) =
            record_recognition_event_at(event, &transcript_tx, &metrics, stt_offset_ms.unwrap_or(0))
        {
            metrics.report_processing_error(&format!(
                "Transcription incomplete at shutdown: {error:#}"
            ));
        }
    }
    drop(transcript_tx);
    metrics.input_level.store(0, Ordering::Relaxed);
    metrics.output_level.store(0, Ordering::Relaxed);
    metrics.state(if result.is_ok() && cleanup_result.is_ok() {
        "stopped"
    } else {
        "error"
    });
    result
        .and(cleanup_result)
        .with_context(|| format!("Stream {name}"))
}

#[derive(Clone, Copy)]
enum Processor {
    Translation,
    Recognition,
}

/// Dropping a provider's sender can wake the receiver before its task result is
/// available. Preserve that result instead of reporting EOF as the root cause.
/// This runs on the processing executor; device workers keep their own runtime.
async fn translation_result_after_eof(
    jobs: &mut JoinSet<(Processor, Result<()>)>,
    cancel: &CancellationToken,
    metrics: &RouteMetrics,
) -> Result<()> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Ok(()),
        result = tokio::time::timeout(Duration::from_secs(2), async {
            while let Some(completed) = jobs.join_next().await {
                match completed.context("Processing task ended unexpectedly")? {
                    (Processor::Translation, Err(error)) => return Err(error),
                    (Processor::Translation, Ok(())) => bail!("The translator ended unexpectedly"),
                    (Processor::Recognition, Err(error)) => {
                        metrics.report_processing_error(&format!("Transcription interrupted: {error:#}"));
                    }
                    (Processor::Recognition, Ok(())) => {}
                }
            }
            bail!("Provider closed the audio channel without a completion result")
        }) => result.context("Provider closed the audio channel but did not finish within 2 seconds")?,
    }
}

fn spawn_processor<F>(
    jobs: &mut JoinSet<(Processor, Result<()>)>,
    handle: &tokio::runtime::Handle,
    kind: Processor,
    future: F,
) where
    F: std::future::Future<Output = Result<()>> + Send + 'static,
{
    jobs.spawn_on(
        async move {
            // Preserve the processor kind even if an adapter panics. A failed STT
            // adapter must not tear down the independent original audio transport.
            let result = std::panic::AssertUnwindSafe(future)
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err(anyhow!("Processing component failed internally")));
            (kind, result)
        },
        handle,
    );
}

async fn forward_processing(
    mut captured: mpsc::Receiver<audio::OriginalFrame>,
    processing: mpsc::Sender<audio::OriginalFrame>,
    cancel: CancellationToken,
    stats: Arc<AudioStats>,
) -> Result<()> {
    loop {
        let frame = tokio::select! { biased; _ = cancel.cancelled() => return Ok(()), frame = captured.recv() => frame };
        let Some(frame) = frame else {
            return Err(anyhow!("Audio capture ended"));
        };
        if processing.try_send(frame).is_err() {
            stats
                .processing_dropped_frames
                .fetch_add(1, Ordering::Relaxed);
            stats.sidecar_dropped_frames.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn provider_eof_preserves_delayed_task_error_and_buffered_events() {
        let mut jobs = JoinSet::new();
        let (events, mut received) = mpsc::channel(2);
        let (finish, finished) = tokio::sync::oneshot::channel();
        spawn_processor(
            &mut jobs,
            &tokio::runtime::Handle::current(),
            Processor::Translation,
            async move {
                events.send(ProviderEvent::Connected).await?;
                events.send(ProviderEvent::TurnComplete).await?;
                drop(events);
                finished.await?;
                bail!("synthetic translator HTTP 503")
            },
        );
        assert_eq!(received.recv().await, Some(ProviderEvent::Connected));
        assert_eq!(received.recv().await, Some(ProviderEvent::TurnComplete));
        assert_eq!(received.recv().await, None);
        let cancel = CancellationToken::new();
        let metrics = RouteMetrics::default();
        let completion = translation_result_after_eof(&mut jobs, &cancel, &metrics);
        tokio::pin!(completion);
        assert!(completion.as_mut().now_or_never().is_none());
        finish.send(()).unwrap();
        let error = completion.await.unwrap_err();
        assert!(error.to_string().contains("synthetic translator HTTP 503"));
    }

    #[tokio::test(start_paused = true)]
    async fn provider_eof_wait_is_bounded_and_cancellable() {
        for stopping in [false, true] {
            let mut jobs = JoinSet::new();
            spawn_processor(
                &mut jobs,
                &tokio::runtime::Handle::current(),
                Processor::Translation,
                std::future::pending(),
            );
            let cancel = CancellationToken::new();
            let metrics = RouteMetrics::default();
            let started = tokio::time::Instant::now();
            let completion = translation_result_after_eof(&mut jobs, &cancel, &metrics);
            tokio::pin!(completion);
            assert!(completion.as_mut().now_or_never().is_none());
            if stopping {
                cancel.cancel();
                completion.await.unwrap();
                assert_eq!(started.elapsed(), Duration::ZERO);
            } else {
                let error = completion.await.unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("did not finish within 2 seconds")
                );
                assert_eq!(started.elapsed(), Duration::from_secs(2));
            }
        }
    }

    #[tokio::test]
    async fn panicking_recognizer_remains_a_processing_failure() {
        let mut jobs = JoinSet::new();
        spawn_processor(
            &mut jobs,
            &tokio::runtime::Handle::current(),
            Processor::Recognition,
            async {
                panic!("fault injected in speech adapter");
                #[allow(unreachable_code)]
                Ok(())
            },
        );
        let (kind, result) = jobs
            .join_next()
            .await
            .unwrap()
            .expect("panic is contained in the adapter");
        assert!(matches!(kind, Processor::Recognition));
        assert!(result.unwrap_err().to_string().contains("Processing"));
    }
}
