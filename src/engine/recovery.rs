//! Session recovery reuses retained originals without opening capture devices.
//! Failed translation may replay to selected outputs; completed directions are
//! skipped. Originals remain until playback and every writer's sync finish.
use super::*;

#[derive(Clone)]
pub(super) struct Archive {
    session: crate::session::SessionIdentity,
    config: AppConfig,
    origin: Instant,
    originals: Arc<retained::RetainedSession>,
    // Share the user's existing optional pre-session buffer. Copying up to an
    // hour into a second live retention queue would exceed its memory budget.
    history: crate::history::HistorySnapshot,
    error: Arc<StdMutex<Option<String>>>,
    recovering: Arc<AtomicBool>,
    pub(super) outputs_committed: Arc<AtomicBool>,
    pub(super) translations_committed: [Arc<AtomicBool>; 2],
}

#[derive(Debug, Serialize)]
pub struct RetainedSessionStatus {
    pub id: String,
    pub name: String,
    pub frames: u64,
    pub memory_bytes: usize,
    pub encrypted_bytes: u64,
    pub error: Option<String>,
    pub recovering: bool,
}

impl Archive {
    pub(super) fn new(
        session: crate::session::SessionIdentity,
        config: AppConfig,
        origin: Instant,
        originals: Arc<retained::RetainedSession>,
        history: crate::history::HistorySnapshot,
    ) -> Self {
        debug_assert_eq!(origin, originals.origin());
        Self {
            session,
            config,
            origin,
            originals,
            history,
            error: Arc::default(),
            recovering: Arc::default(),
            outputs_committed: Arc::default(),
            translations_committed: std::array::from_fn(|_| Arc::default()),
        }
    }

    fn status(&self) -> RetainedSessionStatus {
        let status = self.originals.status();
        RetainedSessionStatus {
            id: self.session.id.clone(),
            name: self.session.name.clone(),
            frames: status.frames + self.history.frames.len() as u64,
            memory_bytes: status.memory_bytes
                + self
                    .history
                    .frames
                    .iter()
                    .map(|frame| frame.samples().len() * 2)
                    .sum::<usize>(),
            encrypted_bytes: status.encrypted_bytes,
            error: self
                .error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .or(status.error),
            recovering: self.recovering.load(Ordering::Acquire),
        }
    }

    fn fail(&self, error: String) {
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
    }
}

/// Called only after the route/worker ownership has ended. Dropping or timing
/// out a provider must never drop the only owner of its retained originals.
pub(super) async fn finish_archive(
    state: &mut State,
    archive: Option<Archive>,
    status: &mut EngineStatus,
) {
    let Some(archive) = archive else {
        return;
    };
    archive.originals.close_capture();
    let retained = archive.originals.status();
    // Retention is released only after every selected consumer has committed,
    // including translated playback, not merely after the files close.
    let error = retained.error.or_else(|| {
        (!archive.outputs_committed.load(Ordering::Acquire)).then(|| {
            status
                .last_error
                .clone()
                .unwrap_or_else(|| "Session processing did not finish".into())
        })
    });
    if (error.is_none() || retained.frames == 0 && archive.history.frames.is_empty())
        && !retained.missing_audio
    {
        release_completed(archive);
        return;
    } else {
        archive.fail(error.unwrap_or_else(|| "Original capture was incomplete".into()));
    }
    let message =
        "Original audio is retained for recovery. Keep Babel open until recovery finishes.";
    status.last_error = Some(match status.last_error.take() {
        Some(error) => format!("{error}. {message}"),
        None => message.into(),
    });
    state.retained.push(archive);
}

impl Controller {
    pub async fn retained_sessions(&self) -> Vec<RetainedSessionStatus> {
        let mut state = self.state.lock().await;
        reap(&mut state).await;
        state.retained.iter().map(Archive::status).collect()
    }

    /// A disconnected browser does not cancel recovery or release its key.
    /// Failed retries remain in Controller ownership until the app exits.
    pub async fn recover_retained_session(&self, id: &str) -> Result<()> {
        let processing = crate::execution::processing_handle()?;
        let (archive, usage) = {
            let mut state = self.state.lock().await;
            let archive = state
                .retained
                .iter()
                .find(|archive| archive.session.id == id)
                .context(
                    "Retained session not found; recovery is available only while Babel stays open",
                )?
                .clone();
            let translation_pending = [
                archive.config.microphone.enabled && !archive.config.microphone_uses_speaker(),
                archive.config.speaker.enabled,
            ]
            .into_iter()
            .enumerate()
            .any(|(index, enabled)| {
                enabled && !archive.translations_committed[index].load(Ordering::Acquire)
            });
            if translation_pending {
                ensure!(
                    archive.config.microphone.playback_device
                        == state.config.microphone.playback_device
                        && archive.config.speaker.capture_device
                            == state.config.speaker.capture_device,
                    "Select this session's virtual endpoints before recovering its translation"
                );
            }
            let usage = if translation_pending {
                endpoint_usage(&mut state)
            } else {
                // File-only recovery opens neither devices nor OS observers.
                watch::channel(audio::activity::EndpointUse::default()).1
            };
            ensure!(
                archive
                    .recovering
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok(),
                "This session is already being recovered"
            );
            (archive, usage)
        };
        let runtime = self.local_runtime.clone();
        let state = self.state.clone();
        processing
            .spawn(async move {
                struct Guard(Arc<AtomicBool>);
                impl Drop for Guard {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _guard = Guard(archive.recovering.clone());
                let work = async {
                    let config = archive.config.clone();
                    let (config, _lease) =
                        runtime.resolve(&config, CancellationToken::new()).await?;
                    let recovered = crate::session::SessionIdentity::new_with_language(
                        Some(&archive.session.name),
                        "en",
                    )?;
                    let files = create_session_files(&config, &recovered, archive.origin).await?;
                    replay(&archive, &config, files, usage).await?;
                    ensure!(!archive.originals.status().missing_audio,
                        "Available originals were saved to new files, but an upstream capture gap cannot be reconstructed; retained audio remains available");
                    state
                        .lock()
                        .await
                        .retained
                        .retain(|pending| pending.session.id != archive.session.id);
                    release_completed(archive.clone());
                    Ok::<_, anyhow::Error>(())
                };
                let result = work.await;
                if let Err(error) = &result {
                    archive.fail(format!("Recovery failed: {error:#}"));
                }
                result
            })
            .await
            .context("Session recovery worker ended unexpectedly")?
    }
}

fn release_completed(archive: Archive) {
    // Final outputs are synced. Cleanup may wait on filesystem operations; it
    // must never hold the Controller state lock or delay routing/status/Stop.
    let handle =
        crate::execution::processing_handle().unwrap_or_else(|_| tokio::runtime::Handle::current());
    handle.spawn(async move {
        if archive.originals.complete().await.is_err() {
            tracing::warn!("Session files saved; temporary retention cleanup failed");
        }
        // Last-owner drop erases the ephemeral key and removes its directory,
        // even if an individual explicit record acknowledgment failed.
        drop(archive);
    });
}

async fn replay(
    archive: &Archive,
    config: &AppConfig,
    files: SessionFiles,
    usage: watch::Receiver<audio::activity::EndpointUse>,
) -> Result<()> {
    let mut originals = archive.originals.snapshot()?;
    let mut workers = JoinSet::new();
    let cancellation = CancellationToken::new();
    let _cancel = cancellation.clone().drop_guard();
    for (index, origin, route) in [
        (0, TranscriptOrigin::Microphone, &config.microphone),
        (1, TranscriptOrigin::Speaker, &config.speaker),
    ] {
        if !route.enabled
            || (index == 0 && config.microphone_uses_speaker())
            || archive.translations_committed[index].load(Ordering::Acquire)
        {
            continue;
        }
        let config = config.clone();
        let route = route.clone();
        let originals = archive.originals.clone();
        let committed = archive.translations_committed[index].clone();
        let usage = usage.clone();
        workers.spawn(async move {
            let (_devices, playback) = watch::channel(route.playback_device.clone());
            translation::run(
                config,
                route,
                origin,
                originals,
                Arc::default(),
                playback,
                None,
                usage,
            )
            .await?;
            committed.store(true, Ordering::Release);
            Ok(())
        });
    }
    let transcript = if let Some(writer) = files.transcript {
        let (sender, received) = mpsc::channel(128);
        workers.spawn(writer.run(received));
        sender.send(TranscriptRecord::Section(format!(
            "Recovered originals from session {}. New files preserve the earlier partial files.", archive.session.id
        ))).await.context("Recovered transcript closed")?;
        Some(sender)
    } else {
        None
    };
    let audio = if let Some(writer) = files.audio {
        let (sender, received) = mpsc::channel(128);
        workers.spawn(writer.run(received));
        Some(sender)
    } else {
        None
    };
    let mut inputs = [None, None];
    for (index, origin, enabled, recognition) in [
        (
            0,
            TranscriptOrigin::Microphone,
            config.transcription.microphone && !config.microphone_uses_speaker(),
            &config.transcription.microphone_recognition,
        ),
        (
            1,
            TranscriptOrigin::Speaker,
            config.transcription.speaker,
            &config.transcription.speaker_recognition,
        ),
    ] {
        if enabled && let Some(transcript) = transcript.clone() {
            let provider = provider::stt::create(recognition, &config.transcription.providers)?;
            let settings =
                provider::stt::session_config(recognition, &config.transcription.providers)?;
            let (sender, received) = mpsc::channel(8);
            inputs[index] = Some(sender);
            workers.spawn(transcribe(
                provider,
                settings,
                received,
                transcript,
                origin,
                cancellation.child_token(),
            ));
        }
    }
    drop(transcript);
    let mut next = [0u64; 2];
    let mut pending = Box::pin(async {
        let mut history = archive.history.frames.iter();
        loop {
            let frame = match history.next() {
                Some(frame) => AudioRecord {
                    lane: frame.lane,
                    samples: frame.samples().to_vec(),
                    captured_at: frame.captured_at,
                },
                None => match originals.next().await? {
                    Some(frame) => frame,
                    None => break,
                },
            };
            let lane = if frame.lane == RecordingLane::Microphone {
                0
            } else {
                1
            };
            if let Some(sender) = &inputs[lane] {
                feed(sender, &frame, archive.origin, &mut next[lane]).await?;
            }
            let record_lane = if lane == 0 {
                config.recording.microphone && !config.microphone_uses_speaker()
            } else {
                config.recording.speaker
            };
            if record_lane && let Some(sender) = &audio {
                sender
                    .send(frame)
                    .await
                    .context("Recovered audio writer closed")?;
            }
        }
        drop(inputs);
        drop(audio);
        Ok::<_, anyhow::Error>(())
    });
    // Observe failed writers/providers while a slow consumer backpressures
    // replay. No live routing task can ever await these sends.
    loop {
        tokio::select! {
            result = &mut pending => { result?; break; }
            result = workers.join_next(), if !workers.is_empty() => {
                result.context("Recovery workers disappeared")?.context("Recovery worker interrupted")??;
            }
        }
    }
    drop(pending);
    while let Some(result) = workers.join_next().await {
        result.context("Recovery worker interrupted")??;
    }
    Ok(())
}

async fn feed(
    sender: &mpsc::Sender<Vec<i16>>,
    frame: &AudioRecord,
    origin: Instant,
    next: &mut u64,
) -> Result<()> {
    let end = (frame
        .captured_at
        .saturating_duration_since(origin)
        .as_nanos()
        * 16_000
        / 1_000_000_000) as u64;
    let candidate = end.saturating_sub(frame.samples.len() as u64);
    let start = if *next > 0 && candidate <= next.saturating_add(800) {
        *next
    } else {
        candidate
    };
    while *next < start {
        let count = (start - *next).min(16_000) as usize;
        sender
            .send(vec![0; count])
            .await
            .context("Recovery recognizer closed")?;
        *next += count as u64;
    }
    for chunk in frame.samples.chunks(16_000) {
        sender
            .send(chunk.to_vec())
            .await
            .context("Recovery recognizer closed")?;
        *next += chunk.len() as u64;
    }
    Ok(())
}

async fn transcribe(
    provider: Arc<dyn provider::SpeechProvider>,
    settings: SessionConfig,
    mut received: mpsc::Receiver<Vec<i16>>,
    transcript: mpsc::Sender<TranscriptRecord>,
    origin: TranscriptOrigin,
    cancel: CancellationToken,
) -> Result<()> {
    let mut position = 0u64;
    let mut ended = false;
    while !ended {
        // Provider historical endpoints have finite request limits. Replay in
        // small windows instead of building a whole long session in model RAM.
        let mut pcm = zeroize::Zeroizing::new(Vec::with_capacity(31 * 16_000));
        while pcm.len() < 30 * 16_000 {
            match received.recv().await {
                Some(frame) => pcm.extend_from_slice(&frame),
                None => {
                    ended = true;
                    break;
                }
            }
        }
        let offset = position / 16;
        position += pcm.len() as u64;
        if pcm.iter().all(|sample| *sample == 0) {
            continue;
        }
        transcribe_window(
            &provider,
            settings.clone(),
            &pcm,
            &transcript,
            origin,
            offset,
            cancel.child_token(),
        )
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn transcribe_window(
    provider: &Arc<dyn provider::SpeechProvider>,
    settings: SessionConfig,
    pcm: &[i16],
    transcript: &mpsc::Sender<TranscriptRecord>,
    origin: TranscriptOrigin,
    offset: u64,
    cancel: CancellationToken,
) -> Result<()> {
    let (audio, received) = mpsc::channel(8);
    let (sender, mut events) = mpsc::channel(32);
    let model = provider.run_history(settings, received, sender, cancel);
    let feed = async {
        for chunk in pcm.chunks(16_000) {
            audio
                .send(chunk.to_vec())
                .await
                .context("Recovery recognizer closed")?;
        }
        drop(audio);
        Ok::<_, anyhow::Error>(())
    };
    let collect = async {
        while let Some(event) = events.recv().await {
            let record = match event {
                ProviderEvent::Transcript {
                    input: true,
                    text,
                    mut metadata,
                } => {
                    metadata.start_ms = metadata.start_ms.map(|ms| ms.saturating_add(offset));
                    metadata.end_ms = metadata.end_ms.map(|ms| ms.saturating_add(offset));
                    metadata.alignment_ms =
                        metadata.alignment_ms.map(|ms| ms.saturating_add(offset));
                    TranscriptRecord::Text {
                        input: true,
                        text,
                        metadata,
                        received_at: chrono::Utc::now().to_rfc3339(),
                    }
                }
                ProviderEvent::TurnComplete => TranscriptRecord::TurnComplete,
                ProviderEvent::Interrupted
                | ProviderEvent::Reconnecting { .. }
                | ProviderEvent::Warning { .. } => {
                    bail!(
                        "Original transcription did not complete; retained audio is still available"
                    )
                }
                _ => continue,
            };
            transcript
                .send(TranscriptRecord::Routed {
                    origin,
                    record: Box::new(record),
                })
                .await
                .context("Recovered transcript closed")?;
        }
        Ok(())
    };
    tokio::try_join!(feed, model, collect)?;
    Ok(())
}

#[cfg(test)]
mod tests;
