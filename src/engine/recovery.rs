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
    pub(super) progress: Arc<SessionProgress>,
}

/// Each model and file confirms its own work. A failure in one feature must
/// not replay another feature's completed inference or rewrite synced files.
#[derive(Default)]
pub(super) struct SessionProgress {
    pub(super) recording: Arc<AtomicBool>,
    pub(super) transcript: Arc<AtomicBool>,
    pub(super) recognition: [Arc<AtomicBool>; 2],
    pub(super) translation: [Arc<AtomicBool>; 2],
}
impl SessionProgress {
    fn pending_config(&self, config: &AppConfig) -> AppConfig {
        let mut pending = config.clone();
        pending.microphone.enabled &= !self.translation[0].load(Ordering::Acquire);
        pending.speaker.enabled &= !self.translation[1].load(Ordering::Acquire);
        pending.recording.enabled &= !self.recording.load(Ordering::Acquire);
        let saved = self.transcript.load(Ordering::Acquire);
        pending.transcription.microphone &= !config.microphone_uses_speaker()
            && (!saved || !self.recognition[0].load(Ordering::Acquire));
        pending.transcription.speaker &= !saved || !self.recognition[1].load(Ordering::Acquire);
        pending.transcription.enabled &=
            pending.transcription.microphone || pending.transcription.speaker;
        pending
    }
    fn complete(&self, config: &AppConfig) -> bool {
        let pending = self.pending_config(config);
        !pending.recording.enabled
            && !pending.transcription.enabled
            && (!config.microphone.enabled
                || config.microphone_uses_speaker()
                || self.translation[0].load(Ordering::Acquire))
            && (!config.speaker.enabled || self.translation[1].load(Ordering::Acquire))
    }
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
            progress: Arc::default(),
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
                    .map(|frame| frame.memory_bytes())
                    .sum::<usize>(),
            encrypted_bytes: status.encrypted_bytes
                + self
                    .history
                    .frames
                    .iter()
                    .filter(|frame| frame.memory_bytes() == 0)
                    .map(|frame| (frame.sample_count() * 2) as u64)
                    .sum::<u64>(),
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
        (!archive.progress.complete(&archive.config)).then(|| {
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
                enabled && !archive.progress.translation[index].load(Ordering::Acquire)
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
                    let config = archive.progress.pending_config(&archive.config);
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
    let mut originals = files
        .audio
        .as_ref()
        .map(|_| archive.originals.snapshot())
        .transpose()?;
    let mut workers = JoinSet::new();
    let cancellation = CancellationToken::new();
    let _cancel = cancellation.clone().drop_guard();
    for (index, origin, route) in [
        (0, TranscriptOrigin::Microphone, &config.microphone),
        (1, TranscriptOrigin::Speaker, &config.speaker),
    ] {
        if !route.enabled
            || (index == 0 && config.microphone_uses_speaker())
            || archive.progress.translation[index].load(Ordering::Acquire)
        {
            continue;
        }
        let config = config.clone();
        let route = route.clone();
        let originals = archive.originals.clone();
        let committed = archive.progress.translation[index].clone();
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
        workers.spawn(commit_writer(
            history::write_transcript(
                writer,
                received,
                config.clone(),
                archive.history.clone(),
                Arc::new(AtomicBool::new(true)),
                Some(archive.originals.transcripts.clone()),
            ),
            Some(archive.progress.transcript.clone()),
        ));
        sender.send(TranscriptRecord::Section(format!(
            "Recovered originals from session {}. New files preserve the earlier partial files.", archive.session.id
        ))).await.context("Recovered transcript closed")?;
        Some(sender)
    } else {
        None
    };
    let audio = if let Some(writer) = files.audio {
        let (sender, received) = mpsc::channel(128);
        workers.spawn(commit_writer(
            writer.run(received),
            Some(archive.progress.recording.clone()),
        ));
        Some(sender)
    } else {
        None
    };
    for (index, origin, enabled) in [
        (
            0,
            TranscriptOrigin::Microphone,
            config.transcription.microphone && !config.microphone_uses_speaker(),
        ),
        (1, TranscriptOrigin::Speaker, config.transcription.speaker),
    ] {
        if enabled && let Some(sender) = transcript.clone() {
            workers.spawn(commit_writer(
                recognition::start_retained(
                    config.clone(),
                    TranscriptSink {
                        sender,
                        origin,
                        retained: Some(archive.originals.clone()),
                    },
                    Arc::default(),
                    archive.origin,
                    archive.originals.clone(),
                ),
                Some(archive.progress.recognition[index].clone()),
            ));
        }
    }
    drop(transcript);
    let mut pending = Box::pin(async {
        let mut history = archive.history.frames.iter();
        let mut history_reader = crate::history::HistoryReader::default();
        while let Some(originals) = &mut originals {
            let frame = match history.next() {
                Some(frame) => AudioRecord {
                    lane: frame.lane,
                    samples: frame.read_samples(&mut history_reader).await?.to_vec(),
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

pub(super) async fn transcribe(
    provider: Arc<dyn provider::SpeechProvider>,
    settings: SessionConfig,
    mut received: mpsc::Receiver<Vec<i16>>,
    transcript: mpsc::Sender<TranscriptRecord>,
    origin: TranscriptOrigin,
    cancel: CancellationToken,
    cache: Option<Arc<transcription_cache::Cache>>,
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
        if provider.is_silent_window(&pcm) {
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
            cache.as_ref(),
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
    cache: Option<&Arc<transcription_cache::Cache>>,
) -> Result<()> {
    let namespace = transcription_cache::Namespace::History(origin);
    let end = offset.saturating_add((pcm.len() as u64).div_ceil(16));
    if let Some(cache) = cache
        && let Some((cached_end, records)) = cache.get(namespace, offset).await?
    {
        ensure!(cached_end == end, "Confirmed history range changed");
        for record in records {
            transcript
                .send(record)
                .await
                .context("Recovered transcript closed")?;
        }
        return Ok(());
    }
    let mut attempts = 0u32;
    let retries = settings.max_reconnect_attempts;
    let mut settings = settings;
    settings.max_reconnect_attempts = 0;
    let results = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => bail!("Original transcription stopped before completion"),
            result = resilience::segment(provider.clone(), settings.clone(), pcm, true) => {
                match result {
                    Ok(results) => break results,
                    Err(error) => {
                        attempts = attempts.saturating_add(1);
                        resilience::retry(&error, attempts, retries)?;
                        tokio::select! {
                            _ = cancel.cancelled() => bail!("Original transcription stopped before completion"),
                            _ = tokio::time::sleep(resilience::delay(attempts)) => {},
                        }
                    }
                }
            }
        }
    };
    let mut records = Vec::new();
    for event in results {
        let record = match event {
            ProviderEvent::Transcript {
                input: true,
                text,
                mut metadata,
            } => {
                metadata.start_ms = metadata.start_ms.map(|ms| ms.saturating_add(offset));
                metadata.end_ms = metadata.end_ms.map(|ms| ms.saturating_add(offset));
                metadata.alignment_ms = metadata.alignment_ms.map(|ms| ms.saturating_add(offset));
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
                bail!("Original transcription did not complete; retained audio is still available")
            }
            _ => continue,
        };
        records.push(TranscriptRecord::Routed {
            origin,
            record: Box::new(record),
        });
    }
    if let Some(cache) = cache {
        cache.put(namespace, offset, end, &records).await?;
    }
    for record in records {
        transcript
            .send(record)
            .await
            .context("Recovered transcript closed")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
